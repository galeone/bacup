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

use crate::config::SshConfig;
use crate::remotes::remote;

use std::io;
use std::io::prelude::*;
use std::io::Write;

use std::iter::once;
use std::path::{Path, PathBuf};

use std::fmt;
use std::string::String;

use log::warn;

use async_trait::async_trait;

use std::process::{Command, Stdio};
use which::which;

#[derive(Debug)]
pub enum Error {
    InvalidPrivateKey(String),
    CommandNotFound(which::Error),
    RuntimeError(io::Error),
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

impl std::error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::CommandNotFound(error) => write!(f, "Command not found: {}", error),
            Error::InvalidPrivateKey(msg) => write!(f, "Invalid private key: {}", msg),
            Error::RuntimeError(error) => write!(f, "Error while reading/writing: {}", error),
        }
    }
}

#[derive(Clone)]
pub struct Ssh {
    remote_name: String,
    config: SshConfig,
    ssh_cmd: PathBuf,
    rsync_cmd: PathBuf,
    ssh_args: Vec<String>,
}

impl Ssh {
    pub fn new(config: SshConfig, remote_name: &str) -> Result<Ssh, Error> {
        use std::fs;

        let ssh_cmd = which("ssh")?;

        let private_key = shellexpand::tilde(&config.private_key).to_string();
        let private_key = PathBuf::from(private_key);
        if !private_key.exists() {
            return Err(Error::InvalidPrivateKey(format!(
                "Private key {} does not exist.",
                private_key.display(),
            )));
        }
        let private_key_file = fs::read_to_string(&private_key)?;

        if private_key_file.contains("Proc-Type") && private_key_file.contains("ENCRYPTED") {
            return Err(Error::InvalidPrivateKey(format!(
                "Private key {} is encrypted with a passphrase. \
                            A key without passphrase is required",
                private_key.display()
            )));
        }

        let port = format!("{}", config.port);
        let host = format!("{}@{}", config.username, config.host);
        let mut args = vec![format!("-p{}", port), host, String::from("true")];

        let output = Command::new(&ssh_cmd).args(&args).output();
        if output.is_err() {
            return Err(Error::RuntimeError(io::Error::new(
                io::ErrorKind::Other,
                format!(
                    "ssh connection to {}@{}:{} failed with error: {}",
                    config.username,
                    config.host,
                    config.port,
                    output.err().unwrap(),
                ),
            )));
        }

        let output = output.unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();

        if stdout.is_empty() && stderr.contains("true") {
            // like on github.com -> can connect, can't execute anything on the shell
            // and we receive a message like
            //
            // Invalid command: 'true'
            //   You appear to be using ssh to clone a git:// URL.
            //   Make sure your core.gitProxy config option and the
            //   GIT_PROXY_COMMAND environment variable are NOT set.
            //
            // But anyway this is a success since the connection was succesfull.
            warn!(
                "Connection to  {}@{}:{} succeded, but received: {}",
                config.username, config.host, config.port, stderr
            );
        } else {
            // In normal circumstances we repeat the connection capturing only the status
            // somehow with the Command API it's not possibile to get output and status :S

            let status = Command::new(&ssh_cmd)
                .args(&args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if status.is_err() {
                return Err(Error::RuntimeError(status.err().unwrap()));
            }

            let status = status.unwrap();

            if !status.success() {
                return Err(Error::RuntimeError(io::Error::new(
                    io::ErrorKind::Other,
                    format!(
                        "ssh connection to {}@{}:{} failed with status: {}",
                        config.username,
                        config.host,
                        config.port,
                        status.code().unwrap(),
                    ),
                )));
            }
        }

        let rsync_cmd = which("rsync")?;
        args.remove(args.iter().position(|x| x == "true").unwrap()); // remove "true"
        let ssh_args = args.iter().map(|s| s.to_string()).collect();
        Ok(Ssh {
            remote_name: String::from(remote_name),
            config,
            ssh_cmd,
            rsync_cmd,
            ssh_args,
        })
    }
}

#[async_trait]
impl remote::Remote for Ssh {
    fn name(&self) -> String {
        self.remote_name.clone()
    }

