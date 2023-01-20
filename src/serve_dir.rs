use std::error::Error;
use std::future::{Future, Ready};
use std::{
    convert::Infallible,
    io,
    path::Path,
    task::{Context, Poll},
};

use bytes::Bytes;
use futures_util::TryFutureExt;
use http::header::ALLOW;
use http::{header, HeaderValue, Method, Request, Response, StatusCode};
use http_body::{combinators::UnsyncBoxBody, Body, Empty, Full};
use percent_encoding::percent_decode;
use tokio::io::AsyncRead;
use tower_http::set_status::SetStatus;
use tower_http::BoxError;
use tower_service::Service;

pub use crate::async_body::AsyncReadBody;
use crate::content_encoding::{encodings, SupportedEncodings};
use crate::fs::Filesystem;
use crate::open_file;
use crate::open_file::{FileOpened, FileRequestExtent, OpenFileOutput};

// default capacity 64KiB
const DEFAULT_CAPACITY: usize = 65536;

pub(crate) type ResponseBody = UnsyncBoxBody<Bytes, io::Error>;

/// Service that serves files from a given directory and all its sub directories.
///
/// The `Content-Type` will be guessed from the file extension.
///
/// An empty response with status `404 Not Found` will be returned if:
///
/// - The file doesn't exist
/// - Any segment of the path contains `..`
/// - Any segment of the path contains a backslash
/// - We don't have necessary permissions to read the file
///
/// # Example
///
/// ```
/// use http_dir::ServeDir;
/// use http_dir::fs::disk::DiskFilesystem;
///
/// // This will serve files in the "assets" directory and
/// // its subdirectories
/// let service = ServeDir::new(DiskFilesystem::from("assets"));
///
/// # async {
/// // Run our service using `hyper`
/// let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 3000));
/// hyper::Server::bind(&addr)
///     .serve(tower::make::Shared::new(service))
///     .await
///     .expect("server error");
/// # };
/// ```
#[derive(Debug, Clone)]
pub struct ServeDir<FS, F = DefaultServeDirFallback> {
    pub(crate) buf_chunk_size: usize,
    pub(crate) precompressed_variants: Option<PrecompressedVariants>,
    // This is used to specialise implementation for single files
    variant: ServeVariant,
    fallback: Option<F>,
    call_fallback_on_method_not_allowed: bool,
    filesystem: FS,
}

impl<FS> ServeDir<FS, DefaultServeDirFallback> {
    /// Create a new [`ServeDir`].
    pub fn new(filesystem: FS) -> Self {
        Self {
            buf_chunk_size: DEFAULT_CAPACITY,
            precompressed_variants: None,
            variant: ServeVariant::Directory {
                append_index_html_on_directories: true,
            },
            fallback: None,
            call_fallback_on_method_not_allowed: false,
            filesystem,
        }
    }

    pub(crate) fn new_single_file(filesystem: FS, mime: HeaderValue) -> Self {
        Self {
            buf_chunk_size: DEFAULT_CAPACITY,
            precompressed_variants: None,
            variant: ServeVariant::SingleFile { mime },
            fallback: None,
            call_fallback_on_method_not_allowed: false,
            filesystem,
        }
    }
}

impl<FS, F> ServeDir<FS, F> {
    /// If the requested path is a directory append `index.html`.
    ///
    /// This is useful for static sites.
    ///
    /// Defaults to `true`.
    pub fn append_index_html_on_directories(mut self, append: bool) -> Self {
        match &mut self.variant {
            ServeVariant::Directory {
                append_index_html_on_directories,
            } => {
                *append_index_html_on_directories = append;
            }
            ServeVariant::SingleFile { .. } => {}
        }

        self
    }

    /// Set a specific read buffer chunk size.
    ///
    /// The default capacity is 64kb.
    pub fn with_buf_chunk_size(mut self, chunk_size: usize) -> Self {
        self.buf_chunk_size = chunk_size;
        self
    }

    /// Informs the service that it should also look for a precompressed gzip
    /// version of _any_ file in the directory.
    ///
    /// Assuming the `dir` directory is being served and `dir/foo.txt` is requested,
    /// a client with an `Accept-Encoding` header that allows the gzip encoding
    /// will receive the file `dir/foo.txt.gz` instead of `dir/foo.txt`.
    /// If the precompressed file is not available, or the client doesn't support it,
    /// the uncompressed version will be served instead.
    /// Both the precompressed version and the uncompressed version are expected
    /// to be present in the directory. Different precompressed variants can be combined.
    pub fn precompressed_gzip(mut self) -> Self {
        self.precompressed_variants
            .get_or_insert(Default::default())
            .gzip = true;
        self
    }

