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
use uuid::Uuid;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Preset tag color keys, ordered to match the RadioGroup option list.
/// Index 0 = "none" (no chip rendered).
pub const TAG_COLOR_PRESETS: &[&str] = &["none", "gray", "blue", "green", "amber", "red"];

fn tag_color_from_form_value(form_value: Option<&str>) -> Option<String> {
    let raw = form_value?.trim();
    if raw.is_empty() {
        return None;
    }
    // Form may pass either the index (RadioGroup) or the preset key (existing config).
    if let Ok(idx) = raw.parse::<usize>() {
        let key = *TAG_COLOR_PRESETS.get(idx)?;
        if key == "none" {
            return None;
        }
        return Some(key.to_string());
    }
    if raw == "none" {
        return None;
    }
    Some(raw.to_string())
}

pub fn tag_color_index(value: Option<&str>) -> usize {
    let Some(v) = value else { return 0 };
    TAG_COLOR_PRESETS.iter().position(|k| *k == v).unwrap_or(0)
}

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
    pub tag: Option<String>,
    pub tag_color: Option<String>,
    pub require_confirm_writes: Option<bool>,
    /// Optional grouping label. Servers with the same `group` string
    /// render under one section header on the servers page. Distinct
    /// from `tag` — tag describes risk/role (PROD/DEV), group
    /// describes ownership/team (Team A / Team B / Personal).
    /// `None` or empty → "Ungrouped" section.
    pub group: Option<String>,
    /// Stable sort key within `(group)`. Lower values render first.
    /// Assigned automatically on save when not present (max+1 across
    /// all servers in the same group). Hand-editing the TOML to
    /// renumber is supported.
    pub sort_order: Option<i64>,
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
            tag: get_str("tag"),
            tag_color: tag_color_from_form_value(get_str("tag_color").as_deref()),
            require_confirm_writes: get_bool("require_confirm_writes"),
            group: get_str("group"),
            // sort_order is owned by reorder buttons / drag-drop, not
            // the edit form. Preserve the existing value (the caller
            // fills it in via `upsert_server`).
            sort_order: None,
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

    /// Serialize this server config to JSON for team sharing.
    ///
    /// `include_secrets=false` (the default for the share UI) blanks
    /// out every credential field — password, SSH password, SSH
    /// private key, and the three TLS materials. The receiving user
    /// then fills those in locally. This is the safe default because
    /// the JSON is destined for a chat / wiki / repo where any
    /// plaintext secret would be a leak.
    ///
    /// `include_secrets=true` keeps everything verbatim — useful for
    /// personal backups (e.g. moving to a new machine) but should
    /// never be shared.
    pub fn to_export_json(&self, include_secrets: bool) -> serde_json::Result<String> {
        let mut clone = self.clone();
        // Strip transient identity bits so the importer treats this
        // as a fresh entry (avoids overwriting an existing config
        // with the same id) and so the export survives across redb
        // versions even if id semantics change.
        clone.id = String::new();
        clone.updated_at = None;
        // sort_order is install-local — the receiver will be assigned
        // a fresh index by upsert_server. Group label is kept since
        // it's a meaningful organizational hint that often carries
        // across teammates.
        clone.sort_order = None;
        if !include_secrets {
            clone.password = None;
            clone.ssh_password = None;
            clone.ssh_key = None;
            clone.client_cert = None;
            clone.client_key = None;
            clone.root_cert = None;
        }
        serde_json::to_string_pretty(&clone)
    }

    /// Parse a JSON blob produced by [`Self::to_export_json`] (or
    /// hand-edited) into a `RedisServer`. The result always gets a
    /// fresh `id` so importing the same JSON twice yields two
    /// distinct entries, and never silently overwrites whatever the
    /// user currently has under that id.
    ///
    /// Returns `Err` for malformed JSON, missing required fields
    /// (name / host / port), or extreme port values. Empty optional
    /// fields are preserved as `None`.
    pub fn from_import_json(json: &str) -> Result<Self, String> {
        let mut server: RedisServer = serde_json::from_str(json).map_err(|e| format!("invalid JSON: {e}"))?;
        // Validate the bare minimum so a typo doesn't ship a broken
        // entry to the list view (which would crash on connect).
        if server.name.trim().is_empty() {
            return Err("missing required field: name".into());
        }
        if server.host.trim().is_empty() {
            return Err("missing required field: host".into());
        }
        if server.port == 0 {
            return Err("port must be a positive number".into());
        }
        // Always allocate a fresh id so import is idempotent and
        // never clobbers an existing entry.
        server.id = Uuid::now_v7().to_string();
        server.updated_at = None;
        // sort_order is install-local — drop the sender's index so
        // the importer appends to the tail of whatever group it
        // lands in (max+1 assigned by upsert_server).
        server.sort_order = None;
        Ok(server)
    }
    pub fn tag_label(&self) -> Option<&str> {
        self.tag.as_deref().map(|s| s.trim()).filter(|s| !s.is_empty())
    }
    /// Returns true when the tag implies production-grade caution: typed-name confirm,
    /// no "remember choice" shortcut. Driven by tag_color preset key, not by tag text.
    pub fn is_high_risk_tag(&self) -> bool {
        matches!(self.tag_color.as_deref(), Some("red"))
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

/// Returns the canonical sort order: group label A→Z (case-insensitive,
/// empty/None group sorts last under the "Ungrouped" header), then
/// within each group by `sort_order` ascending, then by `name`
/// case-insensitive as a stable tiebreaker. A `sort_order` of `None`
/// is treated as `i64::MAX` so brand-new servers (not yet renumbered)
/// land at the tail rather than at the head.
fn server_sort_key(server: &RedisServer) -> (u8, String, i64, String) {
    let group = server.group.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let bucket = if group.is_some() { 0 } else { 1 };
    let group_key = group.map(|s| s.to_ascii_lowercase()).unwrap_or_default();
    let order = server.sort_order.unwrap_or(i64::MAX);
    let name_key = server.name.to_ascii_lowercase();
    (bucket, group_key, order, name_key)
}

pub fn get_servers() -> Result<Vec<RedisServer>> {
    if !SERVER_CONFIG_MAP.load().is_empty() {
        let mut servers: Vec<RedisServer> = SERVER_CONFIG_MAP.load().values().cloned().collect();
        servers.sort_by_key(server_sort_key);
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
    servers.sort_by_key(server_sort_key);
    Ok(servers)
}

/// Returns the distinct, trimmed, non-empty group labels currently in
/// use across all configured servers, sorted case-insensitively. The
/// servers form uses this to surface an autocomplete-style hint list
/// so teammates don't end up with "Team A" / "team a" duplicates.
pub fn get_server_groups() -> Vec<String> {
    let map = SERVER_CONFIG_MAP.load();
    let mut groups: Vec<String> = map
        .values()
        .filter_map(|s| {
            s.group
                .as_deref()
                .map(str::trim)
                .filter(|g| !g.is_empty())
                .map(String::from)
        })
        .collect();
    groups.sort_by_key(|g| g.to_ascii_lowercase());
    groups.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    groups
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_server() -> RedisServer {
        RedisServer {
            id: "src-id".into(),
            name: "prod-cache".into(),
            host: "10.0.0.5".into(),
            port: 6379,
            username: Some("admin".into()),
            password: Some("supersecret".into()),
            ssh_tunnel: Some(true),
            ssh_addr: Some("bastion:22".into()),
            ssh_username: Some("ops".into()),
            ssh_key: Some("-----BEGIN OPENSSH PRIVATE KEY-----\n...".into()),
            updated_at: Some("2026-05-14T08:00:00".into()),
            tag: Some("PROD".into()),
            tag_color: Some("red".into()),
            ..Default::default()
        }
    }

    #[test]
    fn export_strips_secrets_by_default() {
        let json = sample_server().to_export_json(false).expect("serialize");
        // Plain identifiers should be there.
        assert!(json.contains("\"prod-cache\""));
        assert!(json.contains("10.0.0.5"));
        // Secrets must be absent.
        assert!(!json.contains("supersecret"));
        assert!(!json.contains("PRIVATE KEY"));
        // Source id should be blanked so import allocates fresh.
        assert!(json.contains("\"id\": \"\""));
    }

    #[test]
    fn export_with_secrets_keeps_credentials() {
        let json = sample_server().to_export_json(true).expect("serialize");
        assert!(json.contains("supersecret"));
        assert!(json.contains("PRIVATE KEY"));
    }

    #[test]
    fn import_assigns_fresh_id() {
        let exported = sample_server().to_export_json(false).expect("serialize");
        let imported = RedisServer::from_import_json(&exported).expect("import");
        assert_ne!(imported.id, "src-id");
        assert!(!imported.id.is_empty());
        assert_eq!(imported.name, "prod-cache");
        // Stripped secrets remain None.
        assert!(imported.password.is_none());
        assert!(imported.ssh_key.is_none());
    }

    #[test]
    fn import_rejects_missing_required_fields() {
        let bad = r#"{"id":"","name":"","host":"h","port":1}"#;
        assert!(RedisServer::from_import_json(bad).is_err());
        let bad2 = r#"{"id":"","name":"n","host":"","port":1}"#;
        assert!(RedisServer::from_import_json(bad2).is_err());
        let bad3 = r#"{"id":"","name":"n","host":"h","port":0}"#;
        assert!(RedisServer::from_import_json(bad3).is_err());
    }

    #[test]
    fn import_rejects_malformed_json() {
        assert!(RedisServer::from_import_json("not json").is_err());
    }

    #[test]
    fn export_strips_install_local_sort_order_but_keeps_group() {
        let mut s = sample_server();
        s.group = Some("Team A".into());
        s.sort_order = Some(42);
        let json = s.to_export_json(false).expect("serialize");
        assert!(json.contains("\"Team A\""));
        // sort_order should be `null` (Option::None) in the exported
        // JSON — the receiver's upsert assigns its own.
        assert!(json.contains("\"sort_order\": null"));
        let imported = RedisServer::from_import_json(&json).expect("import");
        assert_eq!(imported.group.as_deref(), Some("Team A"));
        assert!(imported.sort_order.is_none());
    }

    #[test]
    fn server_sort_key_orders_groups_then_sort_order_then_name() {
        let mk = |group: Option<&str>, order: Option<i64>, name: &str| RedisServer {
            id: name.into(),
            name: name.into(),
            host: "h".into(),
            port: 1,
            group: group.map(String::from),
            sort_order: order,
            ..Default::default()
        };
        let mut servers = [
            mk(None, None, "zeta"),
            mk(Some("B"), Some(0), "b0"),
            mk(Some("A"), Some(1), "a1"),
            mk(Some("A"), Some(0), "a0"),
            mk(None, Some(0), "alpha-ungrouped"),
        ];
        servers.sort_by_key(server_sort_key);
        let names: Vec<&str> = servers.iter().map(|s| s.name.as_str()).collect();
        // Grouped first (alphabetical group, then sort_order), then
        // ungrouped (sort_order then name).
        assert_eq!(names, vec!["a0", "a1", "b0", "alpha-ungrouped", "zeta"]);
    }
}
