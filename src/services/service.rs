use std::path::PathBuf;

use dyn_clone::DynClone;

pub struct Dump {
    pub path: Option<PathBuf>,
}

impl Drop for Dump {
    fn drop(&mut self) {
        match self.path.clone() {
            Some(path) => {
                // If we created a dump file, we should take care of removing it
                if path.exists() {
                    #[allow(unused_must_use)]
                    {
                        std::fs::remove_file(&path);
                    }
                }
            }
            None => {}
        }
    }
}

pub trait Service: DynClone {
    fn dump(&mut self) -> Result<Dump, Box<dyn std::error::Error>>;
    fn list(&self) -> Vec<PathBuf>;
}
