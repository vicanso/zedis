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
use arc_swap::ArcSwap;
use gpui::SharedString;
use indexmap::IndexMap;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use redis::{ClientTlsConfig, TlsCertificates};
use serde::{Deserialize, Serialize};
use smol::fs;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::{fs::read_to_string, path::PathBuf, sync::LazyLock};
use tracing::{debug, info};
use url::Url;

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Clone, Default)]
struct RedisUrl {
    host: String,
    port: Option<u16>,
    username: String,
    password: Option<String>,
    tls: bool,
}

fn parse_url(host: SharedString) -> RedisUrl {
    let input_to_parse = if host.contains("://") {
        host.to_string()
    } else {
        format!("redis://{host}")
    };
    if let Ok(u) = Url::parse(input_to_parse.as_str()) {
        let host = u.host_str().unwrap_or("");
        let port = u.port();
        RedisUrl {
            host: host.to_string(),
            port,
            username: u.username().to_string(),
            password: u.password().map(|p| p.to_string()),
            tls: u.scheme() == "rediss",
        }
    } else {
        RedisUrl {
            host: host.to_string(),
            ..Default::default()
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
    pub server_type: Option<usize>,
    pub master_name: Option<String>,
    pub description: Option<String>,
    pub updated_at: Option<String>,
    pub tls: Option<bool>,
    pub insecure: Option<bool>,
    pub client_cert: Option<String>,
    pub client_key: Option<String>,
    pub root_cert: Option<String>,
    pub ssh_tunnel: Option<bool>,
    pub readonly: Option<bool>,
    pub ssh_addr: Option<String>,
    pub ssh_username: Option<String>,
    pub ssh_password: Option<String>,
    pub ssh_key: Option<String>,
}
impl RedisServer {
    pub fn from_form_data(id: &str, data: &IndexMap<SharedString, SharedString>) -> Self {
        let get_str = |k: &str| data.get(k).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

        let get_parsed = |k: &str| get_str(k).and_then(|s| s.parse().ok());

        let get_bool = |k: &str| get_str(k).map(|s| s == "true" || s == "1");
        let redis_url = parse_url(get_str("host").unwrap_or_default().into());
        let mut username = get_str("username");
        if username.is_none() && !redis_url.username.is_empty() {
            username = Some(redis_url.username.clone());
        }
        let mut password = get_str("password");
        if password.is_none() && redis_url.password.is_some() {
            password = redis_url.password.clone();
        }
        let mut tls = get_bool("tls");
        if redis_url.tls {
            tls = Some(true);
        }

        Self {
            id: id.to_string(),

            name: get_str("name").unwrap_or_default(),
            host: redis_url.host,
            port: get_parsed("port").unwrap_or_else(|| redis_url.port.unwrap_or(6379)),

            username,
            password,
            master_name: get_str("master_name"),
            description: get_str("description"),
            updated_at: None,

            client_cert: get_str("client_cert"),
            client_key: get_str("client_key"),
            root_cert: get_str("root_cert"),

            ssh_addr: get_str("ssh_addr"),
            ssh_username: get_str("ssh_username"),
            ssh_password: get_str("ssh_password"),
            ssh_key: get_str("ssh_key"),

            server_type: get_parsed("server_type").map(|s| s as usize),

            tls,
            insecure: get_bool("insecure"),
            ssh_tunnel: get_bool("ssh_tunnel"),
            readonly: get_bool("readonly"),
        }
    }
    pub fn get_hash(&self, db: usize) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        db.hash(&mut hasher);
        hasher.finish()
    }
    pub fn is_ssh_tunnel(&self) -> bool {
        self.ssh_tunnel.unwrap_or(false) && self.ssh_addr.as_ref().map(|addr| !addr.is_empty()).unwrap_or(false)
    }
    /// Generates the connection URL based on host, port, and optional password.
    pub fn get_connection_url(&self) -> String {
        let tls = self.tls.unwrap_or(false);
        let scheme = if tls { "rediss" } else { "redis" };

        let safe_pwd = self.password.as_deref().filter(|s| !s.trim().is_empty());
        let safe_usr = self.username.as_deref().filter(|s| !s.trim().is_empty());

        let url = match (safe_pwd, safe_usr) {
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
    debug!(file = path.display().to_string(), "get or create server config");
    if path.exists() {
        return Ok(path);
    }
    std::fs::write(&path, "")?;
    Ok(path)
}

static SERVER_CONFIG_MAP: LazyLock<ArcSwap<HashMap<String, RedisServer>>> =
    LazyLock::new(|| ArcSwap::from_pointee(HashMap::new()));

pub fn get_servers() -> Result<Vec<RedisServer>> {
    if !SERVER_CONFIG_MAP.load().is_empty() {
        let mut servers: Vec<RedisServer> = SERVER_CONFIG_MAP.load().values().cloned().collect();
        servers.sort_by(|a, b| a.id.cmp(&b.id));
        return Ok(servers);
    }
    let path = get_or_create_server_config()?;
    let value = read_to_string(path)?;
    if value.is_empty() {
        return Ok(vec![]);
    }
    let configs: RedisServers = toml::from_str(&value)?;
    let mut servers = configs.servers;
    let mut configs = HashMap::new();
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
        configs.insert(server.id.clone(), server.clone());
    }
    SERVER_CONFIG_MAP.store(Arc::new(configs));
    Ok(servers)
}

/// Saves the server configuration to the file.
pub async fn save_servers(mut servers: Vec<RedisServer>) -> Result<()> {
    let mut configs = HashMap::new();
    for server in servers.iter_mut() {
        configs.insert(server.id.clone(), server.clone());
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

    // Compare with existing configs and log differences
    let old_configs = SERVER_CONFIG_MAP.load();

    // Check for new or modified configs
    for (id, new_server) in configs.iter() {
        if let Some(old_server) = old_configs.get(id) {
            if old_server.get_hash(0) != new_server.get_hash(0) {
                debug!(name = new_server.name, "modified config");
            }
        } else {
            debug!(name = new_server.name, "new config");
        }
    }

    // Check for deleted configs
    for (id, old_server) in old_configs.iter() {
        if !configs.contains_key(id) {
            debug!(name = old_server.name, "deleted config");
        }
    }

    SERVER_CONFIG_MAP.store(Arc::new(configs));
    let path = get_or_create_server_config()?;
    let value = toml::to_string(&RedisServers { servers }).map_err(|e| Error::Invalid { message: e.to_string() })?;
    fs::write(&path, value).await?;
    Ok(())
}

/// Retrieves a single server configuration by name.
pub fn get_server(id: &str) -> Result<RedisServer> {
    if let Some(server) = SERVER_CONFIG_MAP.load().get(id) {
        return Ok(server.clone());
    }
    let servers = get_servers()?;
    let config = servers.iter().find(|config| config.id == id).ok_or(Error::Invalid {
        message: format!("Redis config not found: {id}"),
    })?;
    Ok(config.clone())
}
