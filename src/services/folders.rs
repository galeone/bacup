use glob::glob;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::services::service::{Dump, Service};

#[derive(Clone)]
pub struct Folder {
    paths: Vec<PathBuf>,
    pattern: String,
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
        for token in vec!["*", "?", "["] {
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
                    paths: vec![],
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
        return Ok(Folder {
            paths: vec![],
            pattern: String::from(path.join("**").join("*").to_str().unwrap()),
        });
    }
}

impl Service for Folder {
    fn list(&self) -> Vec<PathBuf> {
        return self.paths.clone();
    }

    fn dump(&mut self) -> Result<Dump, Box<dyn std::error::Error>> {
        self.paths = glob(&self.pattern)
            .unwrap()
            .map(|pb_ge| pb_ge.unwrap())
            .collect::<Vec<PathBuf>>();

        Ok(Dump { path: None })
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
    fn test_dump_and_list_no_wildcard() {
        let cwd = std::env::current_dir().unwrap();
        let folder = Folder::new(cwd.to_str().unwrap());
        assert!(folder.is_ok());
        let mut folder = folder.unwrap();

        // Dump -> evaluate the pattern
        assert!(folder.dump().is_ok());

        let files = folder.list();
        assert!(files.len() > 0);

        let git_info = cwd.join(".git").join("info");
        assert!(files.contains(&git_info));

        let cargo = cwd.join("LICENSE");
        assert!(files.contains(&cargo));
    }

    #[test]
    fn test_dump_and_list_wildcard() {
        let cwd = std::env::current_dir().unwrap();
        let folder = Folder::new(cwd.join("src").join("*").to_str().unwrap());
        assert!(folder.is_ok());
        let mut folder = folder.unwrap();

        // Dump -> evaluate the pattern
        assert!(folder.dump().is_ok());

        let files = folder.list();
        assert!(files.len() > 0);

        let lib_path = cwd.join("src").join("lib.rs");
        assert!(files.contains(&lib_path));
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
