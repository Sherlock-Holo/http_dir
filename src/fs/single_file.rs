use std::future::Future;
use std::io;
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};

use crate::fs::{Filesystem, Metadata};

#[derive(Debug, Clone)]
pub struct SingleFileFilesystem<F> {
    file_path: PathBuf,
    filesystem: F,
}

impl<F> SingleFileFilesystem<F> {
    pub fn new(file_path: PathBuf, filesystem: F) -> Self {
        Self {
            file_path,
            filesystem,
        }
    }
}

impl<F> Filesystem for SingleFileFilesystem<F>
where
    F: Filesystem + Send + Sync + 'static,
{
    type File = F::File;
    type OpenFile<'a> = impl Future<Output=io::Result<Self::File>> + Send + Sync + 'a where Self: 'a;
    type IsDir<'a> = impl Future<Output=io::Result<bool>> + Send + Sync + 'a where Self: 'a;
    type Metadata<'a> = impl Future<Output=io::Result<Metadata>> + Send + Sync + 'a where Self: 'a;

    #[inline]
    fn open<'a>(&'a mut self, path: &'a Path) -> Self::OpenFile<'a> {
        async move {
            if self.file_path != path {
                return Err(Error::from(ErrorKind::NotFound));
            }

            self.filesystem.open(path).await
        }
    }

    #[inline]
    fn is_dir<'a>(&'a self, path: &'a Path) -> Self::IsDir<'a> {
        async move {
            if self.file_path != path {
                return Err(Error::from(ErrorKind::NotFound));
            }

            self.filesystem.is_dir(path).await
        }
    }

    #[inline]
    fn metadata<'a>(&'a self, path: &'a Path) -> Self::Metadata<'a> {
        async move {
            if self.file_path != path {
                return Err(Error::from(ErrorKind::NotFound));
            }

            self.filesystem.metadata(path).await
        }
    }
}
