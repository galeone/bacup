use std::fmt;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::string::String;
use std::vec::Vec;

use which::which;

pub struct PostgreSQL {
    pub username: String,
    pub db_name: String,
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

impl PostgreSQL {
    pub fn new(username: &str, db_name: &str, host: &str, port: u16) -> Result<PostgreSQL, Error> {
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
            username: String::from(username),
            db_name: String::from(db_name),
            args: args.iter().map(|s| s.to_string()).collect(),
            cmd,
        })
    }

    pub fn dump(self, dest: &PathBuf) -> Result<std::process::ExitStatus, Error> {
        let parent = dest.parent().unwrap();
        if ! parent.exists() {
            return Err(Error::RuntimeError(io::Error::new(
                io::ErrorKind::Other,
                format!("Folder {} does not exist.", parent.display()),
            )));
        }
        let mut args = self.args;
        args.push("-f".to_string());
        args.push(dest.to_str().unwrap().to_string());
        match Command::new(self.cmd).args(&args).status() {
            Ok(status) => return Ok(status),
            Err(error) => return Err(Error::RuntimeError(error)),
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
        assert!(PostgreSQL::new(USERNAME, DB_NAME, HOST, PORT).is_ok());
    }

    #[test]
    fn test_new_connection_fail_username() {
        assert!(PostgreSQL::new("wat", DB_NAME, HOST, PORT).is_err());
    }

    #[test]
    fn test_new_connection_fail_db_name() {
        assert!(PostgreSQL::new(USERNAME, "WAT", HOST, PORT).is_err());
    }

    #[test]
    fn test_new_connection_fail_host() {
        assert!(PostgreSQL::new(USERNAME, DB_NAME, "WAT", PORT).is_err());
    }

    #[test]
    fn test_new_connection_fail_port() {
        assert!(PostgreSQL::new(USERNAME, DB_NAME, HOST, 69).is_err());
    }

    #[test]
    fn test_dump_success() {
        let db = PostgreSQL::new(USERNAME, DB_NAME, HOST, PORT).unwrap();
        let dest = Dump {
            path: PathBuf::from(format!(
                "{}",
                std::env::current_dir().unwrap().join("dump.sql").display()
            )),
        };

        assert!(db.dump(&dest.path).is_ok());
    }
}
