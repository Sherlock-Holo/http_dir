use std::future::{ready, Future};
use std::io;
use std::io::{Error, ErrorKind, SeekFrom};
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};

use include_dir::{Dir, DirEntry, File};
use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};

use crate::fs::{FileExt, Filesystem, Metadata};

pub struct IncludeDirFile {
    index: usize,
    file: &'static File<'static>,
}

impl AsyncRead for IncludeDirFile {
    #[inline]
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut data = self.file.contents();
        if self.index >= data.len() {
            return Poll::Ready(Ok(()));
        }
        data = &data[self.index..];

        Pin::new(&mut data).poll_read(cx, buf)
    }
}

impl AsyncSeek for IncludeDirFile {
    fn start_seek(mut self: Pin<&mut Self>, position: SeekFrom) -> io::Result<()> {
        match position {
            SeekFrom::Start(start) => {
                self.index = start as _;
            }
            SeekFrom::End(end) => {
                self.index = self
                    .file
                    .contents()
                    .len()
                    .checked_add_signed(end as _)
                    .ok_or_else(|| {
                        Error::new(
                            ErrorKind::InvalidInput,
                            "invalid seek to a negative or overflowing position",
                        )
                    })?;
            }
            SeekFrom::Current(index) => {
                self.index = self.index.checked_add_signed(index as _).ok_or_else(|| {
                    Error::new(
                        ErrorKind::InvalidInput,
                        "invalid seek to a negative or overflowing position",
                    )
                })?;
            }
        }

        Ok(())
    }

    #[inline]
    fn poll_complete(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<u64>> {
        Poll::Ready(Ok(self.index as _))
    }
}

impl FileExt for IncludeDirFile {
    type Metadata<'a> = impl Future<Output=io::Result<Metadata>> + Send + Sync + 'a where Self: 'a;

    fn metadata(&self) -> Self::Metadata<'_> {
        ready(Ok(self._metadata()))
    }
}

impl IncludeDirFile {
    fn _metadata(&self) -> Metadata {
        let len = self.file.contents().len() as u64;

        self.file
            .metadata()
            .map(|raw_metadata| Metadata {
                modified: Some(raw_metadata.modified()),
                len,
            })
            .unwrap_or(Metadata {
                modified: None,
                len,
            })
    }
}

#[derive(Debug, Clone)]
pub struct IncludeDirFilesystem {
    dir: Dir<'static>,
}

impl IncludeDirFilesystem {
    pub fn new(dir: Dir<'static>) -> Self {
        Self { dir }
    }
}

impl From<Dir<'static>> for IncludeDirFilesystem {
    fn from(value: Dir<'static>) -> Self {
        Self::new(value)
    }
}

impl Filesystem for IncludeDirFilesystem {
    type File = IncludeDirFile;
    type OpenFile<'a> = impl Future<Output=io::Result<Self::File>> + Send + Sync + 'a where Self: 'a;
    type IsDir<'a> = impl Future<Output=io::Result<bool>> + Send + Sync + 'a where Self: 'a;
    type Metadata<'a> = impl Future<Output=io::Result<Metadata>> + Send + Sync + 'a where Self: 'a;

    fn open<'a>(&'a mut self, path: &'a Path) -> Self::OpenFile<'a> {
        ready(
            self.dir
                .get_file(path)
                .ok_or_else(|| Error::from(ErrorKind::NotFound))
                .map(|file| IncludeDirFile { index: 0, file }),
        )
    }

    fn is_dir<'a>(&'a self, path: &'a Path) -> Self::IsDir<'a> {
        ready(
            self.dir
                .get_entry(path)
                .ok_or_else(|| Error::from(ErrorKind::NotFound))
                .map(|entry| matches!(entry, DirEntry::Dir(_))),
        )
    }

    fn metadata<'a>(&'a self, path: &'a Path) -> Self::Metadata<'a> {
        let result = match self
            .dir
            .get_entry(path)
            .ok_or_else(|| Error::from(ErrorKind::NotFound))
            .map(|entry| match entry {
                DirEntry::Dir(_) => Err(Error::from(ErrorKind::NotFound)),
                DirEntry::File(file) => Ok(IncludeDirFile { index: 0, file }._metadata()),
            }) {
            Err(err) => Err(err),
            Ok(Err(err)) => Err(err),
            Ok(Ok(metadata)) => Ok(metadata),
        };

        ready(result)
    }
}
