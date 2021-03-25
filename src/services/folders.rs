use glob::glob;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::services::lister::Lister;

#[derive(Clone)]
pub struct Folder {
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, PartialEq)]
pub enum Error {
    IsNotAbsolute(PathBuf),
    DoesNotExist(PathBuf),
}

impl std::error::Error for Error {}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::IsNotAbsolute(path) => write!(f, "Path {} is not absolute", path.display()),
            Error::DoesNotExist(path) => write!(f, "Path {} does not exist", path.display()),
        }
    }
}

impl Folder {
    pub fn new(pattern: &str) -> Result<Folder, Error> {
        if pattern.contains("*") {
            let base_path = pattern.split("*").next().unwrap();
            let base_path = Path::new(base_path);

            if !base_path.is_absolute() {
                return Err(Error::IsNotAbsolute(PathBuf::from(base_path)));
            }

            if !base_path.exists() {
                return Err(Error::DoesNotExist(PathBuf::from(base_path)));
            }

            return Ok(Folder {
                paths: glob(pattern)
                    .unwrap()
                    .map(|pb_ge| pb_ge.unwrap())
                    .collect::<Vec<PathBuf>>(),
            });
        }
        let path = Path::new(pattern);
        if !path.is_absolute() {
            return Err(Error::IsNotAbsolute(PathBuf::from(path)));
        }
        if !path.exists() {
            return Err(Error::DoesNotExist(PathBuf::from(path)));
        }
        return Ok(Folder {
            paths: vec![PathBuf::from(path)],
        });
    }
}

impl Lister for Folder {
    fn list(&self) -> Vec<PathBuf> {
        return self.paths.clone();
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn test_new_relative() {
        let relative = "relative";
        assert!(Folder::new(relative).is_err());
        assert_eq!(
            Folder::new(relative).err(),
            Some(Error::IsNotAbsolute(PathBuf::from(relative)))
        );
    }

    #[test]
    fn test_new_absolute() {
        let cwd = std::env::current_dir().unwrap();
        assert!(Folder::new(cwd.to_str().unwrap()).is_ok());
    }

    #[test]
    fn test_wildcard() {
        let cwd = std::env::current_dir().unwrap();
        let pattern = cwd.join("*");
        let folder = Folder::new(pattern.to_str().unwrap());
        assert!(folder.is_ok());
        let folder = folder.unwrap();

        let cargo = cwd.join("Cargo.toml");
        assert!(folder.paths.contains(&cargo));
    }

    #[test]
    fn test_non_existing_abolute() {
        let cwd = std::env::current_dir()
            .unwrap()
            .parent()
            .unwrap()
            .join("fakfakefakefake");
        assert!(Folder::new(cwd.to_str().unwrap()).is_err());
    }
}
