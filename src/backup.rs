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
use crate::remotes::uploader;
use crate::services::service::Service;

use job_scheduler::{Job, JobScheduler};
use regex::Regex;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use chrono::Weekday;
use log::{error, info};

use futures::executor;

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
    pub what: Box<dyn Service>,
    pub r#where: Box<dyn uploader::Uploader>,
    pub remote_path: PathBuf,
    pub when: String,
    pub compress: bool,
    pub schedule: cron::Schedule,
    pub keep_last: Option<u32>,
}

impl Backup {
    fn get_hours_and_minutes(when: &str) -> Option<(i8, i8)> {
        let re = Regex::new(r"(\d{2}):(\d{2})").unwrap();
        let cap = re.captures(when)?;

        let ret: (i8, i8) = (cap[1].parse().unwrap(), cap[2].parse().unwrap());
        if (0..24).contains(&ret.0) && (0..60).contains(&ret.1) {
            return Some(ret);
        }
        None
    }

    fn parse_daily(input: &str) -> Result<String, Error> {
        // Daily 12:30
        let daily = "daily";
        if input.contains(daily) {
            let input = input.replace(daily, "");

            let hm = Self::get_hours_and_minutes(&input);
            if hm.is_none() {
                return Err(Error::InvalidWhenConfiguration(String::from(
                    "Unable to find hours:minutes",
                )));
            }
            let hm = hm.unwrap();
            let input = input.replace(&format!("{:02}:{:02}", hm.0, hm.1), "");
            let input = input.trim();
            if !input.is_empty() {
                return Err(Error::InvalidWhenConfiguration(format!(
                    "Expected to consume all the when string, unable to parse \
                    remaining part: {}",
                    input
                )));
            }

            // sec   min   hour   day of month   month   day of week   year
            return Ok(format!(
                "{} {} {} {} {} {} {}",
                0, hm.1, hm.0, "*", "*", "*", "*"
            ));
        }
        Err(Error::InvalidWhenConfiguration(String::from(
            "Unable to find daily identifier",
        )))
    }

    fn parse_weekly(input: &str) -> Result<String, Error> {
        // Monday 15:40 or Weekly Monday 15:40
        let weekdays = vec![
            (Weekday::Mon, "Monday"),
            (Weekday::Tue, "Tuesday"),
            (Weekday::Wed, "Wednesday"),
            (Weekday::Thu, "Thursday"),
            (Weekday::Fri, "Friday"),
            (Weekday::Sat, "Saturday"),
            (Weekday::Sun, "Sunday"),
        ];

        let weekdays = weekdays.iter().map(|d| {
            (
                d.0.to_string().to_lowercase(),
                String::from(d.1).to_lowercase(),
            )
        });
        for day in weekdays {
            let short = input.contains(&day.0);
            let long = input.contains(&day.1);
            if short || long {
                let input = input.replace(if long { &day.1 } else { &day.0 }, "");
                let hm = Backup::get_hours_and_minutes(&input);
                if hm.is_none() {
                    return Err(Error::InvalidWhenConfiguration(String::from(
                        "Unable to find hours:minutes",
                    )));
                }
                let hm = hm.unwrap();
                let input = input.replace(&format!("{:02}:{:02}", hm.0, hm.1), "");
                let input = input.trim();
                if !vec!["", "weekly"].contains(&input) {
                    return Err(Error::InvalidWhenConfiguration(format!(
                        "Expected to consume all the when string, unable to parse \
                        remaining part: {}",
                        input
                    )));
                }
                let day = Weekday::from_str(&day.0).unwrap().number_from_monday();

                // sec   min   hour   day of month   month   day of week   year
                return Ok(format!(
                    "{} {} {} {} {} {} {}",
                    0, hm.1, hm.0, "*", "*", day, "*"
                ));
            }
        }
        Err(Error::InvalidWhenConfiguration(String::from(
            "Unable to find any weekday identifier",
        )))
    }

