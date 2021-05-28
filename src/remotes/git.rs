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

use crate::config::{GitConfig, SshConfig};
use crate::remotes::ssh;
use crate::remotes::uploader;

use std::fs::File;

use std::io;
use std::io::Write;

use std::path::{Path, PathBuf};

use std::fmt;
use std::string::String;

use which::which;

use async_trait::async_trait;

use scopeguard::defer;

use std::process::Command;

#[derive(Debug)]
pub enum Error {
    InvalidPrivateKey(String),
    CommandNotFound(which::Error),
    RuntimeError(io::Error),
    DoesNotExist(PathBuf),
}

impl From<which::Error> for Error {
    fn from(error: which::Error) -> Self {
        Error::CommandNotFound(error)
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Error::RuntimeError(error)
    }
}

impl From<ssh::Error> for Error {
    fn from(error: ssh::Error) -> Self {
        match error {
            ssh::Error::CommandNotFound(e) => Error::CommandNotFound(e),
            ssh::Error::InvalidPrivateKey(e) => Error::InvalidPrivateKey(e),
            ssh::Error::RuntimeError(e) => Error::RuntimeError(e),
        }
    }
}

impl std::error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::CommandNotFound(ref error) => write!(f, "Command not found: {}", error),
            Error::InvalidPrivateKey(ref msg) => write!(f, "Invalid private key: {}", msg),
            Error::RuntimeError(ref error) => write!(f, "Error while reading/writing: {}", error),
            Error::DoesNotExist(ref path) => write!(f, "Path {} does not exist", path.display()),
        }
    }
}

impl From<Error> for uploader::Error {
    fn from(error: Error) -> Self {
        match error {
            Error::CommandNotFound(error) => {
                uploader::Error::LocalError(std::io::Error::new(std::io::ErrorKind::Other, error))
            }
            Error::InvalidPrivateKey(msg) => {
                uploader::Error::LocalError(std::io::Error::new(std::io::ErrorKind::Other, msg))
            }
            Error::RuntimeError(error) => {
                uploader::Error::LocalError(std::io::Error::new(std::io::ErrorKind::Other, error))
            }
            Error::DoesNotExist(path) => uploader::Error::LocalError(std::io::Error::new(
                std::io::ErrorKind::Other,
                path.to_str().unwrap(),
            )),
        }
    }
}

#[derive(Clone)]
pub struct Git {
    pub remote_name: String,
    pub config: GitConfig,
    pub git_cmd: PathBuf,
}

impl Git {
    pub fn new(config: GitConfig, remote_name: &str) -> Result<Git, Error> {
        // Instantiate an ssh remote that will check for us the validity of
        // all the ssh parameters
        let ssh_config = SshConfig {
            host: config.host.clone(),
            port: config.port,
            private_key: config.private_key.clone(),
            username: config.username.clone(),
        };
        ssh::Ssh::new(ssh_config, remote_name)?;

        let git_cmd = which("git")?;
        Ok(Git {
            remote_name: String::from(remote_name),
            config,
            git_cmd,
        })
    }

    fn clone_repository(&self) -> Result<PathBuf, Error> {
        let dest = PathBuf::from(&self.config.repository.split('/').last().unwrap());
        if dest.exists() {
            let git_repo = dest.join(".git");
            if git_repo.exists() && git_repo.is_dir() {
                return Ok(dest);
            }
        }
        let url = format!(
            "ssh://{}@{}:{}/{}",
            &self.config.username, &self.config.host, &self.config.port, &self.config.repository
        );

        let status = Command::new(&self.git_cmd)
            .args(&["clone", &url, "--depth", "1"])
            .status()?;
        if !status.success() {
            return Err(Error::RuntimeError(io::Error::new(
                io::ErrorKind::Other,
                format!(
                    "Unable to execute {} clone {} --depth 1",
                    self.git_cmd.display(),
                    &url
                ),
            )));
        }

        let dest = PathBuf::from(&self.config.repository.split('/').last().unwrap());
        if !dest.exists() {
            return Err(Error::DoesNotExist(dest));
        }
        Ok(dest)
    }
}

#[async_trait]
impl uploader::Uploader for Git {
    fn name(&self) -> String {
        self.remote_name.clone()
    }

    async fn upload_file(&self, path: &Path, remote_path: &Path) -> Result<(), uploader::Error> {
        let repo = self.clone_repository()?;

        // cp file <repo_location>/[<subdir>]
        let dest = repo.join(remote_path.strip_prefix("/").unwrap());
        if !dest.exists() {
            std::fs::create_dir_all(&dest).unwrap();
        }
        std::fs::copy(path, dest.join(path.file_name().unwrap()))?;

        // cd <repo path>
        let cwd = std::env::current_dir()?;
        defer! {
            #[allow(unused_must_use)] {
            std::env::set_current_dir(cwd);
            }
        }
        std::env::set_current_dir(&dest)?;

