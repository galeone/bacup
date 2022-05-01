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

use glob::glob;
use std::fmt;
use std::path::{Path, PathBuf};

use async_trait::async_trait;

use tokio::fs;

use crate::services::service::{Dump, Service};

#[derive(Clone)]
pub struct Folder {
    pattern: String,
}

#[derive(Debug, PartialEq)]
pub enum Error {
    IsNotAbsolute(PathBuf),
    DoesNotExist(PathBuf),
}

impl std::error::Error for Error {}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::IsNotAbsolute(path) => write!(f, "Path {} is not absolute", path.display()),
            Error::DoesNotExist(path) => write!(f, "Path {} does not exist", path.display()),
        }
    }
}

impl Folder {
    pub async fn new(pattern: &str) -> Result<Folder, Error> {
        for token in &["*", "?", "["] {
            if pattern.contains(token) {
                let base_path = pattern.split(token).next().unwrap();
                let base_path = Path::new(base_path);

                if !base_path.is_absolute() {
                    return Err(Error::IsNotAbsolute(PathBuf::from(base_path)));
                }

                if !base_path.exists() {
                    return Err(Error::DoesNotExist(PathBuf::from(base_path)));
                }

                return Ok(Folder {
                    pattern: String::from(pattern),
                });
            }
        }

        // Single file backup support - not a pattern nor a folder.
        if let Ok(meta) = fs::metadata(pattern).await {
            if meta.is_file() {
                return Ok(Folder {
                    pattern: String::from(pattern),
                });
            }
        }
        let path = Path::new(pattern);
        if !path.is_absolute() {
            return Err(Error::IsNotAbsolute(PathBuf::from(path)));
        }
        if !path.exists() {
            return Err(Error::DoesNotExist(PathBuf::from(path)));
        }
        return Ok(Folder {
            pattern: String::from(path.join("**").join("*").to_str().unwrap()),
        });
    }
}

#[async_trait]
impl Service for Folder {
    async fn list(&self) -> Vec<PathBuf> {
        glob(&self.pattern)
            .unwrap()
            .map(|pb_ge| pb_ge.unwrap())
            .collect::<Vec<PathBuf>>()
    }

    async fn dump(&self) -> Result<Dump, Box<dyn std::error::Error>> {
        Ok(Dump { path: None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[tokio::test]
    async fn test_new_relative() {
        let relative = "relative";
        assert!(Folder::new(relative).await.is_err());
        assert_eq!(
            Folder::new(relative).await.err(),
            Some(Error::IsNotAbsolute(PathBuf::from(relative)))
        );
    }

    #[tokio::test]
    async fn test_new_absolute_folder() {
        let cwd = env::current_dir().unwrap();
        assert!(Folder::new(cwd.to_str().unwrap()).await.is_ok());
    }

    #[tokio::test]
    async fn test_new_absolute_file() {
        let cwd = env::current_dir().unwrap().join("Cargo.toml");
        assert!(Folder::new(cwd.to_str().unwrap()).await.is_ok());
    }

    #[tokio::test]
    async fn test_dump_and_list_no_wildcard_folder() {
        let cwd = env::current_dir().unwrap();
        let folder = Folder::new(cwd.to_str().unwrap()).await;
        assert!(folder.is_ok());
        let folder = folder.unwrap();

        // Dump -> evaluate the pattern
        assert!(folder.dump().await.is_ok());

        let files = folder.list().await;
        assert!(files.len() > 0);

        let git_info = cwd.join(".git").join("info");
        assert!(files.contains(&git_info));

        let cargo = cwd.join("LICENSE");
        assert!(files.contains(&cargo));
    }

    #[tokio::test]
    async fn test_dump_and_list_no_wildcard_single_file() {
        let cwd = env::current_dir().unwrap();
        let file = cwd.join("Cargo.toml");

        let folder = Folder::new(file.to_str().unwrap()).await;
        assert!(folder.is_ok());
        let folder = folder.unwrap();

        // Dump -> evaluate the pattern
        assert!(folder.dump().await.is_ok());

        let files = folder.list().await;
        assert!(files.len() == 1);
    }

    #[tokio::test]
    async fn test_dump_and_list_wildcard() {
        let cwd = env::current_dir().unwrap();
        let folder = Folder::new(cwd.join("src").join("*").to_str().unwrap()).await;
        assert!(folder.is_ok());
        let folder = folder.unwrap();

        // Dump -> evaluate the pattern
        assert!(folder.dump().await.is_ok());

        let files = folder.list().await;
        assert!(files.len() > 0);

        let lib_path = cwd.join("src").join("lib.rs");
        assert!(files.contains(&lib_path));
    }

    #[tokio::test]
    async fn test_non_existing_abolute() {
        let cwd = env::current_dir()
            .unwrap()
            .parent()
            .unwrap()
            .join("fakfakefakefake");
        assert!(Folder::new(cwd.to_str().unwrap()).await.is_err());
    }
}
