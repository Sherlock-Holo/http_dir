#![feature(impl_trait_in_assoc_type)]

//! HTTP file server, to access files on the [`Filesystem`]. User can implement own [`Filesystem`],
//! also can use [`DiskFilesystem`](fs::disk::DiskFilesystem) or
//! [`IncludeDirFilesystem`](fs::include_dir::IncludeDirFilesystem) directly
//!
//! # Note
//!
//! This crate require [TAIT](https://github.com/rust-lang/rust/issues/63063) feature, it will be
//! stable soon but now it is a nightly
//! feature
//!
//! # Example
//! ```
//! use http_dir::ServeDir;
//! use http_dir::fs::disk::DiskFilesystem;
//!
//! // This will serve files in the "assets" directory and
//! // its subdirectories
//! let service = ServeDir::new(DiskFilesystem::from("assets"));
//!
//! # async {
//! // Run our service using `hyper`
//! let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 3000));
//! hyper::Server::bind(&addr)
//!     .serve(tower::make::Shared::new(service))
//!     .await
//!     .expect("server error");
//! # };
//! ```

use std::io;

use bytes::Bytes;
use http_body::combinators::UnsyncBoxBody;
pub use serve_dir::{DefaultServeDirFallback, ServeDir};
pub use serve_file::ServeFile;

mod async_body;
mod content_encoding;
pub mod fs;
mod headers;
mod open_file;
mod serve_dir;
mod serve_file;
#[cfg(test)]
mod tests;

pub type ResponseBody = UnsyncBoxBody<Bytes, io::Error>;
