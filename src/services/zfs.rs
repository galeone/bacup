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

use std::fmt;
use std::path::PathBuf;
use std::string::String;
use std::vec::Vec;

use log::{info, warn};

use crate::config::ZfsConfig;
use crate::services::service::{Dump, Service};

use which::which;

use async_trait::async_trait;
use tokio::{fs::metadata, fs::File, io};

use std::process::Stdio;
use tokio::process::Command;

#[derive(Clone)]
pub struct Zfs {
    pub name: String,
    pub cmd: PathBuf,
    pub args: Vec<String>,
}

#[derive(Debug)]
pub enum Error {
    CommandNotFound(which::Error),
    RuntimeError(io::Error),
    ZfsError(String),
}

impl std::error::Error for Error {}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::CommandNotFound(error) => write!(f, "Command not found: {}", error),
            Error::RuntimeError(error) => write!(f, "Runtime error: {}", error),
            Error::ZfsError(error) => write!(f, "ZFS error: {}", error),
        }
    }
}

impl Zfs {
    pub async fn new(config: ZfsConfig, name: &str) -> Result<Zfs, Error> {
        let cmd = match which("zfs") {
            Err(error) => return Err(Error::CommandNotFound(error)),
            Ok(cmd) => cmd,
        };

        // Verify zfs is working and the current user is in the allow list for executing
        // snapshot and send
        let args = vec![String::from("allow"), config.dataset.clone()];
        info!("Executing {} {:?}", cmd.display(), args);
        let output = Command::new(&cmd)
            .args(args)
            // capture output to variable to check if the user is there
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        if let Err(err) = output {
            return Err(Error::RuntimeError(err));
        }
        let output = output.unwrap();
        let status = output.status;
        if !status.success() {
            return Err(Error::ZfsError(format!("Failed to verify zfs user permissions. Please run `zfs allow $USER destroy,hold,send,snapshot {}`", config.dataset)));
        }

        let stdout = String::from_utf8(output.stdout).unwrap();
        if stdout.is_empty() {
            return Err(Error::ZfsError(format!("Failed to verify zfs user permissions. Please run `zfs allow $USER destroy,hold,send,snapshot {}", config.dataset)));
        }

        // Check that the current user is in the allow list for snapshot, send and
        // destroy (destroy is needed to clean up old snapshots created by bacup).
        let user = std::env::var("USER").map_err(|_| {
            Error::ZfsError(
                "USER environment variable is not set, cannot verify zfs permissions".to_string(),
            )
        })?;
        let required = ["destroy", "hold", "send", "snapshot"];
        let prefix = format!("user {} ", user);
        let allowed = stdout
            .lines()
            .find(|line| line.trim_start().starts_with(&prefix))
            .map(|line| {
                line.trim_start()
                    .trim_start_matches(&prefix)
                    .split(',')
                    .map(str::trim)
                    .collect::<Vec<&str>>()
            });
        match allowed {
            Some(permissions) if required.iter().all(|perm| permissions.contains(perm)) => {}
            _ => {
                return Err(Error::ZfsError(format!(
                    "user \"{}\" with permissions {:?} not found in output of `zfs allow {}`. Please run `zfs allow {} destroy,hold,send,snapshot {}`",
                    user, required, config.dataset, user, config.dataset
                )));
            }
        }

        // If here, the current user is in the allow list for zfs send and snapshot

        let args: Vec<String> = vec![
            String::from("snapshot"),
            String::from("-r"),
            format!(
                "{}@{}",
                config.dataset.clone().trim(),
                config.snapshot_name.trim()
            ),
        ];

        Ok(Zfs {
            name: String::from(name),
            args,
            cmd,
        })
    }
}

#[async_trait]
impl Service for Zfs {
    async fn list(&self) -> Vec<PathBuf> {
        let dest = std::env::current_dir()
            .unwrap()
            .join(PathBuf::from(format!("{}.snapshot", self.name)));

        if metadata(&dest).await.is_ok() {
            return vec![dest];
        }
        return vec![];
    }

