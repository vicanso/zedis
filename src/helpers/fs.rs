// Copyright 2025 Tree xie.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::error::Error;
use directories::ProjectDirs;
use home::home_dir;
use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

type Result<T, E = Error> = std::result::Result<T, E>;
pub fn copy_dir_recursive(src: &PathBuf, dst: &Path) -> Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if ty.is_dir() {
            continue;
        }
        fs::copy(&src_path, &dst_path)?;
    }
    Ok(())
}

pub fn is_app_store_build() -> bool {
    if let Ok(exe_path) = env::current_exe() {
        let mut receipt_path = exe_path.clone();
        if receipt_path.pop() && receipt_path.pop() {
            receipt_path.push("_MASReceipt");
            receipt_path.push("receipt");
            return receipt_path.exists();
        }
    }
    false
}

pub fn get_or_create_config_dir() -> Result<PathBuf> {
    let Some(project_dirs) = ProjectDirs::from("com", "bigtree", "zedis") else {
        return Err(Error::Invalid {
            message: "project directories not found".to_string(),
        });
    };
    let config_dir = project_dirs.config_dir();
    if !config_dir.exists() {
        fs::create_dir_all(config_dir)?;
    }
    let Some(home) = home_dir() else {
        return Ok(config_dir.to_path_buf());
    };
    let path = home.join(".zedis");
    if path.exists() {
        let _ = copy_dir_recursive(&path, config_dir);
        let _ = fs::remove_dir_all(&path);
    }
    Ok(config_dir.to_path_buf())
}
