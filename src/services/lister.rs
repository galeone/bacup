use std::path::PathBuf;

pub trait Lister {
    fn list(&self) -> Vec<PathBuf>;
}
