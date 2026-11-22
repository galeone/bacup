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

use glob::glob;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;

use tokio::fs;

use crate::services::service::{Dump, Service};

#[derive(Clone)]
pub struct Folder {
    pattern: String,
    /// When set, dump() fails if the newest file matched by the pattern is
    /// older than this.
    max_age: Option<Duration>,
}

#[derive(Debug, PartialEq)]
pub enum Error {
    IsNotAbsolute(PathBuf),
    DoesNotExist(PathBuf),
    /// max_age is set and the pattern matches no files.
    NoFiles(String),
    /// max_age is set and the newest file is older than it.
    Stale {
        newest: PathBuf,
        age: Duration,
        max_age: Duration,
    },
}

impl Error {
    /// True for the errors of the max_age freshness check.
    pub fn is_freshness(&self) -> bool {
        matches!(self, Error::NoFiles(_) | Error::Stale { .. })
    }
}

impl std::error::Error for Error {}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::IsNotAbsolute(path) => write!(f, "Path {} is not absolute", path.display()),
            Error::DoesNotExist(path) => write!(f, "Path {} does not exist", path.display()),
            Error::NoFiles(pattern) => write!(
                f,
                "stale input: no files match {pattern}, nothing to check max_age against"
            ),
            Error::Stale {
                newest,
                age,
                max_age,
            } => write!(
                f,
                "stale input: the newest file, {}, is {} old (max_age {}). Is whatever writes it still running?",
                newest.display(),
                format_duration(*age),
                format_duration(*max_age),
            ),
        }
    }
}

/// Parses a `max_age` value: one or more `<number><unit>` parts, with unit
/// `s`, `m`, `h` or `d` (e.g. `"26h"`, `"30m"`, `"1d 2h"`, `"1d2h"`).
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let invalid =
        || format!("invalid duration \"{text}\": use e.g. \"30m\", \"26h\", \"2d\" or \"1d 2h\"");
    let mut total: u64 = 0;
    let mut number = String::new();
    let mut parts = 0;
    for c in text.trim().chars() {
        if c.is_ascii_digit() {
            number.push(c);
        } else if c.is_whitespace() {
            if !number.is_empty() {
                return Err(invalid());
            }
        } else {
            let seconds: u64 = match c.to_ascii_lowercase() {
                's' => 1,
                'm' => 60,
                'h' => 3600,
                'd' => 86400,
                _ => return Err(invalid()),
            };
            let value: u64 = number.parse().map_err(|_| invalid())?;
            number.clear();
            total = value
                .checked_mul(seconds)
                .and_then(|part| total.checked_add(part))
                .ok_or_else(invalid)?;
            parts += 1;
        }
    }
    if !number.is_empty() || parts == 0 {
        return Err(invalid());
    }
    if total == 0 {
        return Err(format!(
            "invalid duration \"{text}\": must be greater than zero"
        ));
    }
    Ok(Duration::from_secs(total))
}

