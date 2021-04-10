use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::string::String;

use std::fmt;
use std::fs;

#[derive(Serialize, Deserialize)]
pub struct GitConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub private_key: String,
    pub branch: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub private_key: String,
}

#[derive(Serialize, Deserialize)]
pub struct AwsConfig {
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
}

#[derive(Serialize, Deserialize)]
pub struct GCloudConfig {
    pub service_account_path: String,
}

#[derive(Serialize, Deserialize)]
pub struct PostgreSqlConfig {
    pub username: String,
    pub db_name: String,
    pub host: Option<String>,
    pub port: Option<u16>,
}

#[derive(Serialize, Deserialize)]
pub struct FoldersConfig {
    pub pattern: String,
}

#[derive(Serialize, Deserialize)]
pub struct BackupConfig {
    pub what: String,
    pub r#where: String,
    pub when: String,
    pub remote_path: String,
    pub compress: bool,
}

#[derive(Serialize, Deserialize)]
pub struct LocalhostConfig {
    pub path: String,
}

#[derive(Serialize, Deserialize)]
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
    // mapping
    pub backup: HashMap<String, BackupConfig>,
}

#[derive(Debug)]
pub enum Error {
    Open(std::io::Error),
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

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::Open(error)
    }
}

impl From<toml::de::Error> for Error {
    fn from(error: toml::de::Error) -> Self {
        Error::Parse(error)
    }
}

impl Config {
    pub fn new(path: &Path) -> Result<Config, Error> {
        let txt = fs::read_to_string(path)?;
        let config: Config = toml::from_str(&txt)?;
        Ok(config)
    }
}
