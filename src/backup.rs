use crate::config::BackupConfig;
use crate::remotes::uploader::Uploader;
use crate::services::service::Service;

use job_scheduler::{Job, JobScheduler};
use regex::Regex;
use std::fmt;
use std::io;
use std::path::PathBuf;
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
    pub r#where: Box<dyn Uploader>,
    pub remote_path: PathBuf,
    pub when: String,
    pub compress: bool,
    pub incremental: bool,
    pub schedule: cron::Schedule,
}

impl Backup {
    fn get_hours_and_minutes(when: &str) -> Option<(i8, i8)> {
        let re = Regex::new(r"(\d{2}):(\d{2})").unwrap();
        let cap = re.captures(when);

        if cap.is_none() {
            return None;
        }
        let cap = cap.unwrap();

        let ret: (i8, i8) = (cap[1].parse().unwrap(), cap[2].parse().unwrap());
        if (0..24).contains(&ret.0) && (0..60).contains(&ret.1) {
            return Some(ret);
        }
        return None;
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
            if input != "" {
                return Err(Error::InvalidWhenConfiguration(String::from(format!(
                    "Expected to consume all the when string, unable to parse \
                    remeaining part: {}",
                    input
                ))));
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
                    return Err(Error::InvalidWhenConfiguration(String::from(format!(
                        "Expected to consume all the when string, unable to parse \
                        remeaining part: {}",
                        input
                    ))));
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
                    return Err(Error::InvalidWhenConfiguration(String::from(format!(
                        "Unable to correctly parse the string for the day of the month. \
                        Given input: {}. Error: {}",
                        input, error
                    ))))
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
        let input = when.clone().to_lowercase();
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

        Err(Error::InvalidWhenConfiguration(String::from(format!(
            "Unable to parse for:\n\
        Daily: {}\n
        Weekly: {}\n
        Montly: {}",
            daily.unwrap_err(),
            weekly.unwrap_err(),
            monthly.unwrap_err()
        ))))
    }
    pub fn new(
        name: &str,
        remote: Box<dyn Uploader>,
        service: Box<dyn Service>,
        config: BackupConfig,
    ) -> Result<Backup, Error> {
        let when_to_schedule = Backup::parse_when(&config.when);
        let to_parse: &str;
        let parsable: String;
        if when_to_schedule.is_err() {
            to_parse = &config.when;
        } else {
            parsable = when_to_schedule.unwrap();
            to_parse = &parsable;
        }

        let schedule = cron::Schedule::from_str(to_parse);
        if schedule.is_err() {
            return Err(Error::InvalidCronConfiguration(schedule.err().unwrap()));
        };

        Ok(Backup {
            name: String::from(name),
            what: service,
            r#where: remote,
            remote_path: PathBuf::from(config.remote_path),
            when: config.when,
            compress: config.compress,
            incremental: config.incremental,
            schedule: schedule.unwrap(),
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
        let job = Job::new(self.schedule, move || {
            // First call dump, to trigger the dump service if present
            let dump = match service.dump() {
                Err(error) => {
                    error!("{}", Error::GeneralError(error));
                    return ();
                }
                Ok(dump) => dump,
            };

            let path = dump.path.clone().unwrap_or(PathBuf::new());
            if path.exists() {
                // When dump goes out of scope, the dump is removed by Drop.
                info!("[{}] Dumped {}. Backing it up", name, path.display());
            }

            // Then loop over all the dumped files and backup them as specified
            let local_files = service.list();
            for file in local_files {
                if file.is_dir() {
                    if compress {
                        let result =
                            executor::block_on(remote.upload_folder_compressed(file.clone()));
                        if result.is_ok() {
                            info!(
                                "[{}] Successfully uploaded and compressed folder {} to {}",
                                name,
                                file.display(),
                                remote.name()
                            );
                        } else {
                            error!(
                                "[{}] Error during upload/compression of folder {}. Error: {}",
                                name,
                                file.display(),
                                result.err().unwrap()
                            );
                        }
                    } else {
                        let result = executor::block_on(remote.upload_folder(file.clone()));
                        if result.is_ok() {
                            info!(
                                "[{}] Successfully uploaded folder {} to {}",
                                name,
                                file.display(),
                                remote.name()
                            );
                        } else {
                            error!(
                                "Error during upload of folder {}. Error: {}",
                                file.display(),
                                result.err().unwrap()
                            );
                        }
                    }
                } else {
                    if compress {
                        let result =
                            executor::block_on(remote.upload_file_compressed(file.clone()));
                        if result.is_ok() {
                            info!(
                                "[{}] Successfully uploaded and compressed file {} to {}",
                                name,
                                file.display(),
                                remote.name()
                            );
                        } else {
                            error!(
                                "[{}] Error during upload/compression of file {}. Error: {}",
                                name,
                                file.display(),
                                result.err().unwrap()
                            );
                        }
                    } else {
                        let result = executor::block_on(remote.upload_file(file.clone()));
                        if result.is_ok() {
                            info!(
                                "[{}] Successfully uploaded file {} to {}",
                                name,
                                file.display(),
                                remote.name()
                            );
                        } else {
                            error!(
                                "[{}] Error during upload of file {}. Error: {}",
                                name,
                                file.display(),
                                result.err().unwrap()
                            );
                        }
                    }
                }
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