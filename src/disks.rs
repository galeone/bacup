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

use crate::disks::Error::LocalError;

#[derive(Debug)]
pub enum Error {
    LocalError(std::io::Error),
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        LocalError(error)
    }
}

pub async fn calculate_folder_size(path: &Path) -> Result<u64, Error> {
    let mut total_size = 0u64;

    let mut entries = fs::read_dir(path).await?;
    while let Some(entry_result) = entries.next_entry().await? {
        let metadata = fs::metadata(entry_result.path()).await?;

        if metadata.is_file() {
            total_size += metadata.len();
        } else if metadata.is_dir() {
            total_size += Box::pin(calculate_folder_size(&entry_result.path())).await?;
        }
    }

    Ok(total_size)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_calculate_folder_size_empty_dir() {
        let dir = tempdir().unwrap();
        let size = calculate_folder_size(dir.path()).await.unwrap();
        assert_eq!(size, 0);
    }

    #[tokio::test]
    async fn test_calculate_folder_size_single_file() {
        let dir = tempdir().unwrap();
        let test_file = dir.path().join("test.txt");
        fs::write(&test_file, "Hello, World!").unwrap();
        let size = calculate_folder_size(dir.path()).await.unwrap();
        assert_eq!(size, 13); // "Hello, World!".len()
    }

    #[tokio::test]
    async fn test_calculate_folder_size_nested() {
        let dir = tempdir().unwrap();
        let subdir = dir.path().join("subdir");
        fs::create_dir(&subdir).unwrap();

        let file1 = dir.path().join("file1.txt");
        let c1 = "Content 1";
        fs::write(&file1, c1).unwrap();

        let file2 = subdir.join("file2.txt");
        let c2 = "Content 2 more content";
        fs::write(&file2, c2).unwrap();

        let size = calculate_folder_size(dir.path()).await.unwrap();
        assert_eq!(size, (c1.len() + c2.len()) as u64); // "Content 1".len() + "Content 2 more content".len()
    }

    #[tokio::test]
    async fn test_calculate_folder_size_subdirectory() {
        let dir = tempdir().unwrap();
        let subdir = dir.path().join("subdir");
        fs::create_dir(&subdir).unwrap();

        let file = subdir.join("nested.txt");
        let c1 = "Nested content";
        fs::write(&file, c1).unwrap();

        let size = calculate_folder_size(dir.path()).await.unwrap();
        assert_eq!(size, c1.len() as u64); // "Nested content".len()
    }

    #[tokio::test]
    async fn test_has_enough_space_sufficient_space() {
        let dir = tempdir().unwrap();
        let test_file = dir.path().join("test.txt");
        fs::write(&test_file, "test").unwrap();

        let result = has_enough_space(&test_file, 100).await.unwrap();
        assert!(result, "Should have enough space");
    }

    #[tokio::test]
    async fn test_has_enough_space_insufficient_space() {
        let dir = tempdir().unwrap();
        let test_file = dir.path().join("test.txt");
        fs::write(&test_file, "test").unwrap();

        // Use a very large number to test the boundary condition
        let result = has_enough_space(&test_file, u64::MAX / 2).await.unwrap();
        assert!(!result, "Should not have enough space");
    }

    #[tokio::test]
    async fn test_has_enough_space_parent_not_exists() {
        let test_file = PathBuf::from("/nonexistent/path/test.txt");
        let result = has_enough_space(&test_file, 100).await;
        assert!(result.is_err());
    }
}