    fn parse_monthly(input: &str) -> Result<String, Error> {
        // Monthly 1 12:40
        let monthly = "monthly";
        if input.contains(monthly) {
            let input = input.replace(monthly, "");
            let hm = Backup::get_hours_and_minutes(&input);
            if hm.is_none() {
                return Err(Error::InvalidWhenConfiguration(String::from(
                    "Unable to find hours:minutes",
                )));
            }
            let hm = hm.unwrap();
            let input = input.replace(&format!("{:02}:{:02}", hm.0, hm.1), "");
            let input = input.trim();
            // Input should now contain only the "day of the month"

            let day: i8 = match input.parse() {
                Ok(day) => day,
                Err(error) => {
                    return Err(Error::InvalidWhenConfiguration(format!(
                        "Unable to correctly parse the string for the day of the month. \
                        Given input: {}. Error: {}",
                        input, error
                    )))
                }
            };

            let valid_days = 1..32;
            if !valid_days.contains(&day) {
                return Err(Error::InvalidWhenConfiguration(String::from(
                    "Invalid day of the month specified, out of range [1,31]",
                )));
            }

            // sec   min   hour   day of month   month   day of week   year
            return Ok(format!(
                "{} {} {} {} {} {} {}",
                0, hm.1, hm.0, day, "*", "*", "*"
            ));
        }
        Err(Error::InvalidWhenConfiguration(String::from(
            "Unable to find monthly identifier",
        )))
    }

    fn parse_when(when: &str) -> Result<String, Error> {
        // sec   min   hour   day of month   month   day of week   year
        // *     *     *      *              *       *             *
        let input = when.to_lowercase();
        let daily = Backup::parse_daily(&input);
        if daily.is_ok() {
            return daily;
        }

        let monthly = Backup::parse_monthly(&input);
        if monthly.is_ok() {
            return monthly;
        }

        let weekly = Backup::parse_weekly(&input);
        if weekly.is_ok() {
            return weekly;
        }

        Err(Error::InvalidWhenConfiguration(format!(
            "Unable to parse for:\n\
        Daily: {}\n
        Weekly: {}\n
        Monthly: {}",
            daily.unwrap_err(),
            weekly.unwrap_err(),
            monthly.unwrap_err()
        )))
    }
    pub fn new(
        name: &str,
        remote: Box<dyn uploader::Uploader>,
        service: Box<dyn Service>,
        config: &BackupConfig,
    ) -> Result<Backup, Error> {
        let when_to_schedule = Backup::parse_when(&config.when);
        let to_parse: &str;
        let parsable: String;
        if let Ok(value) = when_to_schedule {
            parsable = value;
            to_parse = &parsable;
        } else {
            to_parse = &config.when;
        };

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

    pub fn schedule(
        self,
        scheduler: &mut JobScheduler,
        schedule: cron::Schedule,
    ) -> Result<(), Error> {
        let remote = self.r#where;
        let mut service = self.what;
        let compress = self.compress;
        let name = self.name;
        let remote_prefix = self.remote_path;
        let keep_last = self.keep_last;

        let log_result = |result: Result<(), uploader::Error>,
                          name: &str,
                          file: &Path,
                          remote_name: &str,
                          remote_path: &Path,
                          compress: bool| {
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
                    if file.is_dir() { "folder" } else { "file" },
                    file.display(),
                    result.err().unwrap()
                );
            }
        };

