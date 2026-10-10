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
use crate::remotes::remote;
use crate::services::service::Service;
use crate::services::zfs::retention_sort_key;
use crate::when;

use cron::Schedule;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use tokio_cron_scheduler::JobSchedulerError;
use tokio_cron_scheduler::{Job, JobScheduler};

use log::{error, info};

use uuid::Uuid;

#[derive(Debug)]
pub enum Error {
    InvalidCronConfiguration(cron::error::Error),
    RuntimeError(io::Error),
    InvalidWhenConfiguration(String),
    GeneralError(Box<dyn std::error::Error>),
}

impl std::error::Error for Error {}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidCronConfiguration(error) => write!(f, "Invalid cron string: {}", error),
            Error::RuntimeError(error) => write!(f, "Runtime error: {}", error),
            Error::InvalidWhenConfiguration(msg) => write!(f, "Invalid when string: {}", msg),
            Error::GeneralError(error) => write!(f, "{}", error),
        }
    }
}

pub struct Backup {
    pub name: String,
    pub what: Box<dyn Service + Send + Sync>,
    pub r#where: Box<dyn remote::Remote + Send + Sync>,
    pub remote_path: PathBuf,
    pub when: String,
    pub compress: bool,
    pub schedule: Schedule,
    pub keep_last: Option<u32>,
}

impl Backup {
    pub async fn new(
        name: &str,
        remote: Box<dyn remote::Remote + Send + Sync>,
        service: Box<dyn Service + Send + Sync>,
        config: &BackupConfig,
    ) -> Result<Backup, Error> {
        let parsable = when::parse_when(&config.when).ok();
        let to_parse: &str = parsable.as_deref().unwrap_or(&config.when);

        let schedule = cron::Schedule::from_str(to_parse);
        if schedule.is_err() {
            return Err(Error::InvalidCronConfiguration(schedule.err().unwrap()));
        };

        Ok(Backup {
            name: String::from(name),
            what: service,
            r#where: remote,
            remote_path: PathBuf::from(config.remote_path.clone()),
            when: config.when.clone(),
            compress: config.compress,
            schedule: schedule.unwrap(),
            keep_last: config.keep_last,
        })
    }

    fn log_result(
        result: Result<(), remote::Error>,
        name: &str,
        file: &Path,
        remote_name: &str,
        remote_path: &Path,
        compress: bool,
    ) {
        if result.is_ok() {
            info!(
                "[{}] Successfully uploaded {} {}: {} to [{}] {}",
                name,
                if compress { " and compressed" } else { "" },
                if file.is_dir() { "folder" } else { "file" },
                file.display(),
                remote_name,
                remote_path.display(),
            );
        } else {
            error!(
                "[{}] Error during upload{} of {}: {}. Error: {}",
                name,
                if compress { " or compression" } else { "" },
                file.display(),
                remote_name,
                result.err().unwrap()
            );
        }
    }

    pub async fn schedule(
        self: Arc<Self>,
        scheduler: &mut JobScheduler,
        schedule: cron::Schedule,
    ) -> Result<Uuid, JobSchedulerError> {
        scheduler
            .add(
                Job::new_async(schedule.to_string().as_str(), move |_uuid, _js| {
                    let inst = self.clone();
                    Box::pin(async move {
                        let inst = inst.as_ref();
                        let remote = &inst.r#where;
                        let service = &inst.what;
                        let compress = inst.compress;
                        let name = inst.name.clone();
                        let remote_prefix = inst.remote_path.clone();
                        let keep_last = inst.keep_last;

                        // First call dump, to trigger the dump service if present
                        info!("[{}] Calling dump...", name);
                        let dump = match service.dump().await {
                            Err(error) => {
                                error!("{}", Error::GeneralError(error));
                                return;
                            }
                            Ok(dump) => dump,
                        };

                        let path = dump.path.clone().unwrap_or_default();
                        if path.exists() {
                            // When dump goes out of scope, the dump is removed by Drop.
                            info!("[{}] Dumped {}. Backing it up", name, path.display());
                        }

                        // Then loop over all the dumped files and backup them as specified
                        let mut local_files = service.list().await;

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

                        // Set when any upload fails: the service is told after the uploads.
                        let mut upload_failed = false;

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
                            upload_failed |= result.is_err();
                            Backup::log_result(
                                result,
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

                            upload_failed |= result.is_err();

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
                                            } else {
                                                info!("[{}] Deleted {}", name, delete_me);
                                            }
                                        }
                                    }
                                    Err(error) => {
                                        error!("Error during remote.enumerate: {}", error)
                                    }
                                }
                            }

                            Backup::log_result(
                                result,
                                &name,
                                &file,
                                &remote.name(),
                                &remote_path,
                                compress,
                            );
                        }

                        if upload_failed {
                            service.upload_failed(&dump).await;
                        }

                        info!(
                            "[{}] Next run: {}",
                            name,
                            inst.schedule.upcoming(chrono::Utc).take(1).next().unwrap()
                        );
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
    use croner::Cron;

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

    fn validate_cron_expression(when: &str) {
        let result = when::parse_when(when);
        assert!(
            result.is_ok(),
            "Failed to parse when string '{}': {}",
            when,
            result.err().unwrap()
        );
        let cron_str = result.unwrap();
        // Same configuration of tokio-cron-scheduler
        assert!(
            Cron::from_str(&cron_str).is_ok(),
            "Invalid croner expression '{}' for when string '{}'",
            cron_str,
            when
        );
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
