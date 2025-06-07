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

use crate::config::LocalhostConfig;
use crate::remotes::remote;

use std::io;
use std::path::{Path, PathBuf};

use async_trait::async_trait;

use std::fmt;

#[derive(Debug, PartialEq)]
pub enum Error {
    IsNotAbsolute(PathBuf),
    DoesNotExist(PathBuf),
    IsNotAFolder(PathBuf),
}

impl std::error::Error for Error {}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::IsNotAbsolute(path) => write!(f, "Path {} is not absolute", path.display()),
            Error::DoesNotExist(path) => write!(f, "Path {} does not exist", path.display()),
            Error::IsNotAFolder(path) => write!(f, "Path {} is not a folder", path.display()),
        }
    }
}

#[derive(Clone)]
pub struct Localhost {
    name: String,
    path: PathBuf,
}

impl Localhost {
    pub fn new(config: LocalhostConfig, name: &str) -> Result<Localhost, Error> {
        let path = PathBuf::from(config.path);

        if path.is_relative() {
            return Err(Error::IsNotAbsolute(path));
        }
        if !path.exists() {
            return Err(Error::DoesNotExist(path));
        }
        if !path.is_dir() {
            return Err(Error::IsNotAFolder(path));
        }

        Ok(Localhost {
            name: String::from(name),
            path,
        })
    }
}

#[async_trait]
impl remote::Remote for Localhost {
    fn name(&self) -> String {
        self.name.clone()
    }

    async fn enumerate(&self, remote_path: &Path) -> Result<Vec<String>, remote::Error> {
        use tokio::fs;

        let remote_path = if remote_path.is_absolute() {
            remote_path.strip_prefix("/").unwrap()
        } else {
            remote_path
        };

        let remote_path = self.path.join(remote_path);
        let mut paths = fs::read_dir(remote_path).await?;
        let mut ret: Vec<String> = vec![];
        while let Some(entry) = paths.next_entry().await? {
            ret.push(
                entry
                    .path()
                    .strip_prefix(&self.path)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string(),
            )
        }
        Ok(ret)
    }

    async fn delete(&self, remote_path: &Path) -> Result<(), remote::Error> {
        use tokio::fs;

        let remote_path = if remote_path.is_absolute() {
            remote_path.strip_prefix("/").unwrap()
        } else {
            remote_path
        };

        let remote_path = self.path.join(remote_path);
        if remote_path.is_dir() {
            fs::remove_dir_all(remote_path).await?;
        } else {
            fs::remove_file(remote_path).await?;
        }
        Ok(())
    }

    async fn upload_file(&self, path: &Path, remote_path: &Path) -> Result<(), remote::Error> {
        use tokio::fs;

        if !path.exists() {
            return Err(remote::Error::LocalError(io::Error::other(format!(
                "{} does not exist",
                path.display()
            ))));
        }

        let remote_path = if remote_path.is_absolute() {
            remote_path.strip_prefix("/").unwrap()
        } else {
            remote_path
        };

        let dest = self.path.join(remote_path.parent().unwrap());
        if !dest.exists() {
            fs::create_dir_all(&dest).await?;
        }
        fs::copy(path, dest.join(remote_path.file_name().unwrap())).await?;
        Ok(())
    }

    async fn upload_file_compressed(
        &self,
        path: &Path,
        remote_path: &Path,
    ) -> Result<(), remote::Error> {
        use tokio::fs;
        use tokio::io::AsyncWriteExt;

        let compressed_bytes = self.compress_file(path).await?;
        let remote_path = if remote_path.is_absolute() {
            remote_path.strip_prefix("/").unwrap()
        } else {
            remote_path
        };
        let parent = self.path.join(remote_path.parent().unwrap());
        if !parent.exists() {
            fs::create_dir_all(&parent).await?;
        }
        let remote_path = parent.join(
            self.remote_compressed_file_path(&PathBuf::from(remote_path.file_name().unwrap())),
        );

        let mut buffer = fs::File::create(remote_path).await?;
        buffer.write_all(&compressed_bytes).await?;
        Ok(())
    }

