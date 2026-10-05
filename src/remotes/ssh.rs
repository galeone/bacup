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

use crate::config::SshConfig;
use crate::remotes::remote;

use std::io;
use std::io::prelude::*;
use std::io::Write;

use std::iter::once;
use std::path::{Path, PathBuf};

use std::fmt;
use std::string::String;

use log::{info, warn};

use tokio::fs;
use tokio::fs::File;
use tokio::io::AsyncReadExt;

use async_trait::async_trait;

use std::process::{Command, Stdio};
use which::which;

use base64::Engine;

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

/// Detect passphrase-encrypted private keys in the modern OpenSSH format
/// (`openssh-key-v1`), which unlike legacy PEM keys has no `Proc-Type`/
/// `ENCRYPTED` header. The cipher name right after the key magic is `none`
/// for unencrypted keys and a cipher name (e.g. `aes256-ctr`) otherwise.
fn openssh_key_is_encrypted(key: &str) -> bool {
    const BEGIN: &str = "-----BEGIN OPENSSH PRIVATE KEY-----";
    const END: &str = "-----END OPENSSH PRIVATE KEY-----";
    let Some(start) = key.find(BEGIN) else {
        return false;
    };
    let rest = &key[start + BEGIN.len()..];
    let Some(end) = rest.find(END) else {
        return false;
    };
    let b64: String = rest[..end]
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '+' || *c == '/')
        .collect();
    let Ok(bytes) = base64::engine::general_purpose::STANDARD_NO_PAD.decode(b64.as_bytes())
    else {
        return false;
    };
    // openssh-key-v1 layout: uint32 len, magic, uint32 len, ciphername, ...
    let read_u32 = |bytes: &[u8], pos: usize| -> Option<u32> {
        bytes
            .get(pos..pos + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    let Some(magic_len) = read_u32(&bytes, 0).map(|v| v as usize) else {
        return false;
    };
    if bytes.len() < 4 + magic_len || &bytes[4..4 + magic_len] != b"openssh-key-v1" {
        return false;
    }
    let Some(cipher_len) = read_u32(&bytes, 4 + magic_len).map(|v| v as usize) else {
        return false;
    };
    let pos = 4 + magic_len + 4;
    if bytes.len() < pos + cipher_len {
        return false;
    }
    bytes[pos..pos + cipher_len] != *b"none"
}

/// Quote a value for use inside a remote shell command (POSIX single-quoting).
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
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
    pub async fn new(config: SshConfig, remote_name: &str) -> Result<Ssh, Error> {
        let ssh_cmd = which("ssh")?;

        let private_key = shellexpand::tilde(&config.private_key).to_string();
        let private_key = PathBuf::from(private_key);
        if !private_key.exists() {
            return Err(Error::InvalidPrivateKey(format!(
                "Private key {} does not exist.",
                private_key.display(),
            )));
        }
        let private_key_file = fs::read_to_string(&private_key).await?;

        if private_key_file.contains("Proc-Type") && private_key_file.contains("ENCRYPTED") {
            return Err(Error::InvalidPrivateKey(format!(
                "Private key {} is encrypted with a passphrase. \
                            A key without passphrase is required",
                private_key.display()
            )));
        }
        // Modern OpenSSH keys ("openssh-key-v1") have no Proc-Type header;
        // detect encryption from the cipher name in the key blob.
        if openssh_key_is_encrypted(&private_key_file) {
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
            return Err(Error::RuntimeError(io::Error::other(format!(
                "ssh connection to {}@{}:{} failed with error: {}",
                config.username,
                config.host,
                config.port,
                output.err().unwrap(),
            ))));
        }

        let output = output.unwrap();
        // Process output is arbitrary bytes (locale-dependent remote
        // banners/messages) — display it lossy rather than panicking on
        // non-UTF8.
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

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
                "Connection to {}@{}:{} succeeded, but received: {}",
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
                return Err(Error::RuntimeError(io::Error::other(format!(
                    "ssh connection to {}@{}:{} failed with status: {}",
                    config.username,
                    config.host,
                    config.port,
                    status,
                ))));
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
                    .chain(once(&format!("find {}/*", shell_quote(remote_path)))),
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

        Err(remote::Error::LocalError(io::Error::other(format!(
            "Error during ls {} on remote host",
            remote_path
        ))))
    }

    async fn delete(&self, remote_path: &Path) -> Result<(), remote::Error> {
        let remote_path = remote_path.to_str().unwrap();
        // ssh -Pxxx user@host "rm -r remote_path"
        let mut ssh = Command::new(&self.ssh_cmd)
            .args(
                self.ssh_args
                    .iter()
                    .chain(once(&format!("rm -r {}", shell_quote(remote_path)))),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        let status = ssh.wait()?;

        if status.success() {
            return Ok(());
        }

        Err(remote::Error::LocalError(io::Error::other(format!(
            "Error during rm -r {} on remote host",
            remote_path
        ))))
    }

    async fn upload_file(&self, path: &Path, remote_path: &Path) -> Result<(), remote::Error> {
        let file_size = fs::metadata(path).await?.len();
        let remote_path = remote_path.to_str().unwrap();
        info!(
            "Uploading {} bytes from {} to {}",
            file_size,
            path.display(),
            remote_path
        );

        // cat file | ssh -Pxxx user@host "cat > file"
        let mut file = File::open(path).await?;
        let mut ssh = Command::new(&self.ssh_cmd)
            .args(
                self.ssh_args
                    .iter()
                    .chain(once(&format!("cat > {}", shell_quote(remote_path)))),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        {
            let stdin = ssh.stdin.as_mut().unwrap();
            // This is the "cat file" on localhost piped into ssh:
            // stream the file in chunks instead of reading it into memory.
            let mut buf = vec![0u8; 8192];
            loop {
                let n = file.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                stdin.write_all(&buf[..n])?;
            }
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
            return Err(remote::Error::LocalError(io::Error::other(message)));
        }
        info!(
            "Successfully uploaded {} bytes from {} to {}",
            file_size,
            path.display(),
            remote_path
        );
        Ok(())
    }

    async fn upload_file_compressed(
        &self,
        path: &Path,
        remote_path: &Path,
    ) -> Result<(), remote::Error> {
        // Read and compress
        let compressed_file = self.compress_file(path).await?;
        let remote_path = self.remote_compressed_file_path(remote_path);
        info!(
            "Uploading compressed file {} to {}",
            compressed_file.path().display(),
            remote_path.display()
        );

        // cat file | ssh -Pxxx user@host "cat > file"

        let mut cat = Command::new("cat")
            .arg(format!("{}", compressed_file.path().display()))
            .stdout(Stdio::piped())
            .spawn()?;

        let cat_output = match cat.stdout.take() {
            Some(out) => out,
            None => {
                return Err(remote::Error::LocalError(io::Error::other(format!(
                    "Unable to cat {}",
                    compressed_file.path().display()
                ))))
            }
        };

        let mut ssh = Command::new(&self.ssh_cmd)
            .stdin(cat_output)
            .stdout(Stdio::null())
            .args(self.ssh_args.iter().chain(once(&format!(
                "cat > {}",
                shell_quote(&remote_path.display().to_string())
            ))))
            .spawn()?;

        // Wait on ssh first: if ssh dies early, cat receives SIGPIPE and
        // its failure must not mask the real ssh error.
        let status = ssh.wait()?;
        let _ = cat.wait();

        if !status.success() {
            return Err(remote::Error::LocalError(io::Error::other(
                "Failure while executing ssh command",
            )));
        }
        info!(
            "Successfully uploaded compressed file {} to {}",
            compressed_file.path().display(),
            remote_path.display()
        );
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

        info!(
            "Synchronizing {} file(s) from {} to {}",
            paths.len(),
            src,
            dest
        );

        let status = Command::new(&self.rsync_cmd)
            .stderr(Stdio::null())
            .stdout(Stdio::null())
            .args(&args)
            .status()?;

        if !status.success() {
            return Err(remote::Error::LocalError(io::Error::other(
                "Failed to execute rsync trought ssh command",
            )));
        }

        info!(
            "Successfully synchronized {} file(s) from {} to {}",
            paths.len(),
            src,
            dest
        );
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
        info!(
            "Uploading compressed folder archive {} to {}",
            compressed_folder.path().display(),
            remote_path.display()
        );

        self.upload_file(compressed_folder.path(), &remote_path)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_openssh_key(cipher: &[u8]) -> String {
        // Minimal openssh-key-v1 blob: magic + cipher name. The parser only
        // reads up to the cipher name, so the rest of the structure is not
        // needed.
        let magic = b"openssh-key-v1";
        let mut blob = vec![];
        blob.extend_from_slice(&(magic.len() as u32).to_be_bytes());
        blob.extend_from_slice(magic);
        blob.extend_from_slice(&(cipher.len() as u32).to_be_bytes());
        blob.extend_from_slice(cipher);
        let encoded = base64::engine::general_purpose::STANDARD_NO_PAD.encode(&blob);
        format!(
            "-----BEGIN OPENSSH PRIVATE KEY-----\n{encoded}\n-----END OPENSSH PRIVATE KEY-----\n"
        )
    }

    #[test]
    fn unencrypted_openssh_key_is_not_reported_as_encrypted() {
        let key = make_openssh_key(b"none");
        assert!(!openssh_key_is_encrypted(&key));
    }

    #[test]
    fn encrypted_openssh_key_is_reported_as_encrypted() {
        for cipher in [b"aes256-ctr", b"aes128-cbc"] {
            let key = make_openssh_key(cipher);
            assert!(openssh_key_is_encrypted(&key));
        }
    }

    #[test]
    fn legacy_pem_keys_are_out_of_scope() {
        // Legacy PEM (with or without the Proc-Type/ENCRYPTED header) has no
        // OPENSSH block: the Proc-Type check in new() covers that format.
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIE...\n-----END RSA PRIVATE KEY-----\n";
        assert!(!openssh_key_is_encrypted(pem));
        let encrypted_pem = "Proc-Type: 4,ENCRYPTED\nDEK-Info: AES-128-CBC\nMIIE...";
        assert!(!openssh_key_is_encrypted(encrypted_pem));
    }

    #[test]
    fn malformed_openssh_blocks_are_not_reported_as_encrypted() {
        let garbage = "-----BEGIN OPENSSH PRIVATE KEY-----\n!!!!not-base64!!!!\n-----END OPENSSH PRIVATE KEY-----\n";
        assert!(!openssh_key_is_encrypted(garbage));
        // Valid base64, but not an openssh-key-v1 blob.
        let encoded =
            base64::engine::general_purpose::STANDARD_NO_PAD.encode(b"not a key at all");
        let not_a_key = format!(
            "-----BEGIN OPENSSH PRIVATE KEY-----\n{encoded}\n-----END OPENSSH PRIVATE KEY-----\n"
        );
        assert!(!openssh_key_is_encrypted(&not_a_key));
    }
}
