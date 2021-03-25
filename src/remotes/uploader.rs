use async_trait::async_trait;

use std::fmt;
use std::path::PathBuf;
use std::string::String;

use dyn_clone::DynClone;

#[derive(Debug)]
pub enum Error {
    LocalError(std::io::Error),
    RemoteError(s3::S3Error),
    CompressionError,
    NotADirectory,
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::LocalError(error)
    }
}

impl From<s3::S3Error> for Error {
    fn from(error: s3::S3Error) -> Self {
        Error::RemoteError(error)
    }
}

impl std::error::Error for Error {}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::LocalError(error) => write!(f, "Local (IO) error: {}", error),
            Error::RemoteError(error) => write!(f, "AWS Remote error: {}", error),
            Error::CompressionError => write!(f, "Unable to compress the file/folder"),
            Error::NotADirectory => write!(f, "The specified file is not a directory"),
        }
    }
}

#[async_trait]
pub trait Uploader: DynClone {
    async fn upload_file(&self, path: PathBuf) -> Result<(), Error>;
    async fn upload_file_compressed(&self, path: PathBuf) -> Result<(), Error>;
    async fn upload_folder(&self, path: PathBuf) -> Result<(), Error>;
    async fn upload_folder_compressed(&self, path: PathBuf) -> Result<(), Error>;
    fn name(&self) -> String;
}
