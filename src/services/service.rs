use std::path::PathBuf;

use dyn_clone::DynClone;

pub struct Dump {
    pub path: Option<PathBuf>,
}

impl Drop for Dump {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            // If we created a dump file, we should take care of removing it
            if path.exists() {
                #[allow(unused_must_use)]
                {
                    std::fs::remove_file(&path);
                }
            }
        }
    }
}

pub trait Service: DynClone {
    fn dump(&mut self) -> Result<Dump, Box<dyn std::error::Error>>;
    fn list(&self) -> Vec<PathBuf>;
}
