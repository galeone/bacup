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

use std::fmt;
use std::path::PathBuf;
use std::string::String;
use std::vec::Vec;

use crate::config::DockerConfig;
use crate::services::service::{Dump, Service};

use which::which;

use async_trait::async_trait;
use tokio::{fs::metadata, fs::File, io};

use std::process::Stdio;
use tokio::process::Command;

#[derive(Clone)]
pub struct Docker {
    pub name: String,
    pub cmd: PathBuf,
    pub args: Vec<String>,
}

#[derive(Debug)]
pub enum Error {
    CommandNotFound(which::Error),
    RuntimeError(io::Error),
}

impl std::error::Error for Error {}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::CommandNotFound(error) => write!(f, "Command not found: {}", error),
            Error::RuntimeError(error) => write!(f, "Runtime error: {}", error),
        }
    }
}

impl Docker {
    pub async fn new(config: DockerConfig, name: &str) -> Result<Docker, Error> {
        let cmd = match which("docker") {
            Err(error) => return Err(Error::CommandNotFound(error)),
            Ok(cmd) => cmd,
        };

        let args = vec!["run", "--rm", "hello-world"];
        let status = Command::new(&cmd)
            .args(&args)
            .stdout(Stdio::null())
            .status()
            .await;
        if status.is_err() {
            return Err(Error::RuntimeError(status.err().unwrap()));
        }
        let code = status.unwrap().code().unwrap();
        if code != 0 {
            return Err(Error::RuntimeError(io::Error::other(format!(
                "docker run hello-world failed, exit code {}",
                code
            ))));
        }

        let mut args: Vec<String> = vec![
            String::from("exec"),
            String::from("-t"),
            config.container_name,
        ];
        let split_command: Vec<String> = config
            .command
            .split_whitespace()
            .map(String::from)
            .collect();
        args.extend(split_command);

        Ok(Docker {
            name: String::from(name),
            args,
            cmd,
        })
    }
}

#[async_trait]
impl Service for Docker {
    async fn list(&self) -> Vec<PathBuf> {
        let dest = std::env::current_dir()
            .unwrap()
            .join(PathBuf::from(format!("{}.dump", self.name)));

        if metadata(&dest).await.is_ok() {
            return vec![dest];
        }
        return vec![];
    }

    async fn dump(&self) -> Result<Dump, Box<dyn std::error::Error>> {
        let dest = std::env::current_dir()
            .unwrap()
            .join(PathBuf::from(format!("{}.dump", self.name)));
        let parent = dest.parent().unwrap();
        if !parent.exists() {
            return Err(Error::RuntimeError(io::Error::other(format!(
                "Folder {} does not exist.",
                parent.display()
            )))
            .into());
        }

        let dest_file = File::create(&dest).await?;

        match Command::new(&self.cmd)
            .args(&self.args)
            .stdout(Stdio::from(dest_file.try_into_std().unwrap()))
            .status()
            .await
        {
            Ok(_) => Ok(Dump { path: Some(dest) }),
            Err(error) => Err(Error::RuntimeError(error).into()),
        }
    }
}
