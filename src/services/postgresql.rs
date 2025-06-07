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

use std::{fmt, path::PathBuf, process::Stdio, string::String, vec::Vec};
use tokio::process::Command;

use async_trait::async_trait;
use which::which;

use tokio::{fs::metadata, io};

use crate::config::PostgreSqlConfig;
use crate::services::service::{Dump, Service};

#[derive(Clone)]
pub struct PostgreSql {
    pub name: String,
    pub username: String,
    pub db_name: String,
    pub cmd: PathBuf,
    pub args: Vec<String>,
    pub dumped_to: PathBuf,
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

impl PostgreSql {
    pub async fn new(config: PostgreSqlConfig, name: &str) -> Result<PostgreSql, Error> {
        let username = &config.username;
        let db_name = &config.db_name;
        let host = &config.host.unwrap_or_else(|| String::from("localhost"));
        let port = config.port.unwrap_or(5432);
        let cmd = match which("pg_isready") {
            Err(error) => return Err(Error::CommandNotFound(error)),
            Ok(cmd) => cmd,
        };

        let port = format!("{}", port);
        let mut args = vec![
            "--host",
            host,
            "--port",
            &port,
            "--username",
            username,
            "--dbname",
            db_name,
        ];

        let status = Command::new(cmd)
            .args(&args)
            .stdout(Stdio::null())
            .status()
            .await;
        if status.is_err() {
            return Err(Error::RuntimeError(status.err().unwrap()));
        }
        let code = status.unwrap().code().unwrap();
        if code != 0 {
            return Err(Error::RuntimeError(io::Error::new(
                io::ErrorKind::Other,
                format!("pg_isready failed, exit code {}", code),
            )));
        }

        // Find psql and use it to check if the db exists and we can connect with
        // the provided credentials
        let cmd = match which("psql") {
            Err(error) => return Err(Error::CommandNotFound(error)),
            Ok(cmd) => cmd,
        };

        args.push("-tAc");
        let query = format!(r#"SELECT 1 FROM pg_database WHERE datname='{}'"#, db_name);
        args.push(&query);

        let output = match Command::new(cmd).args(&args).output().await {
            Err(error) => return Err(Error::RuntimeError(error)),
            Ok(output) => output,
        };

        let stderr = std::str::from_utf8(&output.stderr).unwrap().trim();
        if !stderr.is_empty() {
            return Err(Error::RuntimeError(io::Error::new(
                io::ErrorKind::Other,
                stderr,
            )));
        }

        let stdout = std::str::from_utf8(&output.stdout).unwrap().trim();
        if stdout == "0" {
            return Err(Error::RuntimeError(io::Error::new(
                io::ErrorKind::Other,
                format!(
                    "database {} does not exit or user {} not allowed to query the db",
                    db_name, username
                ),
            )));
        }
        // Remove the specific arguments for the psql invocation
        args.pop();
        args.pop();

        // All the database dumps shuld be performend without aksing for password
        args.push("--no-password");

        let cmd = match which("pg_dump") {
            Err(error) => return Err(Error::CommandNotFound(error)),
            Ok(cmd) => cmd,
        };

        Ok(PostgreSql {
            name: String::from(name),
            username: String::from(username),
            db_name: String::from(db_name),
            args: args.iter().map(|s| s.to_string()).collect(),
            cmd,
            dumped_to: PathBuf::new(),
        })
    }
}

#[async_trait]
impl Service for PostgreSql {
    async fn list(&self) -> Vec<PathBuf> {
        let dest = std::env::current_dir()
            .unwrap()
            .join(PathBuf::from(format!("{}-dump.sql", self.name)));

        if metadata(&dest).await.is_ok() {
            return vec![dest];
        }
        return vec![];
    }

    async fn dump(&self) -> Result<Dump, Box<dyn std::error::Error>> {
        let dest = std::env::current_dir()
            .unwrap()
            .join(PathBuf::from(format!("{}-dump.sql", self.name)));
        let parent = dest.parent().unwrap();
        if !parent.exists() {
            return Err(Error::RuntimeError(io::Error::new(
                io::ErrorKind::Other,
                format!("Folder {} does not exist.", parent.display()),
            ))
            .into());
        }

        match Command::new(self.cmd.clone())
            .args(
                self.args
                    .iter()
                    .chain(&["-f".to_string(), dest.to_str().unwrap().to_string()]),
            )
            .status()
            .await
        {
            Ok(_) => Ok(Dump { path: Some(dest) }),
            Err(error) => Err(Error::RuntimeError(error).into()),
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    const USERNAME: &str = "postgres";
    const DB_NAME: &str = "postgres";
    const HOST: &str = "localhost";
    const PORT: u16 = 5432;
    const NAME: &str = "test_service_db";

    #[tokio::test]
    #[ignore]
    async fn test_new_connection_ok() {
        let config = PostgreSqlConfig {
            username: String::from(USERNAME),
            db_name: String::from(DB_NAME),
            host: Some(String::from(HOST)),
            port: Some(PORT),
        };
        assert!(PostgreSql::new(config, NAME).await.is_ok());
    }

    #[tokio::test]
    async fn test_new_connection_fail_username() {
        let config = PostgreSqlConfig {
            username: String::from("wat"),
            db_name: String::from(DB_NAME),
            host: Some(String::from(HOST)),
            port: Some(PORT),
        };
        assert!(PostgreSql::new(config, NAME).await.is_err());
    }

    #[tokio::test]
    async fn test_new_connection_fail_db_name() {
        let config = PostgreSqlConfig {
            username: String::from(USERNAME),
            db_name: String::from("wat"),
            host: Some(String::from(HOST)),
            port: Some(PORT),
        };
        assert!(PostgreSql::new(config, NAME).await.is_err());
    }

    #[tokio::test]
    async fn test_new_connection_fail_host() {
        let config = PostgreSqlConfig {
            username: String::from(USERNAME),
            db_name: String::from(DB_NAME),
            host: Some(String::from("wat")),
            port: Some(PORT),
        };
        assert!(PostgreSql::new(config, NAME).await.is_err());
    }

    #[tokio::test]
    async fn test_new_connection_fail_port() {
        let config = PostgreSqlConfig {
            username: String::from(USERNAME),
            db_name: String::from(DB_NAME),
            host: Some(String::from(HOST)),
            port: Some(69),
        };
        assert!(PostgreSql::new(config, NAME).await.is_err());
    }

    #[tokio::test]
    #[ignore]
    async fn test_dump_success() {
        let config = PostgreSqlConfig {
            username: String::from(USERNAME),
            db_name: String::from(DB_NAME),
            host: Some(String::from(HOST)),
            port: Some(PORT),
        };

        let db = PostgreSql::new(config, NAME).await.unwrap();
        assert!(db.dump().await.is_ok());
    }
}
