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
use serde::Deserialize;
use std::fs::read_to_string;
use std::path::PathBuf;

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Default, Deserialize, Clone)]
pub(crate) struct RedisConfig {
    name: String,
    host: String,
    port: u16,
    // TODO 加密
    password: Option<String>,
}
impl RedisConfig {
    pub fn get_connection_url(&self) -> String {
        let addr = format!("{}:{}", self.host, self.port);
        if let Some(password) = &self.password {
            format!("redis://:{password}@{addr}")
        } else {
            format!("redis://{addr}")
        }
    }
}

fn get_or_create_config_dir() -> Result<PathBuf> {
    let Some(home) = home_dir() else {
        return Err(Error::Invalid {
            message: "Home directory not found".to_string(),
        });
    };
    let path = home.join(".zedis");
    if !path.exists() {
        std::fs::create_dir_all(&path)?;
    }
    Ok(path)
}

pub(crate) fn get_config(name: &str) -> Result<RedisConfig> {
    let config_dir = get_or_create_config_dir()?;
    let path = config_dir.join("redis-servers.json");
    let value = read_to_string(path)?;
    let configs: Vec<RedisConfig> = serde_json::from_str(&value)?;
    let config = configs
        .iter()
        .find(|config| config.name == name)
        .ok_or(Error::Invalid {
            message: format!("Redis config not found: {}", name),
        })?;
    Ok(config.clone())
}
