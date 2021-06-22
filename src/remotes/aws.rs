// Copyright 2021 Paolo Galeone <nessuno@nerdz.eu>
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

use s3::bucket::Bucket;
use s3::creds::Credentials;

use crate::config::AwsConfig;
use crate::remotes::uploader;

use std::io::prelude::*;

use std::fs::File;
use std::path::{Path, PathBuf};

use log::info;

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
pub struct AwsBucket {
    name: String,
    bucket: Bucket,
}

impl AwsBucket {
    pub async fn new(config: AwsConfig, bucket_name: &str) -> Result<AwsBucket, Error> {
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
        Ok(AwsBucket {
            name: String::from(bucket_name),
            bucket,
        })
    }
}

#[async_trait]
impl uploader::Uploader for AwsBucket {
    fn name(&self) -> String {
        self.name.clone()
    }

    async fn enumerate(&self, remote_path: &Path) -> Result<Vec<String>, uploader::Error> {
        info!("aws.enumerate for {}", remote_path.display());
        let mut remote_path = remote_path.to_path_buf();
        remote_path.push("");
        let remote_path = String::from(remote_path.to_str().unwrap());
        info!("listing {}", remote_path);
        let result = self
            .bucket
            .list(String::from(""), Some(String::from("/")))
            .await?;

        let mut ret = Vec::new();
        info!("aws: {}", result.len());
        for res in &result {
            info!("{}", res.name);
            for f in &res.contents {
                info!("key: {}", f.key);
                ret.push(f.key.clone())
            }
        }
        Ok(ret)
    }

    async fn upload_file(&self, path: &Path, remote_path: &Path) -> Result<(), uploader::Error> {
        let mut content: Vec<u8> = vec![];
        let mut file = File::open(path)?;
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
        paths: &[PathBuf],
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

        // Strip local prefix from remote paths
        let mut remote_paths: Vec<PathBuf> = Vec::with_capacity(tot);
        for path in paths.iter() {
            remote_paths.push(remote_path.join(path.strip_prefix(local_prefix).unwrap()));
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
