use s3::bucket::Bucket;
use s3::creds::Credentials;

use crate::config::AWSConfig;
use crate::remotes::uploader;

use std::io::prelude::*;

use std::fs::File;
use std::path::{Path, PathBuf};

use async_trait::async_trait;

use std::fmt;

#[derive(Debug)]
pub enum Error {
    InvalidCredentials(s3::creds::AwsCredsError),
    InvalidBucket(s3::S3Error),
}

impl From<s3::creds::AwsCredsError> for Error {
    fn from(error: s3::creds::AwsCredsError) -> Self {
        Error::InvalidCredentials(error)
    }
}

impl From<s3::S3Error> for Error {
    fn from(error: s3::S3Error) -> Self {
        Error::InvalidBucket(error)
    }
}

impl std::error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidCredentials(error) => write!(f, "Invalid credentials: {}", error),
            Error::InvalidBucket(error) => write!(f, "Error creating bucket object: {}", error),
        }
    }
}

#[derive(Clone)]
pub struct AWSBucket {
    name: String,
    bucket: Bucket,
}

impl AWSBucket {
    pub async fn new(config: AWSConfig, bucket_name: &str) -> Result<AWSBucket, Error> {
        let credentials = Credentials::new(
            Some(&config.access_key),
            Some(&config.secret_key),
            None,
            None,
            None,
        )?;
        let bucket = Bucket::new(bucket_name, config.region.parse().unwrap(), credentials)?;

        // Perform a listing request to check if the configuration is ok
        bucket
            .list(String::from("/"), Some(String::from("/")))
            .await?;
        return Ok(AWSBucket {
            name: String::from(bucket_name),
            bucket,
        });
    }
}

#[async_trait]
impl uploader::Uploader for AWSBucket {
    fn name(&self) -> String {
        return self.name.clone();
    }

    async fn upload_file(&self, path: &Path, remote_path: &Path) -> Result<(), uploader::Error> {
        let mut content: Vec<u8> = vec![];
        let mut file = File::open(path.clone())?;
        file.read_to_end(&mut content)?;
        let remote_path = remote_path.to_str().unwrap();
        self.bucket.put_object(remote_path, &content).await?;
        Ok(())
    }

    async fn upload_file_compressed(
        &self,
        path: &Path,
        remote_path: &Path,
    ) -> Result<(), uploader::Error> {
        let compressed_bytes = self.compress_file(path)?;
        let remote_path = self.remote_compressed_file_path(remote_path);
        self.bucket
            .put_object(remote_path.to_str().unwrap(), &compressed_bytes)
            .await?;
        Ok(())
    }

    async fn upload_folder(
        &self,
        paths: &Vec<PathBuf>,
        remote_path: &Path,
    ) -> Result<(), uploader::Error> {
        let tot = paths.len();

        let mut local_prefix = paths.iter().min_by(|a, b| a.cmp(b)).unwrap();
        // The local_prefix found is the shortest path inside the folder we want to backup.

        // If it is a folder, we of course don't want to consider this a prefix, but its parent.
        let single_location = paths.len() <= 1;
        let parent: PathBuf;
        if !single_location {
            parent = local_prefix.parent().unwrap().to_path_buf();
            local_prefix = &parent;
        }

        // Strip local prefix from remote pathsa
        let mut remote_paths: Vec<PathBuf> = Vec::with_capacity(tot);
        for i in 0..tot {
            remote_paths.push(remote_path.join(paths[i].strip_prefix(local_prefix).unwrap()));
        }

        // Upload all the files one by one
        let mut futures = vec![];
        // Add only files - paths are automatically created remotely from the full file path
        for i in 0..tot {
            if paths[i].is_file() {
                futures.push(self.upload_file(&paths[i], &remote_paths[i]));
            }
        }

        futures::future::join_all(futures).await;
        Ok(())
    }

    async fn upload_folder_compressed(
        &self,
        path: &Path,
        remote_path: &Path,
    ) -> Result<(), uploader::Error> {
        if !path.is_dir() {
            return Err(uploader::Error::NotADirectory);
        }

        let remote_path = self.remote_archive_path(remote_path);
        let compressed_folder = self.compress_folder(path)?;
        self.upload_file(compressed_folder.path(), &remote_path)
            .await?;
        Ok(())
    }
}
