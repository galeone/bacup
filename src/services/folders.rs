use std::path::{Path, PathBuf};

use glob::glob;

pub struct Folder {
    pub paths: Vec<PathBuf>,
}

#[derive(Debug)]
pub enum FolderError {
    IsNotAbsolute,
    DoesNotExist(PathBuf),
}

impl Folder {
    pub fn new(pattern: &str) -> Result<Folder, FolderError> {
        if pattern.contains("*") {
            let base_path = pattern.split("*").next().unwrap();
            let base_path = Path::new(base_path);
            if !base_path.is_absolute() {
                return Err(FolderError::IsNotAbsolute);
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
            return Err(FolderError::IsNotAbsolute);
        }
        return Ok(Folder {
            paths: vec![PathBuf::from(path)],
        });
    }
}
