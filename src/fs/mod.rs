//! Allow user implement own filesystem, to provide file for the [`ServeDir`](crate::ServeDir)

use std::future::Future;
use std::io;
use std::path::Path;
use std::time::SystemTime;

use tokio::io::{AsyncRead, AsyncSeek};

#[cfg(feature = "disk")]
/// a [`tokio`](https://docs.rs/tokio/latest/tokio/) based implement
pub mod disk;
#[cfg(feature = "include-dir")]
/// a [`include_dir`](https://docs.rs/include_dir/latest/include_dir) based implement
pub mod include_dir;
pub(crate) mod single_file;

/// A simple Metadata
#[derive(Debug, Clone)]
pub struct Metadata {
    /// file last modified time
    pub modified: Option<SystemTime>,

    /// file size
    pub len: u64,
}

/// File extension
pub trait FileExt {
    type Metadata<'a>: Future<Output = io::Result<Metadata>> + Send + Sync + 'a
    where
        Self: 'a;

    /// get file [`Metadata`]
    fn metadata(&self) -> Self::Metadata<'_>;
}

/// Define a filesystem trait
pub trait Filesystem {
    type File: AsyncRead + AsyncSeek + FileExt + Send + Sync + Unpin;
    type OpenFile<'a>: Future<Output = io::Result<Self::File>> + Send + Sync + 'a
    where
        Self: 'a;
    type IsDir<'a>: Future<Output = io::Result<bool>> + Send + Sync + 'a
    where
        Self: 'a;

    type Metadata<'a>: Future<Output = io::Result<Metadata>> + Send + Sync + 'a
    where
        Self: 'a;

    /// open a [`file`](Filesystem::File) by path
    fn open<'a>(&'a mut self, path: &'a Path) -> Self::OpenFile<'a>;

    /// check the path is a dir or not
    fn is_dir<'a>(&'a self, path: &'a Path) -> Self::IsDir<'a>;

    /// get [`Metadata`] by path
    fn metadata<'a>(&'a self, path: &'a Path) -> Self::Metadata<'a>;
}