        // git switch -c branch (ignore failures - we might be in the branch already)
        Command::new(&self.git_cmd)
            .args(&["switch", "-c", &self.config.branch])
            .status()?;

        // git pull origin branch (ignore failures)
        Command::new(&self.git_cmd)
            .args(&["pull", "origin", &self.config.branch])
            .status()?;

        // git add . -A
        let status = Command::new(&self.git_cmd)
            .args(&["add", ".", "-A"])
            .status()?;
        if !status.success() {
            return Err(uploader::Error::LocalError(io::Error::new(
                io::ErrorKind::Other,
                format!("Unable to execute git add . -A into {}", dest.display()),
            )));
        }
        // git commit -m '[bacup] snapshot'
        let status = Command::new(&self.git_cmd)
            .args(&["commit", "-m", "[bacup] snapshot"])
            .status()?;
        if !status.success() {
            return Err(uploader::Error::LocalError(io::Error::new(
                io::ErrorKind::Other,
                format!(
                    "Unable to execute git commit -m [bacup] snapshot into {}",
                    dest.display()
                ),
            )));
        }
        // git push origin <branch>
        let status = Command::new(&self.git_cmd)
            .args(&["push", "origin", &self.config.branch])
            .status()?;
        if !status.success() {
            return Err(uploader::Error::LocalError(io::Error::new(
                io::ErrorKind::Other,
                format!("Unable to execute git add . -A into {}", dest.display()),
            )));
        }
        Ok(())
    }

    async fn upload_file_compressed(
        &self,
        path: &Path,
        remote_path: &Path,
    ) -> Result<(), uploader::Error> {
        // Read and compress
        let compressed_bytes = self.compress_file(path)?;
        let remote_path = self.remote_compressed_file_path(remote_path);

        let mut buffer = File::create(&remote_path)?;
        buffer.write_all(&compressed_bytes)?;

        defer! {
            #[allow(unused_must_use)]
            {
                std::fs::remove_file(&remote_path);
            }
        }
        self.upload_file(&remote_path, &remote_path).await?;
        Ok(())
    }

    async fn upload_folder(
        &self,
        paths: &[PathBuf],
        remote_path: &Path,
    ) -> Result<(), uploader::Error> {
        let repo = self.clone_repository()?;

        // cp file <repo_location>/[<subdir>]
        let dest = repo.join(remote_path.strip_prefix("/").unwrap());
        if !dest.exists() {
            std::fs::create_dir_all(&dest).unwrap();
        }
        let git_folder = std::path::Component::Normal(".git".as_ref());
        for path in paths.iter() {
            // Skip .git and content of this folder
            if path.components().any(|x| x == git_folder) {
                continue;
            }
            if path.is_dir() {
                std::fs::create_dir_all(dest.join(path.file_name().unwrap()))?;
            } else {
                std::fs::copy(path, dest.join(path.file_name().unwrap()))?;
            }
        }

        // cd <repo path>
        let cwd = std::env::current_dir()?;
        defer! {
            #[allow(unused_must_use)] {
            std::env::set_current_dir(cwd);
            }
        }
        std::env::set_current_dir(&dest)?;

        // git switch -c branch (ignore failures - we might be in the branch already)
        Command::new(&self.git_cmd)
            .args(&["switch", "-c", &self.config.branch])
            .status()?;

        // git pull origin branch (ignore failures)
        Command::new(&self.git_cmd)
            .args(&["pull", "origin", &self.config.branch])
            .status()?;

        // git add . -A
        let status = Command::new(&self.git_cmd)
            .args(&["add", ".", "-A"])
            .status()?;
        if !status.success() {
            return Err(uploader::Error::LocalError(io::Error::new(
                io::ErrorKind::Other,
                format!("Unable to execute git add . -A into {}", dest.display()),
            )));
        }
        // git commit -m '[bacup] snapshot'
        let status = Command::new(&self.git_cmd)
            .args(&["commit", "-m", "[bacup] snapshot"])
            .status()?;
        if !status.success() {
            return Err(uploader::Error::LocalError(io::Error::new(
                io::ErrorKind::Other,
                format!(
                    "Unable to execute git commit -m [bacup] snapshot into {}",
                    dest.display()
                ),
            )));
        }
        // git push origin <branch>
        let status = Command::new(&self.git_cmd)
            .args(&["push", "origin", &self.config.branch])
            .status()?;
        if !status.success() {
            return Err(uploader::Error::LocalError(io::Error::new(
                io::ErrorKind::Other,
                format!("Unable to execute git add . -A into {}", dest.display()),
            )));
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
            .await
    }
}
