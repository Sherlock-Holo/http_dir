use std::error::Error;
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::task::{Context, Poll};

use bytes::Bytes;
use http::{HeaderValue, Request, Response};
use http_body::Body;
use mime_guess::{mime, Mime};
use tower_service::Service;

use crate::fs::single_file::SingleFileFilesystem;
use crate::fs::Filesystem;
use crate::serve_dir::ResponseBody;
use crate::{DefaultServeDirFallback, ServeDir};

/// Service that serves a file
#[derive(Debug, Clone)]
pub struct ServeFile<FS, F = DefaultServeDirFallback> {
    inner: ServeDir<FS, F>,
}

impl<FS> ServeFile<FS, DefaultServeDirFallback> {
    /// Create a new ServeFile.
    ///
    /// The Content-Type will be guessed from the file extension.
    pub fn new<P: Into<PathBuf>>(path: P, filesystem: FS) -> ServeFile<SingleFileFilesystem<FS>> {
        let path = path.into();

        let guess = mime_guess::from_path(&path);
        let mime = guess
            .first_raw()
            .map(HeaderValue::from_static)
            .unwrap_or_else(|| {
                HeaderValue::from_str(mime::APPLICATION_OCTET_STREAM.as_ref()).unwrap()
            });

        ServeFile {
            inner: ServeDir::new_single_file(SingleFileFilesystem::new(path, filesystem), mime),
        }
    }

    /// Create a new ServeFile with a specific mime type.
    ///
    /// # Panics
    /// Will panic if the mime type isn’t a valid
    /// [header value](https://docs.rs/http/latest/http/header/struct.HeaderValue.html).
    pub fn new_with_mime<P: Into<PathBuf>>(
        path: P,
        mime: &Mime,
        filesystem: FS,
    ) -> ServeFile<SingleFileFilesystem<FS>> {
        let mime = HeaderValue::from_str(mime.as_ref()).expect("mime isn't a valid header value");

        ServeFile {
            inner: ServeDir::new_single_file(
                SingleFileFilesystem::new(path.into(), filesystem),
                mime,
            ),
        }
    }
}

impl<FS, F> ServeFile<FS, F> {
    /// Set a specific read buffer chunk size.
    ///
    /// The default capacity is 64kb.
    pub fn with_buf_chunk_size(mut self, chunk_size: usize) -> Self {
        self.inner.buf_chunk_size = chunk_size;
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
        self.inner
            .precompressed_variants
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
        self.inner
            .precompressed_variants
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
        self.inner
            .precompressed_variants
            .get_or_insert(Default::default())
            .deflate = true;
        self
    }
}

impl<ReqBody, F, FResBody, FS> Service<Request<ReqBody>> for ServeFile<FS, F>
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
        self.inner.poll_ready(cx)
    }

    #[inline]
    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        self.inner.call(req)
    }
}

#[cfg(test)]
mod tests {
    use http::StatusCode;
    use http_body::Body as HttpBody;
    use hyper::Body;
    use tower::ServiceExt;

    use super::*;
    use crate::fs::disk::DiskFilesystem;

    #[tokio::test]
    async fn basic() {
        let svc = ServeFile::new("README.md", DiskFilesystem::from("."));

        let req = Request::builder()
            .uri("/README.md")
            .body(Body::empty())
            .unwrap();
        let res = svc.oneshot(req).await.unwrap();

        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()["content-type"], "text/markdown");

        let body = body_into_text(res.into_body()).await;

        let contents = std::fs::read_to_string("./README.md").unwrap();
        assert_eq!(body, contents);
    }

    async fn body_into_text<B>(body: B) -> String
    where
        B: HttpBody<Data = Bytes> + Unpin,
        B::Error: std::fmt::Debug,
    {
        let bytes = hyper::body::to_bytes(body).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }
}
