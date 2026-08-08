// Copyright 2022-2026 Paolo Galeone <nessuno@nerdz.eu>
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

use aws_credential_types::provider::SharedCredentialsProvider;
use aws_sdk_s3::primitives::{ByteStream, Length};
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use aws_sdk_s3::Client;
use aws_types::region::Region;

use crate::config::AwsConfig;
use crate::remotes::remote;

use std::path::{Path, PathBuf};

use tokio::fs::File;
use tokio::io::AsyncReadExt;

use async_trait::async_trait;

use std::io;

#[derive(Clone)]
pub struct AwsBucket {
    name: String,
    bucket: Bucket,
}

#[derive(Clone)]
struct Bucket {
    client: Client,
    bucket_name: String,
}

#[derive(Debug)]
pub enum AwsError {
    RemoteError(aws_sdk_s3::Error),
    LocalError(io::Error),
    GenericError(String),
}

impl From<io::Error> for AwsError {
    fn from(err: io::Error) -> Self {
        AwsError::LocalError(err)
    }
}

impl From<aws_sdk_s3::Error> for AwsError {
    fn from(err: aws_sdk_s3::Error) -> Self {
        AwsError::RemoteError(err)
    }
}

impl Bucket {
    pub async fn list(&self, prefix: &str) -> Result<Vec<String>, AwsError> {
        let response = self
            .client
            .list_objects_v2()
            .bucket(&self.bucket_name)
            .prefix(prefix.trim_start_matches('/'))
            .send()
            .await;
        if response.is_err() {
            return Err(AwsError::RemoteError(response.err().unwrap().into()));
        }
        let response = response.unwrap();
        let mut ret: Vec<String> = vec![];
        for res in response.contents.iter() {
            for object in res {
                ret.push(object.key.as_ref().unwrap().to_owned());
            }
        }
        Ok(ret)
    }

    pub async fn put_object(&self, remote_path: &str, path: &Path) -> Result<(), AwsError> {
        // https://docs.aws.amazon.com/AmazonS3/latest/userguide/qfacts.html
        use get_file_size::GetFileSize;
        const CHUNK_SIZE: u64 = 1024 * 1024 * 1024; // 1024 MiB
        const MAX_CHUNKS: u64 = 10000; // 1024 * 10000 ~= 9.7 TiB max

        let file_size = path.file_size().await.unwrap_or_default();

        let remote_path = remote_path.trim_start_matches('/');

        if file_size <= CHUNK_SIZE {
            // Just read the file and upload to bytes.
            // Suppose CHUNK_SIZE free memory available.
            let mut content: Vec<u8> = vec![];
            let mut file = File::open(path).await?;
            file.read_to_end(&mut content).await?;
            let response = self
                .client
                .put_object()
                .bucket(&self.bucket_name)
                .key(remote_path)
                .body(ByteStream::from(content))
                .send()
                .await;

            if response.is_err() {
                return Err(AwsError::RemoteError(response.err().unwrap().into()));
            }
        } else {
            // Multipart upload

            let multipart_upload_res = self
                .client
                .create_multipart_upload()
                .bucket(&self.bucket_name)
                .key(remote_path)
                .send()
                .await;
            if multipart_upload_res.is_err() {
                return Err(AwsError::RemoteError(
                    multipart_upload_res.err().unwrap().into(),
                ));
            }
            let multipart_upload_res = multipart_upload_res.unwrap();

            let upload_id = multipart_upload_res
                .upload_id()
                .ok_or(AwsError::GenericError(
                    "Missing upload_id after CreateMultipartUpload".to_string(),
                ))?;

            let mut chunk_count = (file_size / CHUNK_SIZE) + 1;
            let mut size_of_last_chunk = file_size % CHUNK_SIZE;
            if size_of_last_chunk == 0 {
                size_of_last_chunk = CHUNK_SIZE;
                chunk_count -= 1;
            }

            if chunk_count > MAX_CHUNKS {
                return Err(AwsError::GenericError(format!(
                    "Too many chunks: {} > {}",
                    chunk_count, MAX_CHUNKS
                )));
            }

            let mut upload_parts: Vec<aws_sdk_s3::types::CompletedPart> = Vec::new();

            for chunk_index in 0..chunk_count {
                let this_chunk = if chunk_count - 1 == chunk_index {
                    size_of_last_chunk
                } else {
                    CHUNK_SIZE
                };
                let stream = ByteStream::read_from()
                    .path(path)
                    .offset(chunk_index * CHUNK_SIZE)
                    .length(Length::Exact(this_chunk))
                    .build()
                    .await
                    .unwrap();

                // Chunk index needs to start at 0, but part numbers start at 1.
                let part_number = (chunk_index as i32) + 1;
                let upload_part_res = self
                    .client
                    .upload_part()
                    .key(remote_path)
                    .bucket(&self.bucket_name)
                    .upload_id(upload_id)
                    .body(stream)
                    .part_number(part_number)
                    .send()
                    .await;

                if upload_part_res.is_err() {
                    return Err(AwsError::RemoteError(
                        upload_part_res.err().unwrap().into_service_error().into(),
                    ));
                }
                let upload_part_res = upload_part_res.unwrap();

                upload_parts.push(
                    CompletedPart::builder()
                        .e_tag(upload_part_res.e_tag.unwrap_or_default())
                        .part_number(part_number)
                        .build(),
                );
            }

            let completed_multipart_upload = CompletedMultipartUpload::builder()
                .set_parts(Some(upload_parts))
                .build();

            let complete_multipart_upload_res = self
                .client
                .complete_multipart_upload()
                .bucket(&self.bucket_name)
                .key(remote_path)
                .multipart_upload(completed_multipart_upload)
                .upload_id(upload_id)
                .send()
                .await;
            if complete_multipart_upload_res.is_err() {
                return Err(AwsError::RemoteError(
                    complete_multipart_upload_res.err().unwrap().into(),
                ));
            }
        }
        Ok(())
    }