    /// Informs the service that it should also look for a precompressed brotli
    /// version of _any_ file in the directory.
    ///
    /// Assuming the `dir` directory is being served and `dir/foo.txt` is requested,
    /// a client with an `Accept-Encoding` header that allows the brotli encoding
    /// will receive the file `dir/foo.txt.br` instead of `dir/foo.txt`.
    /// If the precompressed file is not available, or the client doesn't support it,
    /// the uncompressed version will be served instead.
    /// Both the precompressed version and the uncompressed version are expected
    /// to be present in the directory. Different precompressed variants can be combined.
    pub fn precompressed_br(mut self) -> Self {
        self.precompressed_variants
            .get_or_insert(Default::default())
            .br = true;
        self
    }

    /// Informs the service that it should also look for a precompressed deflate
    /// version of _any_ file in the directory.
    ///
    /// Assuming the `dir` directory is being served and `dir/foo.txt` is requested,
    /// a client with an `Accept-Encoding` header that allows the deflate encoding
    /// will receive the file `dir/foo.txt.zz` instead of `dir/foo.txt`.
    /// If the precompressed file is not available, or the client doesn't support it,
    /// the uncompressed version will be served instead.
    /// Both the precompressed version and the uncompressed version are expected
    /// to be present in the directory. Different precompressed variants can be combined.
    pub fn precompressed_deflate(mut self) -> Self {
        self.precompressed_variants
            .get_or_insert(Default::default())
            .deflate = true;
        self
    }

    /// Set the fallback service.
    ///
    /// This service will be called if there is no file at the path of the request.
    ///
    /// The status code returned by the fallback will not be altered. Use
    /// [`ServeDir::not_found_service`] to set a fallback and always respond with `404 Not Found`.
    ///
    /// # Example
    ///
    /// This can be used to respond with a different file:
    ///
    /// ```rust
    /// use http_dir::ServeDir;
    /// use http_dir::fs::disk::DiskFilesystem;
    /// use tower_http::services::ServeFile;
    ///
    /// let service = ServeDir::new(DiskFilesystem::from("assets"))
    ///     // respond with `not_found.html` for missing files
    ///     .fallback(ServeFile::new("assets/not_found.html"));
    ///
    /// # async {
    /// // Run our service using `hyper`
    /// let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 3000));
    /// hyper::Server::bind(&addr)
    ///     .serve(tower::make::Shared::new(service))
    ///     .await
    ///     .expect("server error");
    /// # };
    /// ```
    pub fn fallback<F2>(self, new_fallback: F2) -> ServeDir<FS, F2> {
        ServeDir {
            buf_chunk_size: self.buf_chunk_size,
            precompressed_variants: self.precompressed_variants,
            variant: self.variant,
            fallback: Some(new_fallback),
            call_fallback_on_method_not_allowed: self.call_fallback_on_method_not_allowed,
            filesystem: self.filesystem,
        }
    }

    /// Set the fallback service and override the fallback's status code to `404 Not Found`.
    ///
    /// This service will be called if there is no file at the path of the request.
    ///
    /// # Example
    ///
    /// This can be used to respond with a different file:
    ///
    /// ```rust
    /// use http_dir::ServeDir;
    /// use http_dir::fs::disk::DiskFilesystem;
    /// use tower_http::services::ServeFile;
    ///
    /// let service = ServeDir::new(DiskFilesystem::from("assets"))
    ///     // respond with `404 Not Found` and the contents of `not_found.html` for missing files
    ///     .not_found_service(ServeFile::new("assets/not_found.html"));
    ///
    /// # async {
    /// // Run our service using `hyper`
    /// let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 3000));
    /// hyper::Server::bind(&addr)
    ///     .serve(tower::make::Shared::new(service))
    ///     .await
    ///     .expect("server error");
    /// # };
    /// ```
    ///
    /// Setups like this are often found in single page applications.
    pub fn not_found_service<F2>(self, new_fallback: F2) -> ServeDir<FS, SetStatus<F2>> {
        self.fallback(SetStatus::new(new_fallback, StatusCode::NOT_FOUND))
    }

    /// Customize whether or not to call the fallback for requests that aren't `GET` or `HEAD`.
    ///
    /// Defaults to not calling the fallback and instead returning `405 Method Not Allowed`.
    pub fn call_fallback_on_method_not_allowed(mut self, call_fallback: bool) -> Self {
        self.call_fallback_on_method_not_allowed = call_fallback;
        self
    }
}

