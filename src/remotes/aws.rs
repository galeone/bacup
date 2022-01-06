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

use rusoto_core::credential::StaticProvider;
use rusoto_core::{HttpClient, Region};
use rusoto_s3::S3Client;
use rusoto_s3::S3;

use crate::config::AwsConfig;
use crate::remotes::remote;

use std::path::{Path, PathBuf};

use rusoto_core::credential::CredentialsError;
use rusoto_core::RusotoError;
use rusoto_s3::{DeleteObjectError, ListObjectsV2Error, PutObjectError};

use async_trait::async_trait;

use std::fmt;

#[derive(Debug)]
pub enum Error {
    InvalidCredentials(RusotoError<CredentialsError>),
    ListError(RusotoError<ListObjectsV2Error>),
    PutError(RusotoError<PutObjectError>),
    DeleteError(RusotoError<DeleteObjectError>),
}

impl From<RusotoError<CredentialsError>> for Error {
    fn from(error: RusotoError<CredentialsError>) -> Self {
        Error::InvalidCredentials(error)
    }
}

impl From<RusotoError<ListObjectsV2Error>> for Error {
    fn from(error: RusotoError<ListObjectsV2Error>) -> Self {
        Error::ListError(error)
    }
}

impl From<RusotoError<PutObjectError>> for Error {
    fn from(error: RusotoError<PutObjectError>) -> Self {
        Error::PutError(error)
    }
}

impl From<RusotoError<DeleteObjectError>> for Error {
    fn from(error: RusotoError<DeleteObjectError>) -> Self {
        Error::DeleteError(error)
    }
}

impl std::error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidCredentials(error) => write!(f, "Invalid credentials: {}", error),
            Error::ListError(error) => write!(f, "AWS List V2 error error: {}", error),
            Error::PutError(error) => write!(f, "AWS Put object error: {}", error),
            Error::DeleteError(error) => write!(f, "AWS delete object error: {}", error),
        }
    }
}

#[derive(Clone)]
pub struct AwsBucket {
    name: String,
    bucket: Bucket,
}

#[derive(Clone)]
struct Bucket {
    client: S3Client,
    bucket_name: String,
}

impl Bucket {
    pub async fn list(&self, prefix: &str) -> Result<Vec<String>, Error> {
        let response = self
            .client
            .list_objects_v2(rusoto_s3::ListObjectsV2Request {
                bucket: self.bucket_name.to_owned(),
                prefix: Some(prefix.trim_start_matches('/').to_owned()),
                ..Default::default()
            })
            .await?;
        let mut ret: Vec<String> = vec![];
        for res in response.contents.iter() {
            for object in res {
                ret.push(object.key.as_ref().unwrap().to_owned());
            }
        }
        Ok(ret)
    }

    pub async fn put_object(&self, remote_path: &str, content: Vec<u8>) -> Result<(), Error> {
        self.client
            .put_object(rusoto_s3::PutObjectRequest {
                bucket: self.bucket_name.clone(),
                key: remote_path.trim_start_matches('/').to_owned(),
                body: Some(content.into()),
                ..Default::default()
            })
            .await?;
        Ok(())
    }

    pub async fn delete(&self, remote_path: &str) -> Result<(), Error> {
        self.client
            .delete_object(rusoto_s3::DeleteObjectRequest {
                bucket: self.bucket_name.clone(),
                key: remote_path.to_owned(),
                ..Default::default()
            })
            .await?;
        Ok(())
    }
}

impl AwsBucket {
    pub async fn new(config: AwsConfig, bucket_name: &str) -> Result<AwsBucket, Error> {
        let region: Region = config.region.parse::<Region>().unwrap();
        let bucket = Bucket {
            client: S3Client::new_with(
                HttpClient::new().unwrap(),
                StaticProvider::new(config.access_key, config.secret_key, None, None),
                region,
            ),
            bucket_name: bucket_name.to_owned(),
        };

        // Perform a listing request to check if the configuration is ok
        bucket.list("").await?;
        Ok(AwsBucket {
            name: String::from(bucket_name),
            bucket,
        })
    }
}

#[async_trait]
impl remote::Remote for AwsBucket {
    fn name(&self) -> String {
        self.name.clone()
    }

    async fn enumerate(&self, remote_path: &Path) -> Result<Vec<String>, remote::Error> {
        let ret = self.bucket.list(remote_path.to_str().unwrap()).await?;
        Ok(ret)
    }

    async fn delete(&self, remote_path: &Path) -> Result<(), remote::Error> {
        self.bucket.delete(remote_path.to_str().unwrap()).await?;
        Ok(())
    }

    async fn upload_file(&self, path: &Path, remote_path: &Path) -> Result<(), remote::Error> {
        use tokio::fs::File;
        use tokio::io::AsyncReadExt;

        let mut content: Vec<u8> = vec![];
        let mut file = File::open(path).await?;
        file.read_to_end(&mut content).await?;

        let remote_path = remote_path.to_str().unwrap();
        self.bucket.put_object(remote_path, content).await?;
        Ok(())
    }

    async fn upload_file_compressed(
        &self,
        path: &Path,
        remote_path: &Path,
    ) -> Result<(), remote::Error> {
        let compressed_bytes = self.compress_file(path)?;
        let remote_path = self.remote_compressed_file_path(remote_path);
        self.bucket
            .put_object(remote_path.to_str().unwrap(), compressed_bytes)
            .await?;
        Ok(())
    }

    async fn upload_folder(
        &self,
        paths: &[PathBuf],
        remote_path: &Path,
    ) -> Result<(), remote::Error> {
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
    ) -> Result<(), remote::Error> {
        if !path.is_dir() {
            return Err(remote::Error::NotADirectory);
        }

        let remote_path = self.remote_archive_path(remote_path);
        let compressed_folder = self.compress_folder(path)?;
        self.upload_file(compressed_folder.path(), &remote_path)
            .await?;
        Ok(())
    }
}
