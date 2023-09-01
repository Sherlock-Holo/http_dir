use std::{
    ffi::OsStr,
    io::{self, SeekFrom},
    ops::RangeInclusive,
    path::PathBuf,
};

use bytes::Bytes;
use http::{header, HeaderValue, Method, Request, Uri};
use http_body::Empty;
use http_range_header::RangeUnsatisfiableError;
use mime_guess::mime;
use tokio::io::AsyncSeekExt;

use super::headers::{IfModifiedSince, IfUnmodifiedSince, LastModified};
use crate::content_encoding::{Encoding, QValue};
use crate::fs::{FileExt, Filesystem, Metadata};
use crate::serve_dir::ServeVariant;

pub(super) enum OpenFileOutput<IO> {
    FileOpened(Box<FileOpened<IO>>),
    Redirect { location: HeaderValue },
    FileNotFound,
    PreconditionFailed,
    NotModified,
}

pub(super) struct FileOpened<IO> {
    pub(super) extent: FileRequestExtent<IO>,
    pub(super) chunk_size: usize,
    pub(super) mime_header_value: HeaderValue,
    pub(super) maybe_encoding: Option<Encoding>,
    pub(super) maybe_range: Option<Result<Vec<RangeInclusive<u64>>, RangeUnsatisfiableError>>,
    pub(super) last_modified: Option<LastModified>,
}

pub(super) enum FileRequestExtent<IO> {
    Full(IO, Metadata),
    Head(Metadata),
}

pub(super) async fn open_file<FS: Filesystem>(
    filesystem: &mut FS,
    variant: &ServeVariant,
    mut path_to_file: PathBuf,
    req: Request<Empty<Bytes>>,
    negotiated_encodings: Vec<(Encoding, QValue)>,
    range_header: Option<String>,
    buf_chunk_size: usize,
) -> io::Result<OpenFileOutput<FS::File>> {
    let if_unmodified_since = req
        .headers()
        .get(header::IF_UNMODIFIED_SINCE)
        .and_then(IfUnmodifiedSince::from_header_value);

    let if_modified_since = req
        .headers()
        .get(header::IF_MODIFIED_SINCE)
        .and_then(IfModifiedSince::from_header_value);

    let mime = match variant {
        ServeVariant::Directory {
            append_index_html_on_directories,
        } => {
            if let Some(output) = maybe_redirect_or_append_path(
                filesystem,
                &mut path_to_file,
                req.uri(),
                *append_index_html_on_directories,
            )
            .await
            {
                return Ok(output);
            }

            mime_guess::from_path(&path_to_file)
                .first_raw()
                .map(HeaderValue::from_static)
                .unwrap_or_else(|| {
                    HeaderValue::from_str(mime::APPLICATION_OCTET_STREAM.as_ref()).unwrap()
                })
        }
        ServeVariant::SingleFile { mime } => mime.clone(),
    };

    if req.method() == Method::HEAD {
        let (meta, maybe_encoding) =
            file_metadata_with_fallback(filesystem, path_to_file, negotiated_encodings).await?;

        let last_modified = meta.modified.map(LastModified::from);
        if let Some(output) = check_modified_headers(
            last_modified.as_ref(),
            if_unmodified_since,
            if_modified_since,
        ) {
            return Ok(output);
        }

        let maybe_range = try_parse_range(range_header.as_deref(), meta.len);

        Ok(OpenFileOutput::FileOpened(Box::new(FileOpened {
            extent: FileRequestExtent::Head(meta),
            chunk_size: buf_chunk_size,
            mime_header_value: mime,
            maybe_encoding,
            maybe_range,
            last_modified,
        })))
    } else {
        let (mut file, maybe_encoding) =
            open_file_with_fallback(filesystem, path_to_file, negotiated_encodings).await?;
        let meta = file.metadata().await?;
        let last_modified = meta.modified.map(LastModified::from);
        if let Some(output) = check_modified_headers(
            last_modified.as_ref(),
            if_unmodified_since,
            if_modified_since,
        ) {
            return Ok(output);
        }

        let maybe_range = try_parse_range(range_header.as_deref(), meta.len);
        if let Some(Ok(ranges)) = maybe_range.as_ref() {
            // if there is any other amount of ranges than 1 we'll return an
            // unsatisfiable later as there isn't yet support for multipart ranges
            if ranges.len() == 1 {
                file.seek(SeekFrom::Start(*ranges[0].start())).await?;
            }
        }

        Ok(OpenFileOutput::FileOpened(Box::new(FileOpened {
            extent: FileRequestExtent::Full(file, meta),
            chunk_size: buf_chunk_size,
            mime_header_value: mime,
            maybe_encoding,
            maybe_range,
            last_modified,
        })))
    }
}

fn check_modified_headers<IO>(
    modified: Option<&LastModified>,
    if_unmodified_since: Option<IfUnmodifiedSince>,
    if_modified_since: Option<IfModifiedSince>,
) -> Option<OpenFileOutput<IO>> {
    if let Some(since) = if_unmodified_since {
        let precondition = modified
            .as_ref()
            .map(|time| since.precondition_passes(time))
            .unwrap_or(false);

        if !precondition {
            return Some(OpenFileOutput::PreconditionFailed);
        }
    }

    if let Some(since) = if_modified_since {
        let unmodified = modified
            .as_ref()
            .map(|time| !since.is_modified(time))
            // no last_modified means its always modified
            .unwrap_or(false);
        if unmodified {
            return Some(OpenFileOutput::NotModified);
        }
    }

    None
}