impl<ReqBody, F, FResBody, FS> Service<Request<ReqBody>> for ServeDir<FS, F>
where
    F: Service<Request<ReqBody>, Response = Response<FResBody>> + Clone,
    F::Error: Into<io::Error>,
    F::Future: Send,
    FResBody: Body<Data = Bytes> + Send + 'static,
    FResBody::Error: Into<Box<dyn Error + Send + Sync>>,
    FS: Filesystem + Clone + Send + Sync,
    FS::File: 'static,
{
    type Response = Response<ResponseBody>;
    type Error = io::Error;
    type Future = impl Future<Output = Result<Self::Response, Self::Error>>;

    #[inline]
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if let Some(fallback) = &mut self.fallback {
            fallback.poll_ready(cx).map_err(Into::into)
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        let mut this = self.clone();

        async move {
            if req.method() != Method::GET && req.method() != Method::HEAD {
                if this.call_fallback_on_method_not_allowed {
                    if let Some(fallback) = &mut this.fallback {
                        return fallback
                            .call(req)
                            .err_into()
                            .map_ok(|response| {
                                response
                                    .map(|body| {
                                        body.map_err(|err| {
                                            match err.into().downcast::<io::Error>() {
                                                Ok(err) => *err,
                                                Err(err) => {
                                                    io::Error::new(io::ErrorKind::Other, err)
                                                }
                                            }
                                        })
                                        .boxed_unsync()
                                    })
                                    .map(ResponseBody::new)
                            })
                            .await;
                    }
                } else {
                    let mut res = response_with_status(StatusCode::METHOD_NOT_ALLOWED);
                    res.headers_mut()
                        .insert(ALLOW, HeaderValue::from_static("GET,HEAD"));

                    return Ok(res);
                }
            }

            // `ServeDir` doesn't care about the request body but the fallback might. So move out the
            // body and pass it to the fallback, leaving an empty body in its place
            //
            // this is necessary because we cannot clone bodies
            let (mut parts, body) = req.into_parts();
            // same goes for extensions
            let extensions = std::mem::take(&mut parts.extensions);
            let req = Request::from_parts(parts, Empty::<Bytes>::new());

            let mut fallback_and_request = this.fallback.as_mut().map(|fallback| {
                let mut fallback_req = Request::new(body);
                *fallback_req.method_mut() = req.method().clone();
                *fallback_req.uri_mut() = req.uri().clone();
                *fallback_req.headers_mut() = req.headers().clone();
                *fallback_req.extensions_mut() = extensions;

                // get the ready fallback and leave a non-ready clone in its place
                let clone = fallback.clone();
                let fallback = std::mem::replace(fallback, clone);

                (fallback, fallback_req)
            });

            let path_decoded =
                match percent_decode(req.uri().path().trim_start_matches('/').as_ref())
                    .decode_utf8()
                    .ok()
                {
                    None => {
                        return if let Some((mut fallback, request)) = fallback_and_request.take() {
                            call_fallback(&mut fallback, request).await
                        } else {
                            Ok(not_found())
                        }
                    }

                    Some(path) => path,
                };
            let path_to_file = Path::new(&*path_decoded).to_path_buf();

            let buf_chunk_size = this.buf_chunk_size;
            let range_header = req
                .headers()
                .get(header::RANGE)
                .and_then(|value| value.to_str().ok())
                .map(|s| s.to_owned());

            let negotiated_encodings = encodings(
                req.headers(),
                this.precompressed_variants.unwrap_or_default(),
            );

            match open_file::open_file(
                &mut this.filesystem,
                &this.variant,
                path_to_file,
                req,
                negotiated_encodings,
                range_header,
                buf_chunk_size,
            )
            .await
            {
                Ok(OpenFileOutput::FileOpened(file_output)) => Ok(build_response(*file_output)),

                Ok(OpenFileOutput::Redirect { location }) => {
                    let mut res = response_with_status(StatusCode::TEMPORARY_REDIRECT);
                    res.headers_mut().insert(header::LOCATION, location);

                    Ok(res)
                }

                Ok(OpenFileOutput::FileNotFound) => {
                    if let Some((mut fallback, request)) = fallback_and_request.take() {
                        call_fallback(&mut fallback, request).await
                    } else {
                        Ok(not_found())
                    }
                }

                Ok(OpenFileOutput::PreconditionFailed) => {
                    Ok(response_with_status(StatusCode::PRECONDITION_FAILED))
                }

                Ok(OpenFileOutput::NotModified) => {
                    Ok(response_with_status(StatusCode::NOT_MODIFIED))
                }

                Err(err) => {
                    if let io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied = err.kind() {
                        if let Some((mut fallback, request)) = fallback_and_request.take() {
                            call_fallback(&mut fallback, request).await
                        } else {
                            Ok(not_found())
                        }
                    } else {
                        Err(err)
                    }
                }
            }
        }
    }
}

/// The default fallback service used with [`ServeDir`].
#[derive(Debug, Clone, Copy)]
pub struct DefaultServeDirFallback(Infallible);

impl<ReqBody> Service<Request<ReqBody>> for DefaultServeDirFallback
where
    ReqBody: Send + 'static,
{
    type Response = Response<ResponseBody>;
    type Error = io::Error;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.0 {}
    }

    fn call(&mut self, _req: Request<ReqBody>) -> Self::Future {
        match self.0 {}
    }
}

