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
use crate::helpers::get_or_create_config_dir;
use serde::{Deserialize, Serialize};
use smol::fs;
use std::fs::read_to_string;
use std::path::PathBuf;

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Default, Deserialize, Clone, Serialize)]
pub struct RedisServer {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub password: Option<String>,
    pub master_name: Option<String>,
    pub description: Option<String>,
    pub updated_at: Option<String>,
}
impl RedisServer {
    /// Generates the connection URL based on host, port, and optional password.
    pub fn get_connection_url(&self) -> String {
        match &self.password {
            Some(pwd) => format!("redis://:{pwd}@{}:{}", self.host, self.port),
            None => format!("redis://{}:{}", self.host, self.port),
        }
    }
}

/// Wrapper struct to match the TOML `[[servers]]` structure.
#[derive(Debug, Default, Deserialize, Clone, Serialize)]
pub(crate) struct RedisServers {
    servers: Vec<RedisServer>,
}

/// Gets or creates the path to the server configuration file.
fn get_or_create_server_config() -> Result<PathBuf> {
    let config_dir = get_or_create_config_dir()?;
    let path = config_dir.join("redis-servers.toml");
    if path.exists() {
        return Ok(path);
    }
    std::fs::write(&path, "")?;
    Ok(path)
}

pub fn get_servers() -> Result<Vec<RedisServer>> {
    let path = get_or_create_server_config()?;
    let value = read_to_string(path)?;
    if value.is_empty() {
        return Ok(vec![]);
    }
    let configs: RedisServers = toml::from_str(&value)?;
    Ok(configs.servers)
}

/// Saves the server configuration to the file.
pub async fn save_servers(servers: Vec<RedisServer>) -> Result<()> {
    let path = get_or_create_server_config()?;
    let value = toml::to_string(&RedisServers { servers }).map_err(|e| Error::Invalid {
        message: e.to_string(),
    })?;
    fs::write(&path, value).await?;
    Ok(())
}

/// Retrieves a single server configuration by name.
pub(crate) fn get_config(id: &str) -> Result<RedisServer> {
    let path = get_or_create_server_config()?;
    let value = read_to_string(path)?;
    // TODO 密码是否应该加密
    // 是否使用toml
    let configs: RedisServers = toml::from_str(&value)?;
    let config = configs
        .servers
        .iter()
        .find(|config| config.id == id)
        .ok_or(Error::Invalid {
            message: format!("Redis config not found: {}", id),
        })?;
    Ok(config.clone())
}
