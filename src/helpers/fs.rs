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
use home::home_dir;
use std::path::PathBuf;

type Result<T, E = Error> = std::result::Result<T, E>;

pub fn get_or_create_config_dir() -> Result<PathBuf> {
    let Some(home) = home_dir() else {
        return Err(Error::Invalid {
            message: "home directory not found".to_string(),
        });
    };
    let path = home.join(".zedis");
    if !path.exists() {
        std::fs::create_dir_all(&path)?;
    }
    Ok(path)
}