    async fn enumerate(&self, remote_path: &Path) -> Result<Vec<String>, remote::Error> {
        let remote_path = remote_path.to_str().unwrap();
        // ssh -Pxxx user@host "find remote_path/*"
        // use find path/* instead of ls path
        // because find returns the fullpath
        // the /* is needed to return the content
        // and not the path itself
        let mut ssh = Command::new(&self.ssh_cmd)
            .args(
                self.ssh_args
                    .iter()
                    .chain(once(&format!("find {}/*", remote_path))),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let status = ssh.wait()?;

        if status.success() {
            let stdout = ssh.stdout.as_mut().unwrap();
            let mut output = String::new();
            stdout.read_to_string(&mut output).unwrap();
            return Ok(output.split_whitespace().map(|s| s.to_string()).collect());
        }

        Err(remote::Error::LocalError(io::Error::new(
            io::ErrorKind::Other,
            format!("Error during ls {} on remote host", remote_path),
        )))
    }

    async fn delete(&self, remote_path: &Path) -> Result<(), remote::Error> {
        let remote_path = remote_path.to_str().unwrap();
        // ssh -Pxxx user@host "rm -r remote_path"
        let mut ssh = Command::new(&self.ssh_cmd)
            .args(
                self.ssh_args
                    .iter()
                    .chain(once(&format!("rm -r {}", remote_path))),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        let status = ssh.wait()?;

        if status.success() {
            return Ok(());
        }

        Err(remote::Error::LocalError(io::Error::new(
            io::ErrorKind::Other,
            format!("Error during rm -r {} on remote host", remote_path),
        )))
    }

    async fn upload_file(&self, path: &Path, remote_path: &Path) -> Result<(), remote::Error> {
        use tokio::fs::File;
        use tokio::io::AsyncReadExt;

        // Read file
        let mut content: Vec<u8> = vec![];
        let mut file = File::open(path).await?;
        file.read_to_end(&mut content).await?;
        let remote_path = remote_path.to_str().unwrap();

        // cat file | ssh -Pxxx user@host "cat > file"
        let mut ssh = Command::new(&self.ssh_cmd)
            .args(
                self.ssh_args
                    .iter()
                    .chain(once(&format!("cat > {}", remote_path))),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        {
            let stdin = ssh.stdin.as_mut().unwrap();
            // This is the "cat file" on localhost piped into ssh
            // when stdin is dropped
            stdin.write_all(&content)?;
        }
        // Close stdin for being 100% sure that the process read all the file

        let status = ssh.wait()?;

        if !status.success() {
            let stdout = ssh.stdout.as_mut().unwrap();
            let stderr = ssh.stderr.as_mut().unwrap();
            let mut errlog = String::new();
            stderr.read_to_string(&mut errlog).unwrap();
            let mut outlog = String::new();
            stdout.read_to_string(&mut outlog).unwrap();

            let message = format!(
                "Failure while executing ssh command.\n\
                Stderr: {}\nStdout: {}",
                errlog, outlog
            );
            return Err(remote::Error::LocalError(io::Error::new(
                io::ErrorKind::Other,
                message,
            )));
        }
        Ok(())
    }

    async fn upload_file_compressed(
        &self,
        path: &Path,
        remote_path: &Path,
    ) -> Result<(), remote::Error> {
        // Read and compress
        let compressed_bytes = self.compress_file(path)?;
        let remote_path = self.remote_compressed_file_path(remote_path);

        // cat file | ssh -Pxxx user@host "cat > file"
        let mut ssh = Command::new(&self.ssh_cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .args(
                self.ssh_args
                    .iter()
                    .chain(once(&format!("cat > {} ", remote_path.display()))),
            )
            .spawn()?;
        ssh.stdin.as_mut().unwrap().write_all(&compressed_bytes)?;
        let status = ssh.wait()?;
        if !status.success() {
            return Err(remote::Error::LocalError(io::Error::new(
                io::ErrorKind::Other,
                "Failure while executing ssh command",
            )));
        }
        Ok(())
    }

    async fn upload_folder(
        &self,
        paths: &[PathBuf],
        remote_path: &Path,
    ) -> Result<(), remote::Error> {
        let mut local_prefix = paths.iter().min_by(|a, b| a.cmp(b)).unwrap();
        // The local_prefix found is:
        // In case of a folder: the shortest path inside the folder we want to backup.

        // If it is a folder, we of course don't want to consider this a prefix, but its parent.
        let single_location = paths.len() <= 1;
        let parent: PathBuf;
        if !single_location {
            parent = local_prefix.parent().unwrap().to_path_buf();
            local_prefix = &parent;
        }

        let remote_path = remote_path.to_str().unwrap();
        let dest = format!(
            "{}@{}:{}",
            self.config.username, self.config.host, remote_path
        );
        let src = local_prefix.to_str().unwrap();
        let ssh_port_opt = format!(r#"ssh -p {}"#, self.config.port);
        // rsync -az -e "ssh -p port" /local/folder user@host:remote_path --delete
        // delete is used to remove from remote and keep it in sync with local
        let args = vec!["-az", "-e", &ssh_port_opt, src, &dest, "--delete"];

        let status = Command::new(&self.rsync_cmd)
            .stderr(Stdio::null())
            .stdout(Stdio::null())
            .args(&args)
            .status()?;

        if !status.success() {
            return Err(remote::Error::LocalError(io::Error::new(
                io::ErrorKind::Other,
                "Failed to execute rsync trought ssh command",
            )));
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
        let compressed_folder = self.compress_folder(path)?;

        self.upload_file(compressed_folder.path(), &remote_path)
            .await
    }
}