/// `31h 2m`, `45m`, `12s`: hours and minutes, seconds only under a minute.
pub fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let (hours, minutes) = (seconds / 3600, seconds % 3600 / 60);
    match (hours, minutes) {
        (0, 0) => format!("{seconds}s"),
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

impl Folder {
    pub async fn new(pattern: &str) -> Result<Folder, Error> {
        for token in &["*", "?", "["] {
            if pattern.contains(token) {
                let base_path = pattern.split(token).next().unwrap();
                let base_path = Path::new(base_path);

                if !base_path.is_absolute() {
                    return Err(Error::IsNotAbsolute(PathBuf::from(base_path)));
                }

                if !base_path.exists() {
                    return Err(Error::DoesNotExist(PathBuf::from(base_path)));
                }

                return Ok(Folder {
                    pattern: String::from(pattern),
                    max_age: None,
                });
            }
        }

        // Single file backup support - not a pattern nor a folder.
        if let Ok(meta) = fs::metadata(pattern).await {
            if meta.is_file() {
                return Ok(Folder {
                    pattern: String::from(pattern),
                    max_age: None,
                });
            }
        }
        let path = Path::new(pattern);
        if !path.is_absolute() {
            return Err(Error::IsNotAbsolute(PathBuf::from(path)));
        }
        if !path.exists() {
            return Err(Error::DoesNotExist(PathBuf::from(path)));
        }
        Ok(Folder {
            pattern: String::from(path.join("**").join("*").to_str().unwrap()),
            max_age: None,
        })
    }

    /// Makes dump() fail when the newest file under the pattern is older
    /// than `max_age`, so stale input is reported instead of uploaded.
    pub fn with_max_age(mut self, max_age: Option<Duration>) -> Folder {
        self.max_age = max_age;
        self
    }

    /// The newest regular file matched by the pattern and its mtime.
    /// Directories are skipped: their mtime also changes when entries are
    /// deleted. Unreadable entries are skipped too.
    fn newest_file(&self) -> Option<(PathBuf, SystemTime)> {
        glob(&self.pattern)
            .ok()?
            .filter_map(Result::ok)
            .filter_map(|path| {
                let meta = std::fs::metadata(&path).ok()?;
                if !meta.is_file() {
                    return None;
                }
                Some((path, meta.modified().ok()?))
            })
            .max_by_key(|(_, modified)| *modified)
    }

    /// Ok when max_age is unset or the newest file is recent enough.
    fn check_freshness(&self, now: SystemTime) -> Result<(), Error> {
        let Some(max_age) = self.max_age else {
            return Ok(());
        };
        let Some((newest, modified)) = self.newest_file() else {
            return Err(Error::NoFiles(self.pattern.clone()));
        };
        // A file from the future (clock skew) counts as fresh.
        let age = now.duration_since(modified).unwrap_or_default();
        if age > max_age {
            return Err(Error::Stale {
                newest,
                age,
                max_age,
            });
        }
        Ok(())
    }
}

#[async_trait]
impl Service for Folder {
    async fn list(&self) -> Vec<PathBuf> {
        glob(&self.pattern)
            .unwrap()
            .map(|pb_ge| pb_ge.unwrap())
            .collect::<Vec<PathBuf>>()
    }

    async fn dump(&self) -> Result<Dump, Box<dyn std::error::Error>> {
        self.check_freshness(SystemTime::now())?;
        Ok(Dump { path: None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[tokio::test]
    async fn test_new_relative() {
        let relative = "relative";
        assert!(Folder::new(relative).await.is_err());
        assert_eq!(
            Folder::new(relative).await.err(),
            Some(Error::IsNotAbsolute(PathBuf::from(relative)))
        );
    }

    #[tokio::test]
    async fn test_new_absolute_folder() {
        let cwd = env::current_dir().unwrap();
        assert!(Folder::new(cwd.to_str().unwrap()).await.is_ok());
    }

    #[tokio::test]
    async fn test_new_absolute_file() {
        let cwd = env::current_dir().unwrap().join("Cargo.toml");
        assert!(Folder::new(cwd.to_str().unwrap()).await.is_ok());
    }

    #[tokio::test]
    async fn test_dump_and_list_no_wildcard_folder() {
        let cwd = env::current_dir().unwrap();
        let folder = Folder::new(cwd.to_str().unwrap()).await;
        assert!(folder.is_ok());
        let folder = folder.unwrap();

        // Dump -> evaluate the pattern
        assert!(folder.dump().await.is_ok());

        let files = folder.list().await;
        assert!(!files.is_empty());

        let git_info = cwd.join(".git").join("info");
        assert!(files.contains(&git_info));

        let cargo = cwd.join("LICENSE");
        assert!(files.contains(&cargo));
    }

    #[tokio::test]
    async fn test_dump_and_list_no_wildcard_single_file() {
        let cwd = env::current_dir().unwrap();
        let file = cwd.join("Cargo.toml");

        let folder = Folder::new(file.to_str().unwrap()).await;
        assert!(folder.is_ok());
        let folder = folder.unwrap();

        // Dump -> evaluate the pattern
        assert!(folder.dump().await.is_ok());

        let files = folder.list().await;
        assert!(files.len() == 1);
    }

    #[tokio::test]
    async fn test_dump_and_list_wildcard() {
        let cwd = env::current_dir().unwrap();
        let folder = Folder::new(cwd.join("src").join("*").to_str().unwrap()).await;
        assert!(folder.is_ok());
        let folder = folder.unwrap();

        // Dump -> evaluate the pattern
        assert!(folder.dump().await.is_ok());

        let files = folder.list().await;
        assert!(!files.is_empty());

        let lib_path = cwd.join("src").join("lib.rs");
        assert!(files.contains(&lib_path));
    }

    mod max_age {
        use super::super::*;
        use std::fs::File;
        use tempfile::TempDir;

        const HOUR: Duration = Duration::from_secs(3600);

        /// Creates `name` under `dir` with an mtime `age` ago.
        fn file_aged(dir: &Path, name: &str, age: Duration) -> PathBuf {
            let path = dir.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "x").unwrap();
            File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(SystemTime::now() - age)
                .unwrap();
            path
        }

        async fn folder(dir: &TempDir, max_age: &str) -> Folder {
            Folder::new(dir.path().to_str().unwrap())
                .await
                .unwrap()
                .with_max_age(Some(parse_duration(max_age).unwrap()))
        }

        #[tokio::test]
        async fn a_fresh_folder_passes() {
            let dir = TempDir::new().unwrap();
            file_aged(dir.path(), "old.dump", 72 * HOUR);
            file_aged(dir.path(), "MANIFEST", HOUR);
            assert!(folder(&dir, "26h").await.dump().await.is_ok());
        }

        #[tokio::test]
        async fn a_stale_folder_fails() {
            let dir = TempDir::new().unwrap();
            file_aged(dir.path(), "database.dump", 31 * HOUR);
            let newest = file_aged(dir.path(), "MANIFEST", 30 * HOUR);
            let folder = folder(&dir, "26h").await;
            match folder.check_freshness(SystemTime::now()) {
                Err(Error::Stale {
                    newest: found,
                    age,
                    max_age,
                }) => {
                    assert_eq!(found, newest);
                    assert!(age >= 30 * HOUR && age < 31 * HOUR, "{:?}", age);
                    assert_eq!(max_age, 26 * HOUR);
                }
                other => panic!("expected Stale, got {:?}", other),
            }
            let error = folder.dump().await.err().unwrap().to_string();
            assert!(error.contains("MANIFEST, is 30h"), "{}", error);
            assert!(error.contains("(max_age 26h)"), "{}", error);
        }

        #[tokio::test]
        async fn a_new_file_in_a_subfolder_counts() {
            let dir = TempDir::new().unwrap();
            file_aged(dir.path(), "old.dump", 72 * HOUR);
            file_aged(dir.path(), "2026/11/22/new.dump", HOUR);
            assert!(folder(&dir, "6h").await.dump().await.is_ok());
        }

        #[tokio::test]
        async fn directories_do_not_count_as_fresh() {
            // A new, empty subfolder bumps no file mtime: still stale.
            let dir = TempDir::new().unwrap();
            file_aged(dir.path(), "old.dump", 72 * HOUR);
            std::fs::create_dir(dir.path().join("new-empty-dir")).unwrap();
            let error = folder(&dir, "6h").await.dump().await.err().unwrap();
            assert!(error.to_string().contains("old.dump"), "{}", error);
        }

        #[tokio::test]
        async fn no_files_fails() {
            let dir = TempDir::new().unwrap();
            std::fs::create_dir(dir.path().join("empty-subdir")).unwrap();
            let folder = folder(&dir, "6h").await;
            assert!(matches!(
                folder.check_freshness(SystemTime::now()),
                Err(Error::NoFiles(_))
            ));
        }

        #[tokio::test]
        async fn without_max_age_old_files_are_fine() {
            let dir = TempDir::new().unwrap();
            file_aged(dir.path(), "old.dump", 720 * HOUR);
            let folder = Folder::new(dir.path().to_str().unwrap()).await.unwrap();
            assert!(folder.dump().await.is_ok());
        }

        #[tokio::test]
        async fn single_file_pattern() {
            let dir = TempDir::new().unwrap();
            let path = file_aged(dir.path(), "config.toml", 10 * 24 * HOUR);
            let folder = Folder::new(path.to_str().unwrap())
                .await
                .unwrap()
                .with_max_age(Some(8 * 24 * HOUR));
            assert!(folder.dump().await.is_err());
            let folder = folder.with_max_age(Some(11 * 24 * HOUR));
            assert!(folder.dump().await.is_ok());
        }

        #[test]
        fn parse_durations() {
            let ok = |text: &str, seconds: u64| {
                assert_eq!(
                    parse_duration(text),
                    Ok(Duration::from_secs(seconds)),
                    "{}",
                    text
                )
            };
            ok("26h", 26 * 3600);
            ok("30m", 1800);
            ok("90s", 90);
            ok("2d", 2 * 86400);
            ok("1d2h", 86400 + 7200);
            ok(" 1d 2h 30m ", 86400 + 7200 + 1800);
            ok("6H", 6 * 3600);
            for bad in [
                "",
                "26",
                "h",
                "26x",
                "1.5h",
                "-1h",
                "0h",
                "1 h",
                "26hours",
                "99999999999999999999d",
            ] {
                assert!(parse_duration(bad).is_err(), "{:?} accepted", bad);
            }
        }

        #[test]
        fn format_durations() {
            assert_eq!(format_duration(Duration::from_secs(12)), "12s");
            assert_eq!(format_duration(Duration::from_secs(45 * 60 + 5)), "45m");
            assert_eq!(format_duration(26 * HOUR), "26h");
            assert_eq!(
                format_duration(31 * HOUR + Duration::from_secs(120)),
                "31h 2m"
            );
        }
    }

    #[tokio::test]
    async fn test_non_existing_abolute() {
        let cwd = env::current_dir()
            .unwrap()
            .parent()
            .unwrap()
            .join("fakfakefakefake");
        assert!(Folder::new(cwd.to_str().unwrap()).await.is_err());
    }
}