        let job = Job::new(self.schedule, move || {
            // First call dump, to trigger the dump service if present
            let dump = match service.dump() {
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
            let mut local_files = service.list();

            // If the local_files list contains a single file, the upload should be in the form:
            // /remote/prefix/filename
            // even if the local file is in /local/path/in/folder/filename
            let mut single_file = local_files.len() <= 1;

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

            // Special case in which we want to upload a folder without compression
            // If all the files share the same prefix, we upload all the files in this prefix.
            // The remote should handle eventual incremental backup.
            if !single_file && all_with_same_prefix && !compress {
                let remote_path = &remote_prefix;
                let result = executor::block_on(remote.upload_folder(&local_files, remote_path));
                log_result(
                    result,
                    &name,
                    local_prefix,
                    &remote.name(),
                    &remote_path,
                    compress,
                );
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

                let result: Result<(), uploader::Error>;
                if file.is_dir() {
                    // compress for sure, the uncompressed scenarios has been treated
                    // outside this loop
                    result =
                        executor::block_on(remote.upload_folder_compressed(&file, &remote_path));
                } else if compress {
                    result = executor::block_on(remote.upload_file_compressed(&file, &remote_path));
                    if let Some(to_keep) = keep_last {
                        match executor::block_on(remote.enumerate(&remote_path.parent().unwrap())) {
                            Ok(list) => {
                                info!("OK list for remote_path {}", remote_path.display());
                                for f in &list {
                                    info!("{}", f);
                                }
                                if list.len() > to_keep as usize {}
                            }
                            Err(error) => error!("Error during remote.enumerate: {}", error),
                        }
                    }
                } else {
                    result = executor::block_on(remote.upload_file(&file, &remote_path));
                }

                log_result(result, &name, &file, &remote.name(), &remote_path, compress);
            }

            info!(
                "[{}] Next run: {}",
                name,
                schedule.upcoming(chrono::Utc).take(1).next().unwrap()
            );
        });
        scheduler.add(job);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_when_daily() {
        let result = Backup::parse_when("daily 00:00");
        assert!(result.is_ok(), "{}", result.err().unwrap());
        let result = Backup::parse_when("daily 12:30");
        assert!(result.is_ok(), "{}", result.err().unwrap());
        let result = Backup::parse_when("Daily 00:00");
        assert!(result.is_ok(), "{}", result.err().unwrap());
        let result = Backup::parse_when("DAILY 11:11");
        assert!(result.is_ok(), "{}", result.err().unwrap());

        let result = Backup::parse_when("dayly 00:00");
        assert!(result.is_err());
        let result = Backup::parse_when("daily 55:00");
        assert!(result.is_err());
        let result = Backup::parse_when("daily 00:61");
        assert!(result.is_err());
        let result = Backup::parse_when("daily 00:60");
        assert!(result.is_err());
        let result = Backup::parse_when("daily 24:01");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_when_weekly() {
        let result = Backup::parse_when("weekly monday 12:30");
        assert!(result.is_ok(), "{}", result.err().unwrap());
        let result = Backup::parse_when("weekly mon 12:30");
        assert!(result.is_ok(), "{}", result.err().unwrap());

        let result = Backup::parse_when("weekly tuesday 12:30");
        assert!(result.is_ok(), "{}", result.err().unwrap());
        let result = Backup::parse_when("weekly tue 12:30");
        assert!(result.is_ok(), "{}", result.err().unwrap());

        let result = Backup::parse_when("weekly wednesday 12:30");
        assert!(result.is_ok(), "{}", result.err().unwrap());
        let result = Backup::parse_when("weekly wed 12:30");
        assert!(result.is_ok(), "{}", result.err().unwrap());

        let result = Backup::parse_when("weekly thursday 12:30");
        assert!(result.is_ok(), "{}", result.err().unwrap());
        let result = Backup::parse_when("weekly thu 12:30");
        assert!(result.is_ok(), "{}", result.err().unwrap());

        let result = Backup::parse_when("weekly friday 12:30");
        assert!(result.is_ok(), "{}", result.err().unwrap());
        let result = Backup::parse_when("weekly fri 12:30");
        assert!(result.is_ok(), "{}", result.err().unwrap());

        let result = Backup::parse_when("weekly Saturday 12:30");
        assert!(result.is_ok(), "{}", result.err().unwrap());

        let result = Backup::parse_when("weekly Sat 12:30");
        assert!(result.is_ok(), "{}", result.err().unwrap());

        let result = Backup::parse_when("WEEKLY SUN 12:30");
        assert!(result.is_ok(), "{}", result.err().unwrap());
        let result = Backup::parse_when("weekly sunday 12:30");
        assert!(result.is_ok(), "{}", result.err().unwrap());

        let result = Backup::parse_when(" SUN 12:30");
        assert!(result.is_ok(), "{}", result.err().unwrap());
        let result = Backup::parse_when(" sunday 12:30");
        assert!(result.is_ok(), "{}", result.err().unwrap());

        // Errors
        assert!(Backup::parse_when("watly monzay 00:00").is_err());
        assert!(Backup::parse_when("monzay 00:00").is_err());
        assert!(Backup::parse_when("Moonday 00:00").is_err());
        assert!(Backup::parse_when("Sundays 1:00").is_err());
        assert!(Backup::parse_when("Today 00:00").is_err());
        assert!(Backup::parse_when("Tomorrow 00:00").is_err());
        assert!(Backup::parse_when("Toyota -1:00").is_err());
    }

    #[test]
    fn test_parse_when_montly() {
        let result = Backup::parse_when("Monthly 1 02:30");
        assert!(result.is_ok(), "{}", result.err().unwrap());

        let result = Backup::parse_when("Monthly 31 02:30");
        assert!(result.is_ok(), "{}", result.err().unwrap());

        assert!(Backup::parse_when("Monthly 00:00").is_err());
        assert!(Backup::parse_when("Monthtly -1 00:00").is_err());
        assert!(Backup::parse_when("Monthtly 0 00:00").is_err());
        assert!(Backup::parse_when("Monthtly 32 00:00").is_err());
    }
}
