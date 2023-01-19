use std::future::Future;
use std::io;
use std::io::{ErrorKind, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::fs;
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};

use crate::fs::{FileExt, Filesystem, Metadata};

#[derive(Debug)]
pub struct DiskFile(File);

impl AsyncRead for DiskFile {
    #[inline]
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_read(cx, buf)
    }
}

impl AsyncSeek for DiskFile {
    #[inline]
    fn start_seek(self: Pin<&mut Self>, position: SeekFrom) -> io::Result<()> {
        Pin::new(&mut self.get_mut().0).start_seek(position)
    }

    #[inline]
    fn poll_complete(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<u64>> {
        Pin::new(&mut self.get_mut().0).poll_complete(cx)
    }
}

impl FileExt for DiskFile {
    type Metadata<'a> = impl Future<Output=io::Result<Metadata>> + Send + Sync + 'a where Self: 'a;

    fn metadata(&self) -> Self::Metadata<'_> {
        async move {
            let raw_metadata = self.0.metadata().await?;
            let modified = raw_metadata.modified().ok();

            Ok(Metadata {
                modified,
                len: raw_metadata.len(),
            })
        }
    }
}

#[derive(Debug, Clone)]
pub struct DiskFilesystem {
    base: PathBuf,
}

impl From<&str> for DiskFilesystem {
    fn from(value: &str) -> Self {
        Self::new(PathBuf::from(value))
    }
}

impl From<&Path> for DiskFilesystem {
    fn from(value: &Path) -> Self {
        Self::new(value.to_path_buf())
    }
}

impl From<PathBuf> for DiskFilesystem {
    fn from(value: PathBuf) -> Self {
        Self::new(value)
    }
}

impl DiskFilesystem {
    pub fn new(base: PathBuf) -> Self {
        Self { base }
    }

    fn build_and_validate_path(&self, path: &Path) -> Option<PathBuf> {
        let mut path_to_file = self.base.clone();
        for component in path.components() {
            match component {
                Component::Normal(comp) => {
                    // protect against paths like `/foo/c:/bar/baz` (#204)
                    if Path::new(&comp)
                        .components()
                        .all(|c| matches!(c, Component::Normal(_)))
                    {
                        path_to_file.push(comp)
                    } else {
                        return None;
                    }
                }
                Component::CurDir => {}
                Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                    return None;
                }
            }
        }
        Some(path_to_file)
    }
}

impl Filesystem for DiskFilesystem {
    type File = DiskFile;
    type OpenFile<'a> = impl Future<Output=io::Result<Self::File>> + Send + Sync + 'a where Self: 'a;
    type IsDir<'a> = impl Future<Output=io::Result<bool>> + Send + Sync + 'a where Self: 'a;
    type Metadata<'a> = impl Future<Output=io::Result<Metadata>> + Send + Sync + 'a where Self: 'a;

    fn open<'a>(&'a mut self, path: &'a Path) -> Self::OpenFile<'a> {
        async move {
            let path = match self.build_and_validate_path(path) {
                None => return Err(io::Error::from(ErrorKind::NotFound)),
                Some(path) => path,
            };

            let file = File::open(&path).await?;

            Ok(DiskFile(file))
        }
    }

    fn is_dir<'a>(&'a self, path: &'a Path) -> Self::IsDir<'a> {
        async move {
            let path = match self.build_and_validate_path(path) {
                None => return Err(io::Error::from(ErrorKind::NotFound)),
                Some(path) => path,
            };

            Ok(fs::metadata(&path).await?.is_dir())
        }
    }

    fn metadata<'a>(&'a self, path: &'a Path) -> Self::Metadata<'a> {
        async move {
            let path = match self.build_and_validate_path(path) {
                None => return Err(io::Error::from(ErrorKind::NotFound)),
                Some(path) => path,
            };

            let raw_metadata = fs::metadata(&path).await?;

            let modified = raw_metadata.modified().ok();

            Ok(Metadata {
                modified,
                len: raw_metadata.len(),
            })
        }
    }
}
