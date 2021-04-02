use std::fmt;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::string::String;
use std::vec::Vec;

use crate::config::PostgreSQLConfig;
use crate::services::service::{Dump, Service};

use which::which;

#[derive(Clone)]
pub struct PostgreSQL {
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

impl PostgreSQL {
    pub fn new(config: PostgreSQLConfig, name: &str) -> Result<PostgreSQL, Error> {
        let username = &config.username;
        let db_name = &config.db_name;
        let host = &config.host.unwrap_or(String::from("localhost"));
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

        let status = Command::new(cmd).args(&args).stdout(Stdio::null()).status();
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

        let output = match Command::new(cmd).args(&args).output() {
            Err(error) => return Err(Error::RuntimeError(error)),
            Ok(output) => output,
        };

        let stderr = std::str::from_utf8(&output.stderr).unwrap().trim();
        if stderr.len() > 0 {
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

        Ok(PostgreSQL {
            name: String::from(name),
            username: String::from(username),
            db_name: String::from(db_name),
            args: args.iter().map(|s| s.to_string()).collect(),
            cmd,
            dumped_to: PathBuf::new(),
        })
    }
}

impl Service for PostgreSQL {
    fn list(&self) -> Vec<PathBuf> {
        if self.dumped_to != PathBuf::new() {
            return vec![self.dumped_to.clone()];
        }
        return vec![];
    }

    fn dump(&mut self) -> Result<Dump, Box<dyn std::error::Error>> {
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
        {
            Ok(_) => {
                self.dumped_to = dest.clone();
                return Ok(Dump { path: Some(dest) });
            }
            Err(error) => {
                return Err(Error::RuntimeError(error).into());
            }
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

    struct Dump {
        path: PathBuf,
    }

    impl Drop for Dump {
        fn drop(&mut self) {
            #[allow(unused_must_use)]
            {
                std::fs::remove_file(&self.path);
            }
        }
    }

    #[test]
    fn test_new_connection_ok() {
        let config = PostgreSQLConfig {
            username: String::from(USERNAME),
            db_name: String::from(DB_NAME),
            host: Some(String::from(HOST)),
            port: Some(PORT),
        };
        assert!(PostgreSQL::new(config, NAME).is_ok());
    }

    #[test]
    fn test_new_connection_fail_username() {
        let config = PostgreSQLConfig {
            username: String::from("wat"),
            db_name: String::from(DB_NAME),
            host: Some(String::from(HOST)),
            port: Some(PORT),
        };
        assert!(PostgreSQL::new(config, NAME).is_err());
    }

    #[test]
    fn test_new_connection_fail_db_name() {
        let config = PostgreSQLConfig {
            username: String::from(USERNAME),
            db_name: String::from("wat"),
            host: Some(String::from(HOST)),
            port: Some(PORT),
        };
        assert!(PostgreSQL::new(config, NAME).is_err());
    }

    #[test]
    fn test_new_connection_fail_host() {
        let config = PostgreSQLConfig {
            username: String::from(USERNAME),
            db_name: String::from(DB_NAME),
            host: Some(String::from("wat")),
            port: Some(PORT),
        };
        assert!(PostgreSQL::new(config, NAME).is_err());
    }

    #[test]
    fn test_new_connection_fail_port() {
        let config = PostgreSQLConfig {
            username: String::from(USERNAME),
            db_name: String::from(DB_NAME),
            host: Some(String::from(HOST)),
            port: Some(69),
        };
        assert!(PostgreSQL::new(config, NAME).is_err());
    }

    #[test]
    fn test_dump_success() {
        let config = PostgreSQLConfig {
            username: String::from(USERNAME),
            db_name: String::from(DB_NAME),
            host: Some(String::from(HOST)),
            port: Some(PORT),
        };

        let mut db = PostgreSQL::new(config, NAME).unwrap();
        assert!(db.dump().is_ok());
    }
}
