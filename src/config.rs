use serde::{Deserialize, Serialize};
use snafu::{ResultExt, Snafu};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::string::String;
use url::Url;

use std::fs;
use toml;

#[derive(Serialize, Deserialize)]
pub struct Git {
    pub host: Url,
    pub port: u16,
    pub username: String,
}

#[derive(Serialize, Deserialize)]
pub struct SSH {
    pub host: Url,
    pub port: u16,
    pub username: String,
}

#[derive(Serialize, Deserialize)]
pub struct AWS {
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
}

#[derive(Serialize, Deserialize)]
pub struct GCloud {
    pub service_account_path: String,
}

#[derive(Serialize, Deserialize)]
pub struct PostgreSQL {
    pub username: String,
    pub db_name: String,
}

#[derive(Serialize, Deserialize)]
pub struct Folder {
    pub path: String,
}

#[derive(Serialize, Deserialize)]
pub struct Config {
    pub aws: Option<HashMap<String, AWS>>,
    pub gcloud: Option<HashMap<String, GCloud>>,
    pub ssh: Option<HashMap<String, SSH>>,
}

#[derive(Debug, Snafu)]
pub enum ConfigError {
    #[snafu(display("Could not open/read config from {}: {}", filename.display(), source))]
    Open {
        filename: PathBuf,
        source: std::io::Error,
    },
    #[snafu(display("Failed to parse {}: {}", filename.display(), source))]
    Parse {
        filename: PathBuf,
        source: toml::de::Error,
    },
}

impl Config {
    pub fn read(path: &Path) -> Result<Config, ConfigError> {
        let txt = fs::read_to_string(path).context(Open { filename: path })?;
        let config: Config = toml::from_str(&txt).context(Parse { filename: path })?;
        Ok(config)
    }
}