    async fn dump(&self) -> Result<Dump, Box<dyn std::error::Error>> {
        let mut args = self.args.clone();
        let checkpoint = args.last_mut().unwrap();

        // Current date in ISO format
        checkpoint.push_str(&format!("-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S")));

        let checkpoint_name = checkpoint.clone();

        // Step 1, execute the checkpoint (atomic, immediate action)
        info!("Executing: {} {:?}", self.cmd.display(), args);
        let status = Command::new(&self.cmd)
            .args(&args)
            .stdout(Stdio::null())
            .status()
            .await;
        if status.is_err() {
            return Err(Error::RuntimeError(status.err().unwrap()).into());
        }

        // Step 2, send the checkpoint to a local file, named: name.snapshot
        let dest = std::env::current_dir()
            .unwrap()
            .join(PathBuf::from(format!("{}.snapshot", self.name)));

        let parent = dest.parent().unwrap();
        if !parent.exists() {
            return Err(Error::RuntimeError(io::Error::other(format!(
                "Folder {} does not exist.",
                parent.display()
            )))
            .into());
        }

        let dest_file = File::create(&dest).await?;
        // TODO: send dry-run, get the "total estimated size is XXX$unit"
        // convert to bytes, and use has_enough_space to check.
        let send_args = vec![
            String::from("send"),
            String::from("-R"),
            String::from("-v"),
            checkpoint_name.to_string(),
        ];

        info!(
            "Executing: {} {:?} > {}",
            self.cmd.display(),
            send_args,
            dest.display()
        );

        let status = Command::new(&self.cmd)
            .args(&send_args)
            .stdout(Stdio::from(dest_file.try_into_std().unwrap()))
            .status()
            .await;
        if let Err(error) = status {
            return Err(Error::RuntimeError(error).into());
        }
        let status = status?;
        match status.success() {
            true => {
                info!("ZFS checkpoint completed successfully");
                destroy_old_snapshots(&args).await;
                Ok(Dump { path: Some(dest) })
            }
            false => Err(Error::RuntimeError(io::Error::other(format!(
                "{} {:?} failed with exit code {}",
                self.cmd.display(),
                self.args,
                status.code().unwrap()
            )))
            .into()),
        }
    }
}

/// Destroy all old snapshots matching the checkpoint name used by bacup,
/// except the one just created, so they don't accumulate indefinitely.
///
/// `args` is the zfs send arg list; its last element is the checkpoint
/// name, e.g. `dataset@name-20260101-120000`.
async fn destroy_old_snapshots(args: &[String]) {
    let Some(checkpoint) = args.last() else {
        return;
    };
    let Some(at_pos) = checkpoint.rfind('@') else {
        return;
    };
    let dataset = &checkpoint[..at_pos];
    let snapshot_prefix = &checkpoint[at_pos + 1..];

    let output = tokio::process::Command::new("zfs")
        .arg("list")
        .arg("-H")
        .arg("-o")
        .arg("name")
        .arg("-t")
        .arg("snapshot")
        .arg(format!("{}@{}-*", dataset, snapshot_prefix))
        .output()
        .await;

    let Ok(output) = output else {
        warn!("Failed to run `zfs list` to find old snapshots");
        return;
    };
    if !output.status.success() {
        warn!(
            "zfs list failed with exit code: {}",
            output.status.code().unwrap_or(-1)
        );
        return;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut destroyed = 0;
    for name in stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if name == checkpoint {
            continue; // keep the snapshot we just created
        }
        match tokio::process::Command::new("zfs")
            .arg("destroy")
            .arg(name)
            .output()
            .await
        {
            Ok(o) if o.status.success() => {
                info!("Destroyed old ZFS snapshot {}", name);
                destroyed += 1;
            }
            Ok(o) => warn!(
                "Failed to destroy ZFS snapshot {}: {}",
                name,
                String::from_utf8_lossy(&o.stderr)
            ),
            Err(e) => warn!("Failed to run `zfs destroy {}`: {}", name, e),
        }
    }
    if destroyed > 0 {
        info!("Destroyed {} old ZFS snapshot(s)", destroyed);
    }
}
