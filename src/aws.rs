use s3::bucket::Bucket;
use s3::creds::Credentials;

use crate::config::AWS;
use crate::uploader::{Uploader, UploaderError};

use std::io::prelude::*;
use std::io::Write;
use std::path::PathBuf;

use snafu::{ResultExt, Snafu};

use async_trait::async_trait;

use flate2::write::{GzEncoder, ZlibEncoder};
use flate2::Compression;

use chrono::{DateTime, Utc};

#[derive(Debug, Snafu)]
pub enum AWSError {
    #[snafu(display("Invalid credentials: {}", source))]
    InvalidCredentials { source: s3::creds::AwsCredsError },

    #[snafu(display("Error creating bucket: {}", source))]
    InvalidBucket { source: s3::S3Error },
}

pub struct AWSBucket {
    name: String,
    bucket: Bucket,
}

impl AWSBucket {
    pub fn new(config: AWS, bucket_name: &str) -> Result<AWSBucket, AWSError> {
        let credentials = Credentials::new(
            Some(&config.access_key),
            Some(&config.secret_key),
            None,
            None,
            None,
        )
        .context(InvalidCredentials)?;
        let bucket = Bucket::new(bucket_name, config.region.parse().unwrap(), credentials)
            .context(InvalidBucket)?;
        return Ok(AWSBucket { name: String::from(bucket_name), bucket });
    }
}

#[async_trait]
impl Uploader for AWSBucket {
    async fn upload_file(&self, path: PathBuf) -> Result<(), UploaderError> {
        let mut content: Vec<u8> = vec![];
        let mut file = match std::fs::File::open(path.clone()) {
            Ok(file) => file,
            Err(error) => return Err(UploaderError::LocalError(error)),
        };

        file.read_to_end(&mut content)?;
        let path = path.to_str().unwrap();
        self.bucket.put_object(path, &content).await?;
        Ok(())
    }

    async fn upload_file_compressed(&self, path: PathBuf) -> Result<(), UploaderError> {
        let mut content: Vec<u8> = vec![];
        let mut file = match std::fs::File::open(path.clone()) {
            Ok(file) => file,
            Err(error) => return Err(UploaderError::LocalError(error)),
        };

        file.read_to_end(&mut content)?;

        let mut e = ZlibEncoder::new(Vec::new(), Compression::default());
        e.write_all(&content)?;
        let compressed_bytes = match e.finish() {
            Ok(bytes) => bytes,
            Err(_) => return Err(UploaderError::CompressionError),
        };

        let path = path.to_str().unwrap();
        self.bucket.put_object(path, &compressed_bytes).await?;
        Ok(())
    }

    async fn upload_folder(&self, path: PathBuf) -> Result<(), UploaderError> {
        if !path.is_dir() {
            return Err(UploaderError::NotADirectory);
        }

        let dirs = std::fs::read_dir(path)?
            .map(|res| res.map(|e| e.path()))
            .collect::<Result<Vec<_>, std::io::Error>>();

        let mut futures = vec![];

        for dir in dirs {
            for file in dir {
                futures.push(self.upload_file(file));
            }
        }

        futures::future::join_all(futures).await;
        Ok(())
    }

    async fn upload_folder_compressed(&self, path: PathBuf) -> Result<(), UploaderError> {
        if !path.is_dir() {
            return Err(UploaderError::NotADirectory);
        }

        let now: DateTime<Utc> = Utc::now();
        let archive = std::fs::File::create(format!(
            "{}-{}.tar.zz",
            path.file_name().unwrap().to_str().unwrap(),
            now
        ))?;
        let e = GzEncoder::new(archive, Compression::default());
        let mut tar = tar::Builder::new(e);
        tar.append_dir_all(".", path.clone())?;
        self.upload_file(path).await?;
        Ok(())
    }
}