#[derive(Clone, Debug)]
pub enum ServeVariant {
    Directory {
        append_index_html_on_directories: bool,
    },
    SingleFile {
        mime: HeaderValue,
    },
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PrecompressedVariants {
    pub(crate) gzip: bool,
    pub(crate) deflate: bool,
    pub(crate) br: bool,
}

impl SupportedEncodings for PrecompressedVariants {
    fn gzip(&self) -> bool {
        self.gzip
    }

    fn deflate(&self) -> bool {
        self.deflate
    }

    fn br(&self) -> bool {
        self.br
    }
}

fn response_with_status(status: StatusCode) -> Response<ResponseBody> {
    Response::builder()
        .status(status)
        .body(empty_body())
        .unwrap()
}

fn empty_body() -> ResponseBody {
    let body = Empty::new().map_err(|err| match err {}).boxed_unsync();
    ResponseBody::new(body)
}

fn body_from_bytes(bytes: Bytes) -> ResponseBody {
    let body = Full::from(bytes).map_err(|err| match err {}).boxed_unsync();
    ResponseBody::new(body)
}

fn build_response<IO: AsyncRead + Send + 'static>(
    output: FileOpened<IO>,
) -> Response<ResponseBody> {
    let (maybe_file, size) = match output.extent {
        FileRequestExtent::Full(file, meta) => (Some(file), meta.len),
        FileRequestExtent::Head(meta) => (None, meta.len),
    };

    let mut builder = Response::builder()
        .header(header::CONTENT_TYPE, output.mime_header_value)
        .header(header::ACCEPT_RANGES, "bytes");

    if let Some(encoding) = output.maybe_encoding {
        builder = builder.header(header::CONTENT_ENCODING, encoding.into_header_value());
    }

    if let Some(last_modified) = output.last_modified {
        builder = builder.header(header::LAST_MODIFIED, last_modified.0.to_string());
    }

    match output.maybe_range {
        Some(Ok(ranges)) => {
            if let Some(range) = ranges.first() {
                if ranges.len() > 1 {
                    builder
                        .header(header::CONTENT_RANGE, format!("bytes */{size}"))
                        .status(StatusCode::RANGE_NOT_SATISFIABLE)
                        .body(body_from_bytes(Bytes::from(
                            "Cannot serve multipart range requests",
                        )))
                        .unwrap()
                } else {
                    let body = if let Some(file) = maybe_file {
                        let range_size = range.end() - range.start() + 1;
                        ResponseBody::new(
                            AsyncReadBody::with_capacity_limited(
                                file,
                                output.chunk_size,
                                range_size,
                            )
                            .boxed_unsync(),
                        )
                    } else {
                        empty_body()
                    };

                    builder
                        .header(
                            header::CONTENT_RANGE,
                            format!("bytes {}-{}/{}", range.start(), range.end(), size),
                        )
                        .header(header::CONTENT_LENGTH, range.end() - range.start() + 1)
                        .status(StatusCode::PARTIAL_CONTENT)
                        .body(body)
                        .unwrap()
                }
            } else {
                builder
                    .header(header::CONTENT_RANGE, format!("bytes */{size}"))
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .body(body_from_bytes(Bytes::from(
                        "No range found after parsing range header, please file an issue",
                    )))
                    .unwrap()
            }
        }

        Some(Err(_)) => builder
            .header(header::CONTENT_RANGE, format!("bytes */{size}"))
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .body(empty_body())
            .unwrap(),

        // Not a range request
        None => {
            let body = if let Some(file) = maybe_file {
                ResponseBody::new(
                    AsyncReadBody::with_capacity(file, output.chunk_size).boxed_unsync(),
                )
            } else {
                empty_body()
            };

            builder
                .header(header::CONTENT_LENGTH, size.to_string())
                .body(body)
                .unwrap()
        }
    }
}

async fn call_fallback<F, B, FResBody>(
    fallback: &mut F,
    req: Request<B>,
) -> io::Result<Response<ResponseBody>>
where
    F: Service<Request<B>, Response = Response<FResBody>> + Clone,
    F::Error: Into<io::Error>,
    F::Future: Send,
    FResBody: Body<Data = Bytes> + Send + 'static,
    FResBody::Error: Into<BoxError>,
{
    fallback
        .call(req)
        .err_into()
        .map_ok(|response| {
            response
                .map(|body| {
                    body.map_err(|err| match err.into().downcast::<io::Error>() {
                        Ok(err) => *err,
                        Err(err) => io::Error::new(io::ErrorKind::Other, err),
                    })
                    .boxed_unsync()
                })
                .map(ResponseBody::new)
        })
        .await
}

fn not_found() -> Response<ResponseBody> {
    response_with_status(StatusCode::NOT_FOUND)
}