// Returns the preferred_encoding encoding and modifies the path extension
// to the corresponding file extension for the encoding.
fn preferred_encoding(
    path: &mut PathBuf,
    negotiated_encoding: &[(Encoding, QValue)],
) -> Option<Encoding> {
    let preferred_encoding = Encoding::preferred_encoding(negotiated_encoding);

    if let Some(file_extension) =
        preferred_encoding.and_then(|encoding| encoding.to_file_extension())
    {
        let new_extension = path
            .extension()
            .map(|extension| {
                let mut os_string = extension.to_os_string();
                os_string.push(file_extension);
                os_string
            })
            .unwrap_or_else(|| file_extension.to_os_string());

        path.set_extension(new_extension);
    }

    preferred_encoding
}

// Attempts to open the file with any of the possible negotiated_encodings in the
// preferred order. If none of the negotiated_encodings have a corresponding precompressed
// file the uncompressed file is used as a fallback.
async fn open_file_with_fallback<FS: Filesystem>(
    filesystem: &mut FS,
    mut path: PathBuf,
    mut negotiated_encoding: Vec<(Encoding, QValue)>,
) -> io::Result<(FS::File, Option<Encoding>)> {
    let (metadata, encoding) = loop {
        // Get the preferred encoding among the negotiated ones.
        let encoding = preferred_encoding(&mut path, &negotiated_encoding);
        match (filesystem.open(&path).await, encoding) {
            (Ok(metadata), maybe_encoding) => break (metadata, maybe_encoding),
            (Err(err), Some(encoding)) if err.kind() == io::ErrorKind::NotFound => {
                // Remove the extension corresponding to a precompressed file (.gz, .br, .zz)
                // to reset the path before the next iteration.
                path.set_extension(OsStr::new(""));
                // Remove the encoding from the negotiated_encodings since the file doesn't exist
                negotiated_encoding
                    .retain(|(negotiated_encoding, _)| *negotiated_encoding != encoding);
                continue;
            }
            (Err(err), _) => return Err(err),
        };
    };
    Ok((metadata, encoding))
}

// Attempts to get the file metadata with any of the possible negotiated_encodings in the
// preferred order. If none of the negotiated_encodings have a corresponding precompressed
// file the uncompressed file is used as a fallback.
async fn file_metadata_with_fallback<FS: Filesystem>(
    filesystem: &FS,
    mut path: PathBuf,
    mut negotiated_encoding: Vec<(Encoding, QValue)>,
) -> io::Result<(Metadata, Option<Encoding>)> {
    let (file, encoding) = loop {
        // Get the preferred encoding among the negotiated ones.
        let encoding = preferred_encoding(&mut path, &negotiated_encoding);
        match (filesystem.metadata(&path).await, encoding) {
            (Ok(file), maybe_encoding) => break (file, maybe_encoding),
            (Err(err), Some(encoding)) if err.kind() == io::ErrorKind::NotFound => {
                // Remove the extension corresponding to a precompressed file (.gz, .br, .zz)
                // to reset the path before the next iteration.
                path.set_extension(OsStr::new(""));
                // Remove the encoding from the negotiated_encodings since the file doesn't exist
                negotiated_encoding
                    .retain(|(negotiated_encoding, _)| *negotiated_encoding != encoding);
                continue;
            }
            (Err(err), _) => return Err(err),
        };
    };
    Ok((file, encoding))
}

async fn maybe_redirect_or_append_path<FS: Filesystem>(
    filesystem: &FS,
    path_to_file: &mut PathBuf,
    uri: &Uri,
    append_index_html_on_directories: bool,
) -> Option<OpenFileOutput<FS::File>> {
    if !uri.path().ends_with('/') {
        if filesystem.is_dir(path_to_file).await.unwrap_or(false) {
            let location =
                HeaderValue::from_str(&append_slash_on_path(uri.clone()).to_string()).unwrap();
            Some(OpenFileOutput::Redirect { location })
        } else {
            None
        }
    } else if filesystem.is_dir(path_to_file).await.unwrap_or(false) {
        if append_index_html_on_directories {
            path_to_file.push("index.html");
            None
        } else {
            Some(OpenFileOutput::FileNotFound)
        }
    } else {
        None
    }
}

fn try_parse_range(
    maybe_range_ref: Option<&str>,
    file_size: u64,
) -> Option<Result<Vec<RangeInclusive<u64>>, RangeUnsatisfiableError>> {
    maybe_range_ref.map(|header_value| {
        http_range_header::parse_range_header(header_value)
            .and_then(|first_pass| first_pass.validate(file_size))
    })
}

fn append_slash_on_path(uri: Uri) -> Uri {
    let http::uri::Parts {
        scheme,
        authority,
        path_and_query,
        ..
    } = uri.into_parts();

    let mut uri_builder = Uri::builder();

    if let Some(scheme) = scheme {
        uri_builder = uri_builder.scheme(scheme);
    }

    if let Some(authority) = authority {
        uri_builder = uri_builder.authority(authority);
    }

    let uri_builder = if let Some(path_and_query) = path_and_query {
        if let Some(query) = path_and_query.query() {
            uri_builder.path_and_query(format!("{}/?{}", path_and_query.path(), query))
        } else {
            uri_builder.path_and_query(format!("{}/", path_and_query.path()))
        }
    } else {
        uri_builder.path_and_query("/")
    };

    uri_builder.build().unwrap()
}
