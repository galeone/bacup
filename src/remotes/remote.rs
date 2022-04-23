// Copyright 2022 Paolo Galeone <nessuno@nerdz.eu>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use async_trait::async_trait;

use std::fmt;
use std::path::{Path, PathBuf};
use std::string::String;

use chrono::DateTime;
use chrono::Utc;

use async_compression::tokio::write::GzipEncoder;

use dyn_clone::DynClone;

use crate::remotes::aws::Error as AWSError;

use tempfile::NamedTempFile;

use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use log::info;

#[derive(Debug)]
pub enum Error {
    LocalError(std::io::Error),
    RemoteError(AWSError),
    CompressionError,
    NotADirectory,
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::LocalError(error)
    }
}

impl From<AWSError> for Error {
    fn from(error: AWSError) -> Self {
        Error::RemoteError(error)
    }
}

impl std::error::Error for Error {}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::LocalError(error) => write!(f, "Local (IO) error: {}", error),
            Error::CompressionError => write!(f, "Unable to compress the file/folder"),
            Error::NotADirectory => write!(f, "The specified file is not a directory"),
            Error::RemoteError(error) => write!(f, "Remote error: {}", error),
        }
    }
}

#[async_trait]
pub trait Remote: DynClone + Send + Sync {
    async fn upload_file(&self, path: &Path, remote_path: &Path) -> Result<(), Error>;
    async fn upload_folder(&self, paths: &[PathBuf], remote_path: &Path) -> Result<(), Error>;
    async fn upload_file_compressed(&self, path: &Path, remote_path: &Path) -> Result<(), Error>;
    async fn upload_folder_compressed(&self, path: &Path, remote_path: &Path) -> Result<(), Error>;
    async fn enumerate(&self, remote_path: &Path) -> Result<Vec<String>, Error>;
    async fn delete(&self, remote_path: &Path) -> Result<(), Error>;

    fn name(&self) -> String;

    async fn compress_folder(&self, path: &Path) -> Result<NamedTempFile, Error>
    where
        Self: Sized,
    {
        info!("Compressing folder {}", path.display());
        let archive_path = NamedTempFile::new()?;

        let file = fs::File::create(&archive_path).await?;
        let encoder = GzipEncoder::new(file);

        let mut builder = tokio_tar::Builder::new(encoder);
        builder
            .append_dir_all(path.file_name().unwrap(), path)
            .await?;

        let mut encoder = builder.into_inner().await?;
        encoder.flush().await?;
        encoder.shutdown().await?;
        info!("Compression of folder {} done.", path.display());
        Ok(archive_path)
    }

    async fn compress_file(&self, path: &Path) -> Result<Vec<u8>, Error>
    where
        Self: Sized,
    {
        info!("Compressing file {}...", path.display());
        let mut content: Vec<u8> = vec![];
        let mut file = match fs::File::open(path).await {
            Ok(file) => file,
            Err(error) => return Err(Error::LocalError(error)),
        };

        file.read_to_end(&mut content).await?;

        let mut e = GzipEncoder::new(Vec::new());
        e.write_all(&content).await?;
        e.shutdown().await?;

        info!("Compression of file {} done.", path.display());
        Ok(content)
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