    async fn upload_folder(
        &self,
        paths: &[PathBuf],
        remote_path: &Path,
    ) -> Result<(), remote::Error> {
        use tokio::fs;

        let mut local_prefix = paths.iter().min_by(|a, b| a.cmp(b)).unwrap();
        // The local_prefix found is the shortest path inside the folder we want to backup.

        // If it is a folder, we of course don't want to consider this a prefix, but its parent.
        let single_location = paths.len() <= 1;
        let parent: PathBuf;
        if !single_location {
            parent = local_prefix.parent().unwrap().to_path_buf();
            local_prefix = &parent;
        }

        // Strip local prefix from remote paths and copy
        // Need to do this because if we join /some/location with /
        // somehow it becomes / and not /some/location/
        let remote_path_str = remote_path.to_str().unwrap();

        let remote_prefix = if remote_path_str.starts_with('/') {
            PathBuf::from(remote_path_str.trim_start_matches('/'))
        } else {
            PathBuf::from(remote_path)
        };

        for path in paths.iter() {
            if path.is_file() {
                let dest = self
                    .path
                    .join(remote_prefix.join(path.strip_prefix(local_prefix).unwrap()));
                let parent = dest.parent().unwrap();
                if !parent.exists() {
                    fs::create_dir_all(parent).await?;
                }
                fs::copy(path, dest).await?;
            }
        }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remotes::remote::Remote;

    use crate::services::folders::Folder;
    use crate::services::service::Service;

    #[tokio::test]
    async fn test_upload_file() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let config = LocalhostConfig {
            path: String::from(tmp_dir.path().to_str().unwrap()),
        };
        let localhost = Localhost::new(config, "test_service").unwrap();

        assert_eq!(localhost.name(), "test_service");

        localhost
            .upload_file(&PathBuf::from("Cargo.toml"), &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();

        assert!(tmp_dir.path().join("Cargo.toml").exists());
    }

    #[tokio::test]
    async fn test_upload_file_compressed() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let config = LocalhostConfig {
            path: String::from(tmp_dir.path().to_str().unwrap()),
        };
        let localhost = Localhost::new(config, "test_service").unwrap();

        assert_eq!(localhost.name(), "test_service");

        localhost
            .upload_file_compressed(&PathBuf::from("Cargo.toml"), &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();

        let now: chrono::DateTime<chrono::Utc> = chrono::Utc::now();
        let dest = tmp_dir
            .path()
            .join(format!("{}-Cargo.toml.gz", now.format("%Y-%m-%d-%H.%M"),));

        assert!(dest.exists());
    }

    #[tokio::test]
    async fn test_upload_folder() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let config = LocalhostConfig {
            path: String::from(tmp_dir.path().to_str().unwrap()),
        };
        let localhost = Localhost::new(config, "test_service").unwrap();

        let folder = Folder::new(
            std::env::current_dir()
                .unwrap()
                .join("src")
                .to_str()
                .unwrap(),
        )
        .await
        .unwrap();
        #[allow(unused_must_use)]
        {
            // Call dump to populate the list (e.g. call ls path/**/*)
            folder.dump();
        }

        let files = folder.list().await;

        localhost
            .upload_folder(&files, &PathBuf::from("/"))
            .await
            .unwrap();

        assert!(tmp_dir.path().join("remotes").join("localhost.rs").exists());

        assert!(tmp_dir.path().join("lib.rs").exists());
    }

    #[tokio::test]
    async fn test_upload_folder_compressed() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let config = LocalhostConfig {
            path: String::from(tmp_dir.path().to_str().unwrap()),
        };
        let localhost = Localhost::new(config, "test_service").unwrap();

        let remote_filename = "remote_archive_name";
        localhost
            .upload_folder_compressed(
                &std::env::current_dir().unwrap().join("src"),
                &PathBuf::from(remote_filename),
            )
            .await
            .unwrap();

        let now: chrono::DateTime<chrono::Utc> = chrono::Utc::now();
        let dest = tmp_dir.path().join(format!(
            "{}-{}.tar.gz",
            now.format("%Y-%m-%d-%H.%M"),
            remote_filename
        ));

        assert!(dest.exists());
    }
}
