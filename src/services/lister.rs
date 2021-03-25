use std::path::PathBuf;

use dyn_clone::DynClone;

pub trait Lister: DynClone {
    fn list(&self) -> Vec<PathBuf>;
}
