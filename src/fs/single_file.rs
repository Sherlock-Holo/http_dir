use std::path::{Path, PathBuf};

use crate::fs::Filesystem;

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
    type OpenFile<'a> = F::OpenFile<'a> where Self: 'a;
    type IsDir<'a> = F::IsDir<'a> where Self: 'a;
    type Metadata<'a> = F::Metadata<'a> where Self: 'a;

    #[inline]
    fn open<'a>(&'a mut self, _path: &'a Path) -> Self::OpenFile<'a> {
        self.filesystem.open(&self.file_path)
    }

    #[inline]
    fn is_dir<'a>(&'a self, _path: &'a Path) -> Self::IsDir<'a> {
        self.filesystem.is_dir(&self.file_path)
    }

    #[inline]
    fn metadata<'a>(&'a self, _path: &'a Path) -> Self::Metadata<'a> {
        self.filesystem.metadata(&self.file_path)
    }
}
