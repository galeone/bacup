// Copyright 2021 Paolo Galeone <nessuno@nerdz.eu>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

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
