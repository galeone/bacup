use async_trait::async_trait;
use std::path::PathBuf;

#[derive(Debug)]
pub enum UploaderError {
    LocalError(std::io::Error),
    RemoteError(s3::S3Error),
    CompressionError,
    NotADirectory,
}

impl From<std::io::Error> for UploaderError {
    fn from(error: std::io::Error) -> Self {
        UploaderError::LocalError(error)
    }
}

impl From<s3::S3Error> for UploaderError {
    fn from(error: s3::S3Error) -> Self {
        UploaderError::RemoteError(error)
    }
}

#[async_trait]
pub trait Uploader {
    async fn upload_file(&self, path: PathBuf) -> Result<(), UploaderError>;
    async fn upload_file_compressed(&self, path: PathBuf) -> Result<(), UploaderError>;
    async fn upload_folder(&self, path: PathBuf) -> Result<(), UploaderError>;
    async fn upload_folder_compressed(&self, path: PathBuf) -> Result<(), UploaderError>;
}
