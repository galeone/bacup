// Copyright 2022-2026 Paolo Galeone <nessuno@nerdz.eu>
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

use std::collections::HashMap;
use std::path::Path;

use sysinfo::Disks;
use tokio::fs;

#[derive(Debug)]
pub enum Error {
    LocalError(std::io::Error),
}

pub async fn has_enough_space(destination_file: &Path, required_space: u64) -> Result<bool, Error> {
    let destination_path =
        destination_file
            .parent()
            .ok_or(Error::LocalError(std::io::Error::other(
                "parent no".to_string(),
            )))?;
    if !destination_path.exists() {
        Err(Error::LocalError(std::io::Error::other(
            "parent no exist".to_string(),
        )))
    } else {
        // Resolve symlinks
        let destination_path = fs::canonicalize(destination_path).await.unwrap();
        let disks = Disks::new_with_refreshed_list();
        let potential_mountpoints = disks
            .list()
            .iter()
            .filter(|disk| destination_path.starts_with(disk.mount_point()));
        // The longer path with this prefix, is the correect disk
        let mut path_space = HashMap::new();
        for mp in potential_mountpoints {
            path_space.insert(mp.mount_point(), mp.available_space());
        }
        // Get the longest mount point first
        let mount_point = path_space
            .iter()
            .max_by_key(|(k, _)| k.to_str().unwrap_or("").len())
            .map(|(k, _)| k.to_owned())
            .ok_or(Error::LocalError(std::io::Error::other(
                "no mounts".to_string(),
            )))?;
        let available_space = path_space
            .get(&mount_point)
            .ok_or(Error::LocalError(std::io::Error::other("??".to_string())))?;
        Ok(required_space < *available_space)
    }
}
