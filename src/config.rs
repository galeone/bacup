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

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::string::String;

use std::fmt;
use tokio::{fs, io};

#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct GitConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub private_key: String,
    pub repository: String,
    pub branch: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub private_key: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AwsConfig {
    pub region: String,
    pub endpoint: Option<String>,
    pub access_key: String,
    pub secret_key: String,
    pub force_path_style: Option<bool>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GCloudConfig {
    pub service_account_path: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostgreSqlConfig {
    pub username: String,
    pub db_name: String,
    pub host: Option<String>,
    pub port: Option<u16>,
    /// Optional password, passed to psql/pg_dump via the `PGPASSWORD`
    /// environment variable (libpq). When absent, pg_dump runs with
    /// `--no-password` and relies on peer/trust authentication.
    #[serde(default)]
    pub password: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DockerConfig {
    pub container_name: String,
    pub command: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ZfsConfig {
    pub dataset: String,
    pub snapshot_name: String,
    /// Optional expression controlling how often a full backup is taken. It
    /// accepts the same format as the `when` field (e.g. `"monthly 1 01:00"`)
    /// or a raw cron expression (e.g. `"0 1 1 * *"`). When set, only the runs
    /// where it is due take a full backup and the runs in between are
    /// incrementals against the latest existing snapshot. When absent, every
    /// run is a full backup (previous behavior).
    #[serde(default)]
    pub full_when: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FoldersConfig {
    pub pattern: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct BackupConfig {
    pub what: String,
    pub r#where: String,
    pub when: String,
    pub remote_path: String,
    pub compress: bool,
    pub keep_last: Option<u32>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalhostConfig {
    pub path: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    // remotes
    pub aws: Option<HashMap<String, AwsConfig>>,
    pub gcloud: Option<HashMap<String, GCloudConfig>>,
    pub ssh: Option<HashMap<String, SshConfig>>,
    pub git: Option<HashMap<String, GitConfig>>,
    pub localhost: Option<HashMap<String, LocalhostConfig>>,
    // services
    pub folders: Option<HashMap<String, FoldersConfig>>,
    pub postgres: Option<HashMap<String, PostgreSqlConfig>>,
    pub docker: Option<HashMap<String, DockerConfig>>,
    pub zfs: Option<HashMap<String, ZfsConfig>>,
    // mapping
    pub backup: HashMap<String, BackupConfig>,
}

#[derive(Debug)]
pub enum Error {
    Open(io::Error),
    Parse(toml::de::Error),
}

impl std::error::Error for Error {}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Open(error) => write!(f, "Could not open/read config: {}", error),
            Error::Parse(error) => write!(f, "Failed to parse config: {}", error),
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Error::Open(error)
    }
}

impl From<toml::de::Error> for Error {
    fn from(error: toml::de::Error) -> Self {
        Error::Parse(error)
    }
}

impl Config {
    pub async fn new(path: &Path) -> Result<Config, Error> {
        let txt = fs::read_to_string(path).await?;
        let config: Config = toml::from_str(&txt)?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn misplaced_full_when_is_rejected() {
        // full_when belongs to [zfs.<name>], not to [backup.<name>]: an
        // unknown key must fail the parse instead of being silently ignored.
        let txt = r#"
            [zfs.storage]
            dataset = "storage"
            snapshot_name = "storage-snap"

            [backup.storage]
            what = "zfs.storage"
            where = "aws.bucket"
            when = "daily 22:00"
            remote_path = "/zfs/storage/"
            compress = false
            full_when = "monthly 1 22:00"
        "#;
        let err = toml::from_str::<Config>(txt)
            .err()
            .expect("unknown key rejected");
        assert!(err.to_string().contains("full_when"), "{}", err);
    }

    #[test]
    fn full_when_in_zfs_section_is_accepted() {
        let txt = r#"
            [zfs.storage]
            dataset = "storage"
            snapshot_name = "storage-snap"
            full_when = "monthly 1 22:00"

            [backup.storage]
            what = "zfs.storage"
            where = "aws.bucket"
            when = "daily 22:00"
            remote_path = "/zfs/storage/"
            compress = false
        "#;
        let config: Config = toml::from_str(txt).unwrap();
        let zfs = config.zfs.unwrap();
        assert_eq!(zfs["storage"].full_when.as_deref(), Some("monthly 1 22:00"));
    }
}
