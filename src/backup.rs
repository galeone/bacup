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

use crate::config::BackupConfig;
use crate::ping::{Pinger, Signal};
use crate::remotes::remote;
use crate::services::folders;
use crate::services::service::Service;
use crate::services::zfs::retention_sort_key;
use crate::when;

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_cron_scheduler::JobSchedulerError;
use tokio_cron_scheduler::{Job, JobScheduler};

use log::{error, info};

use uuid::Uuid;

#[derive(Debug)]
pub enum Error {
    InvalidCronConfiguration(String),
    RuntimeError(io::Error),
    InvalidWhenConfiguration(String),
    GeneralError(Box<dyn std::error::Error>),
}

impl std::error::Error for Error {}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidCronConfiguration(error) => write!(f, "{}", error),
            Error::RuntimeError(error) => write!(f, "Runtime error: {}", error),
            Error::InvalidWhenConfiguration(msg) => write!(f, "Invalid when string: {}", msg),
            Error::GeneralError(error) => write!(f, "{}", error),
        }
    }
}

/// The part of a run that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// bacup could not start (a remote or service failed to initialize).
    Startup,
    /// A folder's `max_age` check failed: its newest file is too old.
    Freshness,
    Dump,
    /// The service produced nothing to back up.
    List,
    Upload,
    /// Compression happens inside the remote's upload call, so the two
    /// can't be told apart.
    CompressUpload,
    /// Deleting old backups for `keep_last` failed (the new backup was
    /// uploaded).
    Prune,
    /// The run panicked.
    Panic,
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Step::Startup => write!(f, "startup"),
            Step::Freshness => write!(f, "freshness"),
            Step::Dump => write!(f, "dump"),
            Step::List => write!(f, "list"),
            Step::Upload => write!(f, "upload"),
            Step::CompressUpload => write!(f, "compress+upload"),
            Step::Prune => write!(f, "prune"),
            Step::Panic => write!(f, "panic"),
        }
    }
}

/// Why a run failed.
#[derive(Debug)]
pub struct Failure {
    pub step: Step,
    pub error: String,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} failed: {}", self.step, self.error)
    }
}

impl Failure {
    /// The plain-text report sent with the `/fail` ping. `what` and
    /// `where` are the config keys (e.g. `folders.home`, `aws.bucket`).
    pub fn report(&self, backup: &str, what: &str, r#where: &str) -> String {
        format!(
            "bacup backup \"{backup}\" failed\n\
             backup: {backup} ({what} -> {where})\n\
             step: {step}\n\
             error: {error}\n\
             host: {host}\n\
             version: bacup {version}\n",
            step = self.step,
            error = self.error,
            host = sysinfo::System::host_name().unwrap_or_else(|| "unknown".to_string()),
            version = env!("CARGO_PKG_VERSION"),
        )
    }
}

/// A backup to notify when bacup itself fails to start.
pub struct StartupTarget {
    pub name: String,
    pub what: String,
    pub r#where: String,
    pub pinger: Pinger,
}

impl StartupTarget {
    /// Every backup with a usable `ping_url`, sorted by name.
    pub fn from_config(backups: &HashMap<String, BackupConfig>) -> Vec<StartupTarget> {
        let mut targets: Vec<StartupTarget> = backups
            .iter()
            .filter_map(|(name, config)| {
                let url = config.ping_url.clone()?;
                match Pinger::new(url) {
                    Ok(pinger) => Some(StartupTarget {
                        name: name.clone(),
                        what: config.what.clone(),
                        r#where: config.r#where.clone(),
                        pinger,
                    }),
                    Err(error) => {
                        error!("[{}] {}", name, error);
                        None
                    }
                }
            })
            .collect();
        targets.sort_by(|a, b| a.name.cmp(&b.name));
        targets
    }
}

