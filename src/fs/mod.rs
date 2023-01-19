use std::future::Future;
use std::io;
use std::path::Path;
use std::time::SystemTime;

use tokio::io::{AsyncRead, AsyncSeek};

#[cfg(feature = "disk")]
pub mod disk;
#[cfg(feature = "include-dir")]
pub mod include_dir;

#[derive(Debug, Clone)]
pub struct Metadata {
    pub modified: Option<SystemTime>,
    pub len: u64,
}

pub trait FileExt {
    type Metadata<'a>: Future<Output = io::Result<Metadata>> + Send + Sync + 'a
    where
        Self: 'a;

    fn metadata(&self) -> Self::Metadata<'_>;
}

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

    fn open<'a>(&'a mut self, path: &'a Path) -> Self::OpenFile<'a>;

    fn is_dir<'a>(&'a self, path: &'a Path) -> Self::IsDir<'a>;

    fn metadata<'a>(&'a self, path: &'a Path) -> Self::Metadata<'a>;
}
