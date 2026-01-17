// Copyright 2026 Tree xie.
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

use crate::{
    error::Error,
    helpers::{decrypt, encrypt, get_or_create_config_dir, is_development},
};
use gpui::Action;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use redis::{ClientTlsConfig, TlsCertificates};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use smol::fs;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::{fmt, fs::read_to_string, path::PathBuf, str::FromStr};
use tracing::info;

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, JsonSchema, Action)]
pub enum QueryMode {
    #[default]
    All,
    Prefix,
    Exact,
}

impl fmt::Display for QueryMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            QueryMode::Prefix => "^",
            QueryMode::Exact => "=",
            _ => "*",
        };
        write!(f, "{}", s)
    }
}

impl FromStr for QueryMode {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "^" => Ok(QueryMode::Prefix),
            "=" => Ok(QueryMode::Exact),
            _ => Ok(QueryMode::All),
        }
    }
}

#[derive(Debug, Default, Deserialize, Clone, Serialize, Hash, Eq, PartialEq)]
pub struct RedisServer {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub master_name: Option<String>,
    pub description: Option<String>,
    pub updated_at: Option<String>,
    pub query_mode: Option<String>,
    pub soft_wrap: Option<bool>,
    pub tls: Option<bool>,
    pub insecure: Option<bool>,
    pub client_cert: Option<String>,
    pub client_key: Option<String>,
    pub root_cert: Option<String>,
    pub ssh_tunnel: Option<bool>,
    pub ssh_addr: Option<String>,
    pub ssh_username: Option<String>,
    pub ssh_password: Option<String>,
    pub ssh_key: Option<String>,
}
impl RedisServer {
    pub fn get_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }
    /// Generates the connection URL based on host, port, and optional password.
    pub fn get_connection_url(&self) -> String {
        let tls = self.tls.unwrap_or(false);
        let scheme = if tls { "rediss" } else { "redis" };

        let url = match (&self.password, &self.username) {
            (Some(pwd), Some(username)) => {
                let pwd_enc = utf8_percent_encode(pwd, NON_ALPHANUMERIC).to_string();
                let username_enc = utf8_percent_encode(username, NON_ALPHANUMERIC).to_string();
                format!("{scheme}://{username_enc}:{pwd_enc}@{}:{}", self.host, self.port)
            }
            (Some(pwd), None) => {
                let pwd_enc = utf8_percent_encode(pwd, NON_ALPHANUMERIC).to_string();
                format!("{scheme}://:{pwd_enc}@{}:{}", self.host, self.port)
            }
            _ => format!("{scheme}://{}:{}", self.host, self.port),
        };
        if tls && self.insecure.unwrap_or(false) {
            return format!("{url}/#insecure");
        }

        url
    }
    pub fn tls_certificates(&self) -> Option<TlsCertificates> {
        if !self.tls.unwrap_or(false) {
            return None;
        }
        let mut client_tls = None;
        if let Some(client_cert) = self.client_cert.clone()
            && let Some(client_key) = self.client_key.clone()
        {
            client_tls = Some(ClientTlsConfig {
                client_cert: client_cert.as_bytes().to_vec(),
                client_key: client_key.as_bytes().to_vec(),
            });
        }
        let root_cert = self.root_cert.clone().map(|root_cert| root_cert.as_bytes().to_vec());
        if client_tls.is_none() && root_cert.is_none() {
            return None;
        }
        Some(TlsCertificates { client_tls, root_cert })
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
    if is_development() {
        info!("config file: {}", path.display());
    }
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
    let mut servers = configs.servers;
    for server in servers.iter_mut() {
        if let Some(password) = &server.password {
            server.password = Some(decrypt(password).unwrap_or(password.clone()));
        }
        if let Some(ssh_password) = &server.ssh_password {
            server.ssh_password = Some(decrypt(ssh_password).unwrap_or(ssh_password.clone()));
        }
        if let Some(ssh_key) = &server.ssh_key {
            server.ssh_key = Some(decrypt(ssh_key).unwrap_or(ssh_key.clone()));
        }
    }
    Ok(servers)
}

/// Saves the server configuration to the file.
pub async fn save_servers(mut servers: Vec<RedisServer>) -> Result<()> {
    for server in servers.iter_mut() {
        if let Some(password) = &server.password {
            server.password = Some(encrypt(password)?);
        }
        if let Some(ssh_password) = &server.ssh_password {
            server.ssh_password = Some(encrypt(ssh_password)?);
        }
        if let Some(ssh_key) = &server.ssh_key {
            server.ssh_key = Some(encrypt(ssh_key)?);
        }
    }
    let path = get_or_create_server_config()?;
    let value = toml::to_string(&RedisServers { servers }).map_err(|e| Error::Invalid { message: e.to_string() })?;
    fs::write(&path, value).await?;
    Ok(())
}

/// Retrieves a single server configuration by name.
pub(crate) fn get_config(id: &str) -> Result<RedisServer> {
    let servers = get_servers()?;
    let config = servers.iter().find(|config| config.id == id).ok_or(Error::Invalid {
        message: format!("Redis config not found: {id}"),
    })?;
    Ok(config.clone())
}