/// Sends a `startup` failure to every target, concurrently.
pub async fn report_startup_failure(targets: &[StartupTarget], error: &str) {
    let failure = Failure {
        step: Step::Startup,
        error: error.to_string(),
    };
    futures::future::join_all(targets.iter().map(|target| {
        let report = failure.report(&target.name, &target.what, &target.r#where);
        target.pinger.send(Signal::Fail(report), &target.name)
    }))
    .await;
}

/// A panic payload as text.
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        message.to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

pub struct Backup {
    pub name: String,
    /// The `what` and `where` config keys, for failure reports.
    pub what_key: String,
    pub where_key: String,
    pub what: Box<dyn Service + Send + Sync>,
    pub r#where: Box<dyn remote::Remote + Send + Sync>,
    pub remote_path: PathBuf,
    pub when: String,
    pub compress: bool,
    pub schedule: when::Schedule,
    pub keep_last: Option<u32>,
    pub pinger: Option<Pinger>,
}

impl Backup {
    pub async fn new(
        name: &str,
        remote: Box<dyn remote::Remote + Send + Sync>,
        service: Box<dyn Service + Send + Sync>,
        config: &BackupConfig,
    ) -> Result<Backup, Error> {
        let schedule =
            when::Schedule::parse(&config.when).map_err(Error::InvalidCronConfiguration)?;

        let pinger = match &config.ping_url {
            Some(url) => Some(Pinger::new(url.clone()).map_err(|e| Error::GeneralError(e.into()))?),
            None => None,
        };

        Ok(Backup {
            name: String::from(name),
            what_key: config.what.clone(),
            where_key: config.r#where.clone(),
            what: service,
            r#where: remote,
            remote_path: PathBuf::from(config.remote_path.clone()),
            when: config.when.clone(),
            compress: config.compress,
            schedule,
            keep_last: config.keep_last,
            pinger,
        })
    }

    fn log_result(
        result: &Result<(), remote::Error>,
        name: &str,
        file: &Path,
        remote_name: &str,
        remote_path: &Path,
        compress: bool,
    ) {
        match result {
            Ok(()) => info!(
                "[{}] Successfully uploaded {} {}: {} to [{}] {}",
                name,
                if compress { " and compressed" } else { "" },
                if file.is_dir() { "folder" } else { "file" },
                file.display(),
                remote_name,
                remote_path.display(),
            ),
            Err(error) => error!(
                "[{}] Error during upload{} of {}: {}. Error: {}",
                name,
                if compress { " or compression" } else { "" },
                file.display(),
                remote_name,
                error
            ),
        }
    }

    /// One backup run: dump, upload, prune. Returns the first failure; an
    /// upload failure wins over a prune failure.
    pub async fn run(&self) -> Result<(), Failure> {
        let remote = &self.r#where;
        let service = &self.what;
        let compress = self.compress;
        let name = self.name.clone();
        let remote_prefix = self.remote_path.clone();
        let keep_last = self.keep_last;

        // First call dump, to trigger the dump service if present
        info!("[{}] Calling dump...", name);
        let dump = service.dump().await.map_err(|error| Failure {
            step: match error.downcast_ref::<folders::Error>() {
                Some(error) if error.is_freshness() => Step::Freshness,
                _ => Step::Dump,
            },
            error: error.to_string(),
        })?;

        let path = dump.path.clone().unwrap_or_default();
        if path.exists() {
            // When dump goes out of scope, the dump is removed by Drop.
            info!("[{}] Dumped {}. Backing it up", name, path.display());
        }

        // Then loop over all the dumped files and backup them as specified
        let mut local_files = service.list().await;
        if local_files.is_empty() {
            return Err(Failure {
                step: Step::List,
                error: "nothing to back up: the service listed no files".to_string(),
            });
        }

        // If the local_files list contains a single file, the upload should be in the form:
        // /remote/prefix/filename
        // even if the local file is in /local/path/in/folder/filename
        let mut single_file = local_files.len() == 1;

        // If the local_files list is a list of multiple files, we suppose these files all
        // share the same root. To find the root we can simply find the shortest string.
        // In this way, we can remove the "root prefix" and upload correctly.
        // From:
        // - /local/path/in/folder/A
        // - /local/path/in/folder/B
        // To
        // - /remote/prefix/A
        // - /remote/prefix/B
        let local_files_clone = local_files.clone();
        let mut local_prefix = local_files_clone
            .iter()
            .min_by(|a, b| a.cmp(b))
            .unwrap()
            .as_path();

        // The local_prefix found is:
        // In case of a folder: the shortest path inside the folder we want to backup.
        // In case of a file: the file itself.

        // If is a folder, we of course don't want to consider this a prefix, but its parent.
        if !single_file {
            local_prefix = local_prefix.parent().unwrap();
        }

        // If we are going to compress the local_files we need to take care of the content of
        // the .list()-ed files.
        // In case of compression of a folder, e.g. if the list_contains glob(/a/folder/**)
        // we have to pass the the Remote.upload_folder_compressed only /a/folder for creating
        // a single archive.
        // Otherwise we'll create a different archive for every file/folder and this is wrong.
        let all_with_same_prefix = local_files_clone
            .iter()
            .all(|path| path.starts_with(local_prefix));
        if compress && !single_file && all_with_same_prefix {
            single_file = true;
            local_files = vec![PathBuf::from(local_prefix)];
        }

        // The first upload error, if any: the service is told after the
        // uploads, and the run fails with it.
        let mut upload_error: Option<String> = None;
        // The first keep_last pruning error, if any.
        let mut prune_error: Option<String> = None;
        let mut failed_uploads = 0usize;
        let mut record = |result: &Result<(), remote::Error>, file: &Path| {
            if let Err(error) = result {
                failed_uploads += 1;
                upload_error.get_or_insert_with(|| format!("{}: {}", file.display(), error));
            }
        };

        // Special case in which we want to upload a folder without compression
        // If all the files share the same prefix, we upload all the files in this prefix.
        // The remote should handle eventual incremental backup.
        if !single_file && all_with_same_prefix && !compress {
            let remote_path = &remote_prefix;
            info!(
                "[{}] Uploading a list of files to {}",
                name,
                remote_path.display()
            );
            let result = remote.upload_folder(&local_files, remote_path).await;
            record(&result, local_prefix);
            Backup::log_result(
                &result,
                &name,
                local_prefix,
                &remote.name(),
                remote_path,
                compress,
            );
            info!("[{}] Uploaded completed.", name);
            // Set local_files to empty vector for skipping the next loop
            // and avoid to add another else branch that will increase the
            // indentation again.
            local_files = vec![];
        }

        for file in local_files {
            let remote_path = if single_file {
                remote_prefix.join(file.file_name().unwrap())
            } else {
                remote_prefix.join(file.strip_prefix(local_prefix).unwrap())
            };

            let result = if file.is_dir() {
                // compress for sure, the uncompressed scenarios has been treated
                // outside this loop
                info!(
                    "[{}] Compressing folder {} and uploading to {}",
                    name,
                    file.display(),
                    remote_path.display()
                );
                remote.upload_folder_compressed(&file, &remote_path).await
            } else if compress {
                info!(
                    "[{}] Compressing file {} and uploading to {}",
                    name,
                    file.display(),
                    remote_path.display()
                );
                remote.upload_file_compressed(&file, &remote_path).await
            } else {
                info!(
                    "[{}] Uploading file {} to {}",
                    name,
                    file.display(),
                    remote_path.display()
                );
                remote.upload_file(&file, &remote_path).await
            };

            record(&result, &file);

            // Handle keep_last. Skipped when the upload failed: the
            // remote did not get a new backup, so nothing old goes.
            if let (Some(to_keep), true) = (keep_last, result.is_ok()) {
                let to_keep = to_keep as usize;
                match remote.enumerate(remote_path.parent().unwrap()).await {
                    Ok(list) => {
                        for delete_me in &to_prune(list, to_keep) {
                            if let Some(error) =
                                remote.delete(&PathBuf::from(delete_me)).await.err()
                            {
                                error!(
                                    "[{}] Error during delete of {}: {}",
                                    name, delete_me, error
                                );
                                prune_error.get_or_insert_with(|| {
                                    format!("delete of {} failed: {}", delete_me, error)
                                });
                            } else {
                                info!("[{}] Deleted {}", name, delete_me);
                            }
                        }
                    }
                    Err(error) => {
                        error!("[{}] Error during remote.enumerate: {}", name, error);
                        prune_error.get_or_insert_with(|| {
                            format!(
                                "listing {} failed: {}",
                                remote_path.parent().unwrap().display(),
                                error
                            )
                        });
                    }
                }
            }

            Backup::log_result(
                &result,
                &name,
                &file,
                &remote.name(),
                &remote_path,
                compress,
            );
        }

        if let Some(error) = upload_error {
            service.upload_failed(&dump).await;
            let error = if failed_uploads > 1 {
                format!("{failed_uploads} uploads failed, the first: {error}")
            } else {
                error
            };
            return Err(Failure {
                step: if compress {
                    Step::CompressUpload
                } else {
                    Step::Upload
                },
                error,
            });
        }
        if let Some(error) = prune_error {
            return Err(Failure {
                step: Step::Prune,
                error,
            });
        }
        Ok(())
    }

    /// [Backup::run], with a panic turned into a [Step::Panic] failure.
    /// The panic is still printed to the log by the default panic hook.
    pub async fn run_catching_panics(&self) -> Result<(), Failure> {
        use futures::FutureExt;
        match std::panic::AssertUnwindSafe(self.run())
            .catch_unwind()
            .await
        {
            Ok(result) => result,
            Err(payload) => Err(Failure {
                step: Step::Panic,
                error: panic_message(payload),
            }),
        }
    }

    /// A scheduled run: pings `start`, runs the backup, then pings
    /// `success`, or `fail` with a report of the failed step. Pings never
    /// change the outcome.
    pub async fn run_and_report(&self) {
        if let Some(pinger) = &self.pinger {
            pinger.send(Signal::Start, &self.name).await;
        }
        match self.run_catching_panics().await {
            Ok(()) => {
                info!("[{}] Backup completed", self.name);
                if let Some(pinger) = &self.pinger {
                    pinger.send(Signal::Success, &self.name).await;
                }
            }
            Err(failure) => {
                error!("[{}] Backup failed: {}", self.name, failure);
                if let Some(pinger) = &self.pinger {
                    let report = failure.report(&self.name, &self.what_key, &self.where_key);
                    pinger.send(Signal::Fail(report), &self.name).await;
                }
            }
        }
        if let Some(next) = self.schedule.next() {
            info!("[{}] Next run: {}", self.name, next);
        }
    }

    pub async fn schedule(
        self: Arc<Self>,
        scheduler: &mut JobScheduler,
    ) -> Result<Uuid, JobSchedulerError> {
        let expression = self.schedule.expression.clone();
        scheduler
            .add(
                Job::new_async(expression.as_str(), move |_uuid, _js| {
                    let inst = self.clone();
                    Box::pin(async move {
                        inst.run_and_report().await;
                    })
                })
                .unwrap(),
            )
            .await
    }
}

/// The remote objects to delete so that only the `to_keep` newest remain.
/// Objects are ordered chronologically by [retention_sort_key], so zfs
/// `full`/`inc` dump files are pruned by age and not by kind.
fn to_prune(mut list: Vec<String>, to_keep: usize) -> Vec<String> {
    if list.len() <= to_keep {
        return vec![];
    }
    list.sort_by_cached_key(|name| retention_sort_key(name));
    list.reverse();
    list.split_off(to_keep)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_prune_keeps_the_newest_by_name() {
        let list = vec![
            "p/2026-10-03-22:00-db.gz".to_string(),
            "p/2026-10-05-22:00-db.gz".to_string(),
            "p/2026-10-04-22:00-db.gz".to_string(),
        ];
        assert_eq!(to_prune(list.clone(), 2), vec!["p/2026-10-03-22:00-db.gz"]);
        assert!(to_prune(list, 3).is_empty());
    }

    #[test]
    fn to_prune_zfs_chain_across_a_new_full() {
        // daily backups, monthly full on the 1st, keep_last = 32: the
        // October chain (full on Oct 5 + daily incs) followed by the
        // November full and a week of incs.
        let file = |kind: &str, month: u32, day: u32| {
            format!("gtr7/zfs/storage/storage-{kind}-2026{month:02}{day:02}-220000.snapshot")
        };
        let mut list = vec![file("full", 10, 5)];
        list.extend((6..=31).map(|d| file("inc", 10, d)));
        list.push(file("full", 11, 1));
        list.extend((2..=7).map(|d| file("inc", 11, d)));

        let pruned = to_prune(list.clone(), 32);
        // Only the oldest objects go, and the November full is kept:
        // the remote stays restorable from it.
        assert_eq!(pruned, vec![file("inc", 10, 6), file("full", 10, 5)]);
        let kept: Vec<_> = list.iter().filter(|f| !pruned.contains(f)).collect();
        assert!(kept.contains(&&file("full", 11, 1)));
        assert_eq!(kept.len(), 32);
    }

    mod runs {
        use super::super::*;
        use crate::config::{BackupConfig, LocalhostConfig};
        use crate::ping::tests::{fast, FakeEndpoint, Received, Reply};
        use crate::remotes::localhost::Localhost;
        use crate::services::folders::Folder;
        use crate::services::service::Dump;
        use async_trait::async_trait;
        use tempfile::TempDir;
        use url::Url;

        #[derive(Clone)]
        struct FailingDump;

        #[async_trait]
        impl Service for FailingDump {
            async fn dump(&self) -> Result<Dump, Box<dyn std::error::Error>> {
                Err("pg_dump exited with status 1".into())
            }
            async fn list(&self) -> Vec<PathBuf> {
                vec![]
            }
        }

        #[derive(Clone)]
        struct PanickingDump;

        #[async_trait]
        impl Service for PanickingDump {
            async fn dump(&self) -> Result<Dump, Box<dyn std::error::Error>> {
                panic!("index out of bounds");
            }
            async fn list(&self) -> Vec<PathBuf> {
                vec![]
            }
        }

        /// Localhost, except that listing the remote (for keep_last) fails.
        #[derive(Clone)]
        struct BrokenListing(Localhost);

        #[async_trait]
        impl remote::Remote for BrokenListing {
            async fn upload_file(&self, p: &Path, r: &Path) -> Result<(), remote::Error> {
                self.0.upload_file(p, r).await
            }
            async fn upload_folder(&self, p: &[PathBuf], r: &Path) -> Result<(), remote::Error> {
                self.0.upload_folder(p, r).await
            }
            async fn upload_file_compressed(
                &self,
                p: &Path,
                r: &Path,
            ) -> Result<(), remote::Error> {
                self.0.upload_file_compressed(p, r).await
            }
            async fn upload_folder_compressed(
                &self,
                p: &Path,
                r: &Path,
            ) -> Result<(), remote::Error> {
                self.0.upload_folder_compressed(p, r).await
            }
            async fn enumerate(&self, _: &Path) -> Result<Vec<String>, remote::Error> {
                Err(remote::Error::LocalError(std::io::Error::other(
                    "InvalidAccessKeyId",
                )))
            }
            async fn delete(&self, r: &Path) -> Result<(), remote::Error> {
                self.0.delete(r).await
            }
            fn name(&self) -> String {
                self.0.name()
            }
        }

        struct Fixture {
            source: TempDir,
            remote: TempDir,
        }

        impl Fixture {
            fn new() -> Fixture {
                let source = TempDir::new().unwrap();
                std::fs::write(source.path().join("data.txt"), "hello").unwrap();
                Fixture {
                    source,
                    remote: TempDir::new().unwrap(),
                }
            }

            async fn backup(
                &self,
                service: Option<Box<dyn Service + Send + Sync>>,
                ping_url: Option<Url>,
            ) -> Backup {
                self.backup_with(service, ping_url, None, None).await
            }

            fn localhost(&self) -> Localhost {
                Localhost::new(
                    LocalhostConfig {
                        path: self.remote.path().to_str().unwrap().to_string(),
                    },
                    "test",
                )
                .unwrap()
            }

            async fn backup_with(
                &self,
                service: Option<Box<dyn Service + Send + Sync>>,
                ping_url: Option<Url>,
                remote: Option<Box<dyn remote::Remote + Send + Sync>>,
                keep_last: Option<u32>,
            ) -> Backup {
                let config = BackupConfig {
                    what: "folders.test".to_string(),
                    r#where: "localhost.test".to_string(),
                    when: "daily 01:00".to_string(),
                    remote_path: "/out/".to_string(),
                    compress: false,
                    keep_last,
                    ping_url,
                };
                let remote = remote.unwrap_or_else(|| Box::new(self.localhost()));
                let service = match service {
                    Some(service) => service,
                    None => Box::new(
                        Folder::new(self.source.path().join("data.txt").to_str().unwrap())
                            .await
                            .unwrap(),
                    ),
                };
                let mut backup = Backup::new("test", remote, service, &config).await.unwrap();
                backup.pinger = backup.pinger.map(fast);
                backup
            }

            /// Makes every upload fail: the remote root becomes a file, so
            /// no directory can be created under it (even as root).
            fn break_remote(&self) {
                std::fs::remove_dir_all(self.remote.path()).unwrap();
                std::fs::write(self.remote.path(), "not a directory").unwrap();
            }
        }

        fn paths(endpoint: &FakeEndpoint) -> Vec<(String, String)> {
            endpoint
                .requests()
                .into_iter()
                .map(|r| (r.method, r.path))
                .collect()
        }

        fn get(path: &str) -> (String, String) {
            ("GET".to_string(), path.to_string())
        }

        /// Checks that the endpoint got `start` then a `fail` whose report
        /// names the backup and `step`, and returns the report.
        fn assert_failed(endpoint: &FakeEndpoint, step: &str) -> String {
            let requests = endpoint.requests();
            assert_eq!(
                paths(endpoint),
                vec![
                    get("/ping/abc/start"),
                    ("POST".to_string(), "/ping/abc/fail".to_string())
                ]
            );
            let Received { body, .. } = requests[1].clone();
            assert!(
                body.contains("backup: test (folders.test -> localhost.test)"),
                "{}",
                body
            );
            assert!(body.contains(&format!("step: {step}\n")), "{}", body);
            assert!(body.contains("host: "), "{}", body);
            body
        }

        #[tokio::test]
        async fn success_pings_start_then_success() {
            let endpoint = FakeEndpoint::start(Reply::Status(200)).await;
            let fixture = Fixture::new();
            let backup = fixture.backup(None, Some(endpoint.base.clone())).await;
            backup.run_and_report().await;
            assert!(fixture.remote.path().join("out").join("data.txt").exists());
            assert_eq!(
                paths(&endpoint),
                vec![get("/ping/abc/start"), get("/ping/abc")]
            );
        }

        #[tokio::test]
        async fn failed_upload_pings_fail_with_the_report() {
            let endpoint = FakeEndpoint::start(Reply::Status(200)).await;
            let fixture = Fixture::new();
            let backup = fixture.backup(None, Some(endpoint.base.clone())).await;
            fixture.break_remote();

            let failure = backup.run().await.unwrap_err();
            assert_eq!(failure.step, Step::Upload);
            assert!(failure.error.contains("data.txt"), "{}", failure);

            backup.run_and_report().await;
            let body = assert_failed(&endpoint, "upload");
            assert!(body.contains("data.txt"), "{}", body);
        }

        #[tokio::test]
        async fn failed_dump_pings_fail_with_the_report() {
            let endpoint = FakeEndpoint::start(Reply::Status(200)).await;
            let fixture = Fixture::new();
            let backup = fixture
                .backup(Some(Box::new(FailingDump)), Some(endpoint.base.clone()))
                .await;

            let failure = backup.run().await.unwrap_err();
            assert_eq!(failure.step, Step::Dump);
            assert_eq!(failure.error, "pg_dump exited with status 1");

            backup.run_and_report().await;
            let body = assert_failed(&endpoint, "dump");
            assert!(
                body.contains("error: pg_dump exited with status 1"),
                "{}",
                body
            );
        }

        #[tokio::test]
        async fn a_panic_is_reported_as_a_failure() {
            let endpoint = FakeEndpoint::start(Reply::Status(200)).await;
            let fixture = Fixture::new();
            let backup = fixture
                .backup(Some(Box::new(PanickingDump)), Some(endpoint.base.clone()))
                .await;
            backup.run_and_report().await;
            let body = assert_failed(&endpoint, "panic");
            assert!(body.contains("index out of bounds"), "{}", body);
        }

        #[tokio::test]
        async fn an_empty_source_fails_instead_of_panicking() {
            let endpoint = FakeEndpoint::start(Reply::Status(200)).await;
            let fixture = Fixture::new();
            let empty = TempDir::new().unwrap();
            let folder = Folder::new(empty.path().join("*").to_str().unwrap())
                .await
                .unwrap();
            let backup = fixture
                .backup(Some(Box::new(folder)), Some(endpoint.base.clone()))
                .await;
            backup.run_and_report().await;
            assert_failed(&endpoint, "list");
        }

        #[tokio::test]
        async fn stale_input_is_reported_and_not_uploaded() {
            let endpoint = FakeEndpoint::start(Reply::Status(200)).await;
            let fixture = Fixture::new();
            let old = std::time::SystemTime::now() - std::time::Duration::from_secs(31 * 3600);
            std::fs::File::options()
                .write(true)
                .open(fixture.source.path().join("data.txt"))
                .unwrap()
                .set_modified(old)
                .unwrap();
            let folder = Folder::new(fixture.source.path().to_str().unwrap())
                .await
                .unwrap()
                .with_max_age(Some(folders::parse_duration("26h").unwrap()));
            let backup = fixture
                .backup(Some(Box::new(folder)), Some(endpoint.base.clone()))
                .await;
            backup.run_and_report().await;
            let body = assert_failed(&endpoint, "freshness");
            assert!(body.contains("is 31h old (max_age 26h)"), "{}", body);
            // Nothing reached the remote.
            assert!(!fixture.remote.path().join("out").exists());
        }

        #[tokio::test]
        async fn a_prune_error_fails_the_run_after_the_upload() {
            let endpoint = FakeEndpoint::start(Reply::Status(200)).await;
            let fixture = Fixture::new();
            let remote = Box::new(BrokenListing(fixture.localhost()));
            let backup = fixture
                .backup_with(None, Some(endpoint.base.clone()), Some(remote), Some(3))
                .await;
            backup.run_and_report().await;
            // The new backup is on the remote; only the pruning failed.
            assert!(fixture.remote.path().join("out").join("data.txt").exists());
            let body = assert_failed(&endpoint, "prune");
            assert!(body.contains("InvalidAccessKeyId"), "{}", body);
        }

        #[tokio::test]
        async fn startup_failure_reaches_every_ping_url() {
            let first = FakeEndpoint::start(Reply::Status(200)).await;
            let second = FakeEndpoint::start(Reply::Status(200)).await;
            let config = |what: &str, ping_url: Option<Url>| BackupConfig {
                what: what.to_string(),
                r#where: "aws.backups".to_string(),
                when: "daily 01:00".to_string(),
                remote_path: "/".to_string(),
                compress: true,
                keep_last: None,
                ping_url,
            };
            let mut backups = HashMap::new();
            backups.insert(
                "a".to_string(),
                config("postgres.a", Some(first.base.clone())),
            );
            backups.insert(
                "b".to_string(),
                config("folders.b", Some(second.base.clone())),
            );
            backups.insert("c".to_string(), config("folders.c", None));

            let mut targets = StartupTarget::from_config(&backups);
            assert_eq!(targets.len(), 2);
            for target in &mut targets {
                target.pinger = fast(target.pinger.clone());
            }
            report_startup_failure(
                &targets,
                "Failed to configure remote aws.backups: InvalidAccessKeyId",
            )
            .await;

            for (endpoint, line) in [
                (&first, "backup: a (postgres.a -> aws.backups)"),
                (&second, "backup: b (folders.b -> aws.backups)"),
            ] {
                let requests = endpoint.requests();
                assert_eq!(requests.len(), 1);
                assert_eq!(requests[0].path, "/ping/abc/fail");
                let body = &requests[0].body;
                assert!(body.contains(line), "{}", body);
                assert!(body.contains("step: startup\n"), "{}", body);
                assert!(body.contains("InvalidAccessKeyId"), "{}", body);
            }
        }

        #[test]
        fn report_lists_backup_step_and_error() {
            let failure = Failure {
                step: Step::CompressUpload,
                error: "Remote error: InvalidAccessKeyId".to_string(),
            };
            let report = failure.report("blog", "postgres.blog", "aws.backups");
            let lines: Vec<_> = report.lines().collect();
            assert_eq!(lines[0], "bacup backup \"blog\" failed");
            assert_eq!(lines[1], "backup: blog (postgres.blog -> aws.backups)");
            assert_eq!(lines[2], "step: compress+upload");
            assert_eq!(lines[3], "error: Remote error: InvalidAccessKeyId");
            assert!(lines[4].starts_with("host: "));
        }

        #[tokio::test]
        async fn a_dead_ping_endpoint_does_not_fail_the_backup() {
            let endpoint = FakeEndpoint::start(Reply::Status(503)).await;
            let fixture = Fixture::new();
            let backup = fixture.backup(None, Some(endpoint.base.clone())).await;
            backup.run_and_report().await;
            assert!(fixture.remote.path().join("out").join("data.txt").exists());
        }

        #[tokio::test]
        async fn without_ping_url_nothing_is_sent() {
            let fixture = Fixture::new();
            let backup = fixture.backup(None, None).await;
            assert!(backup.pinger.is_none());
            assert!(backup.run().await.is_ok());
        }
    }

    fn validate_cron_expression(when: &str) {
        let result = when::parse_when(when);
        assert!(
            result.is_ok(),
            "Failed to parse when string '{}': {}",
            when,
            result.err().unwrap()
        );
        // Parsed with the same configuration as tokio-cron-scheduler.
        if let Err(e) = when::Schedule::parse(when) {
            panic!("Invalid schedule for when string '{}': {}", when, e);
        }
    }

    #[test]
    fn test_parse_when_daily() {
        // Valid cases
        validate_cron_expression("daily 00:00");
        validate_cron_expression("daily 12:30");
        validate_cron_expression("Daily 00:00");
        validate_cron_expression("DAILY 11:11");

        // Invalid cases
        assert!(when::parse_when("dayly 00:00").is_err());
        assert!(when::parse_when("daily 55:00").is_err());
        assert!(when::parse_when("daily 00:61").is_err());
        assert!(when::parse_when("daily 00:60").is_err());
        assert!(when::parse_when("daily 24:01").is_err());
    }

    #[test]
    fn test_parse_when_weekly() {
        // Valid cases
        validate_cron_expression("weekly monday 12:30");
        validate_cron_expression("weekly mon 12:30");
        validate_cron_expression("weekly tuesday 12:30");
        validate_cron_expression("weekly tue 12:30");
        validate_cron_expression("weekly wednesday 12:30");
        validate_cron_expression("weekly wed 12:30");
        validate_cron_expression("weekly thursday 12:30");
        validate_cron_expression("weekly thu 12:30");
        validate_cron_expression("weekly friday 12:30");
        validate_cron_expression("weekly fri 12:30");
        validate_cron_expression("weekly Saturday 12:30");
        validate_cron_expression("weekly Sat 12:30");
        validate_cron_expression("WEEKLY SUN 12:30");
        validate_cron_expression("weekly sunday 12:30");
        validate_cron_expression(" SUN 12:30");
        validate_cron_expression(" sunday 12:30");

        // Invalid cases
        assert!(when::parse_when("watly monzay 00:00").is_err());
        assert!(when::parse_when("monzay 00:00").is_err());
        assert!(when::parse_when("Moonday 00:00").is_err());
        assert!(when::parse_when("Sundays 1:00").is_err());
        assert!(when::parse_when("Today 00:00").is_err());
        assert!(when::parse_when("Tomorrow 00:00").is_err());
        assert!(when::parse_when("Toyota -1:00").is_err());
    }

    #[test]
    fn test_parse_when_montly() {
        // Valid cases
        validate_cron_expression("Monthly 1 02:30");
        validate_cron_expression("Monthly 31 02:30");

        // Invalid cases
        assert!(when::parse_when("Monthly 00:00").is_err());
        assert!(when::parse_when("Monthtly -1 00:00").is_err());
        assert!(when::parse_when("Monthtly 0 00:00").is_err());
        assert!(when::parse_when("Monthtly 32 00:00").is_err());
    }
}