    pub async fn delete(&self, remote_path: &str) -> Result<(), AwsError> {
        let response = self
            .client
            .delete_object()
            .bucket(&self.bucket_name)
            .key(remote_path)
            .send()
            .await;

        if response.is_err() {
            return Err(AwsError::RemoteError(response.err().unwrap().into()));
        }

        Ok(())
    }
}

impl AwsBucket {
    pub async fn new(config: AwsConfig, bucket_name: &str) -> Result<AwsBucket, AwsError> {
        let region = Region::new(config.region);
        let mut builder =
            aws_config::defaults(aws_config::BehaviorVersion::latest()).region(region);
        if let Some(endpoint) = &config.endpoint {
            builder = builder.endpoint_url(endpoint);
        }
        let sdk_config = builder
            .credentials_provider(SharedCredentialsProvider::new(
                aws_credential_types::Credentials::from_keys(
                    config.access_key,
                    config.secret_key,
                    None,
                ),
            ))
            .load()
            .await;

        let mut conf_builder = aws_sdk_s3::config::Builder::from(&sdk_config);
        conf_builder.set_force_path_style(config.force_path_style);
        let client = Client::from_conf(conf_builder.build());
        let bucket = Bucket {
            client,
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
        self.bucket
            .put_object(remote_path.to_str().unwrap(), path)
            .await?;
        Ok(())
    }

    async fn upload_file_compressed(
        &self,
        path: &Path,
        remote_path: &Path,
    ) -> Result<(), remote::Error> {
        let compressed_file = self.compress_file(path).await?;
        let remote_path = self.remote_compressed_file_path(remote_path);
        self.bucket
            .put_object(remote_path.to_str().unwrap(), compressed_file.path())
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
        let compressed_folder = self.compress_folder(path).await?;
        self.upload_file(compressed_folder.path(), &remote_path)
            .await?;
        Ok(())
    }
}
