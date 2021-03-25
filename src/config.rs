use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::string::String;
use url::Url;

use std::fmt;
use std::fs;

use toml;

#[derive(Serialize, Deserialize)]
pub struct GitConfig {
    pub host: Url,
    pub port: u16,
    pub username: String,
}

#[derive(Serialize, Deserialize)]
pub struct SSHConfig {
    pub host: Url,
    pub port: u16,
    pub username: String,
}

#[derive(Serialize, Deserialize)]
pub struct AWSConfig {
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
}

#[derive(Serialize, Deserialize)]
pub struct GCloudConfig {
    pub service_account_path: String,
}

#[derive(Serialize, Deserialize)]
pub struct PostgreSQLConfig {
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
    pub incremental: bool,
}

#[derive(Serialize, Deserialize)]
pub struct Config {
    // remotes
    pub aws: Option<HashMap<String, AWSConfig>>,
    pub gcloud: Option<HashMap<String, GCloudConfig>>,
    pub ssh: Option<HashMap<String, SSHConfig>>,
    // services
    pub folders: Option<HashMap<String, FoldersConfig>>,
    pub postgres: Option<HashMap<String, PostgreSQLConfig>>,
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
