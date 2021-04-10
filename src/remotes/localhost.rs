use crate::config::LocalhostConfig;
use crate::remotes::uploader;

use std::io::prelude::*;

use std::fs;
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
impl uploader::Uploader for Localhost {
    fn name(&self) -> String {
        self.name.clone()
    }

    async fn upload_file(&self, path: &Path, remote_path: &Path) -> Result<(), uploader::Error> {
        if !path.exists() {
            return Err(uploader::Error::LocalError(io::Error::new(
                io::ErrorKind::Other,
                format!("{} does not exist", path.display()),
            )));
        }

        let remote_path = if remote_path.is_absolute() {
            remote_path.strip_prefix("/").unwrap()
        } else {
            remote_path
        };

        let dest = self.path.join(remote_path.parent().unwrap());
        if !dest.exists() {
            fs::create_dir_all(&dest)?;
        }
        fs::copy(path, dest.join(remote_path.file_name().unwrap()))?;
        Ok(())
    }

    async fn upload_file_compressed(
        &self,
        path: &Path,
        remote_path: &Path,
    ) -> Result<(), uploader::Error> {
        let compressed_bytes = self.compress_file(path)?;
        let remote_path = if remote_path.is_absolute() {
            remote_path.strip_prefix("/").unwrap()
        } else {
            remote_path
        };
        let parent = self.path.join(remote_path.parent().unwrap());
        if !parent.exists() {
            fs::create_dir_all(&parent)?;
        }
        let remote_path = parent.join(
            self.remote_compressed_file_path(&PathBuf::from(remote_path.file_name().unwrap())),
        );

        let mut buffer = fs::File::create(remote_path)?;
        buffer.write_all(&compressed_bytes)?;
        Ok(())
    }

    async fn upload_folder(
        &self,
        paths: &[PathBuf],
        remote_path: &Path,
    ) -> Result<(), uploader::Error> {
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
        let remote_prefix: PathBuf;
        // Need to do this because if we join /some/location with /
        // somehow it becomes / and not /some/location/
        let remote_path_str = remote_path.to_str().unwrap();
        if remote_path_str.starts_with('/') {
            remote_prefix = PathBuf::from(remote_path_str.trim_start_matches('/'));
        } else {
            remote_prefix = PathBuf::from(remote_path);
        }
        for path in paths.iter() {
            if path.is_file() {
                let dest = self
                    .path
                    .join(remote_prefix.join(path.strip_prefix(local_prefix).unwrap()));
                let parent = dest.parent().unwrap();
                if !parent.exists() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(path, dest)?;
            }
        }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remotes::uploader::Uploader;

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

        let mut folder = Folder::new(std::env::current_dir().unwrap().to_str().unwrap()).unwrap();
        #[allow(unused_must_use)]
        {
            // Call dump to populate the list (e.g. call ls path/**/*)
            folder.dump();
        }

        let files = folder.list();

        localhost
            .upload_folder(&files, &PathBuf::from("/"))
            .await
            .unwrap();

        assert!(tmp_dir
            .path()
            .join("src")
            .join("remotes")
            .join("localhost.rs")
            .exists());

        assert!(tmp_dir.path().join("README.md").exists());
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
