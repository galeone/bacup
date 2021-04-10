use async_trait::async_trait;

use std::fmt;
use std::path::{Path, PathBuf};
use std::string::String;

use chrono::DateTime;
use chrono::Utc;

use flate2::write::GzEncoder;
use flate2::Compression;
use std::io::prelude::*;

use dyn_clone::DynClone;

use tempfile::NamedTempFile;

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
    async fn upload_file(&self, path: &Path, remote_path: &Path) -> Result<(), Error>;
    async fn upload_folder(&self, paths: &[PathBuf], remote_path: &Path) -> Result<(), Error>;
    async fn upload_file_compressed(&self, path: &Path, remote_path: &Path) -> Result<(), Error>;
    async fn upload_folder_compressed(&self, path: &Path, remote_path: &Path) -> Result<(), Error>;
    fn name(&self) -> String;

    fn compress_folder(&self, path: &Path) -> Result<NamedTempFile, Error> {
        let archive_path = NamedTempFile::new()?;

        let archive = std::fs::File::create(&archive_path)?;
        let mut encoder = GzEncoder::new(archive, Compression::default());
        {
            let mut tar = tar::Builder::new(&mut encoder);
            tar.append_dir_all(".", path)?;
        }
        let enc_res = encoder.finish();
        if enc_res.is_err() {
            return Err(Error::CompressionError);
        }
        Ok(archive_path)
    }

    fn compress_file(&self, path: &Path) -> Result<Vec<u8>, Error> {
        let mut content: Vec<u8> = vec![];
        let mut file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) => return Err(Error::LocalError(error)),
        };

        file.read_to_end(&mut content)?;

        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(&content)?;

        match e.finish() {
            Ok(bytes) => Ok(bytes),
            Err(_) => Err(Error::CompressionError),
        }
    }

    fn remote_archive_path(&self, remote_path: &Path) -> PathBuf {
        let now: DateTime<Utc> = Utc::now();
        let parent = match remote_path.parent() {
            Some(path) => path.to_path_buf(),
            None => PathBuf::from("/"),
        };

        parent.join(format!(
            "{}-{}.tar.gz",
            now.format("%Y-%m-%d-%H.%M"),
            remote_path.file_name().unwrap().to_str().unwrap()
        ))
    }

    fn remote_compressed_file_path(&self, remote_path: &Path) -> PathBuf {
        let now: DateTime<Utc> = Utc::now();
        let parent = match remote_path.parent() {
            Some(path) => path.to_path_buf(),
            None => PathBuf::from("/"),
        };

        parent.join(format!(
            "{}-{}.gz",
            now.format("%Y-%m-%d-%H.%M"),
            remote_path.file_name().unwrap().to_str().unwrap()
        ))
    }
}
