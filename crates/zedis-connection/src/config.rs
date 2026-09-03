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

use crate::error::Error;
use crate::string::{decrypt, encrypt};
use arc_swap::ArcSwap;
use indexmap::IndexMap;
use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use redis::{ClientTlsConfig, TlsCertificates};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use smol::unblock;
use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::{path::PathBuf, sync::LazyLock};
use tracing::{debug, error, info, warn};
use url::Url;
use uuid::Uuid;
use zedis_core::env::is_development;
use zedis_core::fs::{
    ConfigRecovery, get_or_create_config_dir, load_config_with_recovery, write_file_atomic_with_backup,
};
use zedis_core::string::{bracket_ipv6, strip_ipv6_brackets};

type Result<T, E = Error> = std::result::Result<T, E>;

/// Preset tag color keys, ordered to match the RadioGroup option list.
/// Index 0 = "none" (no chip rendered).
/// Tag color preset keys, ordered Local → Dev → UAT → Prod → Archive.
/// Environment presets offered in the server form, paired 1:1 with
/// [`TAG_ENV_LABELS`] by index. `magenta` is the production / high-risk
/// color (see [`RedisServer::is_high_risk_tag`]). Retired keys (`sky`,
/// `slate`, `green`, `red`, ...) are no longer offered but still render via
/// `canonical_tag_key`, so older servers keep their colored chip.
pub const TAG_COLOR_PRESETS: &[&str] = &["none", "teal", "purple", "magenta"];

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

/// User-facing environment names for the server form's single "Environment"
/// select, index-aligned with [`TAG_COLOR_PRESETS`] (None→none, Dev→teal,
/// UAT→purple, Prod→magenta). These double as the persisted display tag, so
/// they are intentionally kept as stable English identifiers (not localized)
/// — like the color preset keys they pair with.
pub const TAG_ENV_LABELS: &[&str] = &["None", "Dev", "UAT", "Prod"];

/// Display label for a canonical tag-color preset key (e.g. `magenta` → `Prod`).
/// Returns `None` for `none`/unknown so a server with no environment stays
/// unlabeled.
fn env_label_for_key(key: &str) -> Option<&'static str> {
    let idx = TAG_COLOR_PRESETS.iter().position(|k| *k == key)?;
    let label = *TAG_ENV_LABELS.get(idx)?;
    (label != "None").then_some(label)
}

#[derive(Debug, Clone, Default)]
struct RedisUrl {
    host: String,
    port: Option<u16>,
    username: String,
    password: Option<String>,
    tls: bool,
}

fn parse_url(host: String) -> RedisUrl {
    let input_to_parse = if host.contains("://") {
        host.to_string()
    } else {
        format!("redis://{host}")
    };
    if let Ok(u) = Url::parse(input_to_parse.as_str()) {
        // `url` keeps the brackets on an IPv6 host; the stored host is the
        // bare literal, bracketed again only where a URL is built.
        let host = strip_ipv6_brackets(u.host_str().unwrap_or(""));
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

/// Reduce a multi-host connection string — the comma-separated
/// `scheme://[userinfo@]host1:p1,host2:p2,…` extension some clients use for
/// cluster/sentinel seeds — to a standard single-host URI by dropping every
/// address after the first, keeping any trailing path/query intact. Only
/// commas in the host part count: a raw comma is legal inside userinfo, so
/// a password like `pa,ss` passes through untouched. Returns the input
/// unchanged when there is no host list (the common case).
fn keep_first_host(uri: &str) -> Cow<'_, str> {
    let Some(auth_start) = uri.find("://").map(|i| i + 3) else {
        return Cow::Borrowed(uri);
    };
    let auth_end = uri[auth_start..]
        .find(['/', '?', '#'])
        .map_or(uri.len(), |i| auth_start + i);
    let host_start = uri[auth_start..auth_end]
        .rfind('@')
        .map_or(auth_start, |i| auth_start + i + 1);
    match uri[host_start..auth_end].find(',') {
        Some(i) => Cow::Owned(format!("{}{}", &uri[..host_start + i], &uri[auth_end..])),
        None => Cow::Borrowed(uri),
    }
}

/// `RedisServer::server_type` discriminants — the server form's radio order
/// (Auto · Standalone · Sentinel · Cluster) and what `manager.rs`'s
/// `From<usize> for ServerType` reads back. `SERVER_TYPE_AUTO` (like `None`)
/// lets the pool detect the topology from `ROLE` / `INFO cluster`; the other
/// three pin it and *skip* detection. So an importer that writes the wrong
/// number does worse than mislabel a server: it switches off the detection
/// that would have corrected it.
pub const SERVER_TYPE_AUTO: usize = 0;
pub const SERVER_TYPE_STANDALONE: usize = 1;
pub const SERVER_TYPE_SENTINEL: usize = 2;
pub const SERVER_TYPE_CLUSTER: usize = 3;

/// One saved connection (`redis-servers.toml`). `#[serde(default)]` is the
/// upgrade contract: an entry written by any earlier version must parse, so
/// a new field is always optional or defaulted — see `upgrade_fixtures` in
/// the tests, which loads real old files.
#[derive(Debug, Default, Deserialize, Clone, Serialize, Hash, Eq, PartialEq)]
#[serde(default)]
pub struct RedisServer {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub server_type: Option<usize>,
    /// User-set number of selectable logical databases. When `None` the
    /// count is probed at connect via `CONFIG GET databases`. Lets managed
    /// clouds that block `CONFIG` (e.g. ElastiCache) or Valkey cluster
    /// (which, unlike Redis cluster, allows multi-db) still get the db
    /// switcher by setting it explicitly.
    pub databases: Option<usize>,
    /// Database this server opens on, pinned by the user. `None` (the
    /// default) reopens whichever DB the server was last viewed on — DB 0
    /// the first time. Set it when one DB of a multi-DB server is "the"
    /// working set, so every connect lands there instead of needing a
    /// manual switch.
    ///
    /// `u16` because the real ceiling is the server's own `databases`
    /// count, which isn't known until connect: an out-of-range pin is
    /// accepted here and surfaces as Redis's own "DB index is out of
    /// range" on the first SELECT.
    pub default_db: Option<u16>,
    /// Per-server connection-establishment timeout in **seconds**. When
    /// unset, falls back to the global setting — handy on a flaky link
    /// where the global default is too slow to fail.
    pub connection_timeout: Option<u64>,
    /// Per-server per-command response timeout in **seconds**. When
    /// unset, falls back to the global setting.
    pub response_timeout: Option<u64>,
    /// Character used to split Redis key names into key-tree folders.
    /// When unset or empty, defaults to `":"`.
    pub key_separator: Option<String>,
    /// Keys fetched per SCAN round. When unset, falls back to the global
    /// Settings value (default 10_000).
    pub key_scan_count: Option<usize>,
    /// Max key-tree nesting depth. When unset, falls back to global Settings.
    pub max_key_tree_depth: Option<usize>,
    /// Auto-expand folders when total keys are below this count. When unset,
    /// falls back to global Settings.
    pub auto_expand_threshold: Option<usize>,
    /// Show TTL chips in the key tree. `None` = use global Settings default.
    pub show_key_tree_ttl: Option<bool>,
    pub master_name: Option<String>,
    /// Credentials for the Sentinel nodes themselves, when they differ from
    /// the data nodes' — `username` / `password` always authenticate the
    /// master and replicas Sentinel points at. Both empty means the
    /// sentinels take the same credentials, with the legacy "retry without
    /// a password" fallback for an auth-less sentinel in front of a
    /// protected master.
    pub sentinel_username: Option<String>,
    /// Encrypted at rest like `password`.
    pub sentinel_password: Option<String>,
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
    /// Passphrase for an encrypted `ssh_key` — distinct from `ssh_password`
    /// (login-password auth) so the two never share a field's semantics.
    /// Encrypted at rest like the other secrets.
    pub ssh_key_passphrase: Option<String>,
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

/// Recursively drop `null` entries from JSON objects so exported configs stay
/// terse. Import is unaffected: every optional field deserializes to `None`
/// when its key is absent.
fn strip_null_fields(value: serde_json::Value) -> serde_json::Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k, strip_null_fields(v)))
                .collect(),
        ),
        Value::Array(arr) => Value::Array(arr.into_iter().map(strip_null_fields).collect()),
        other => other,
    }
}

/// Why an import failed, surfaced to the paste-to-import dialog. Kept as a
/// typed enum (not a `String`) so the UI layer can localize each case; the
/// JSON / URI parser details are carried verbatim since they can't be
/// meaningfully translated.
#[derive(Clone, Debug, PartialEq)]
pub enum ImportError {
    /// `serde_json` couldn't parse the input as JSON (detail = parser message).
    InvalidJson(String),
    /// The Redis URI was malformed (detail = parser message).
    InvalidUri(String),
    /// URI scheme other than `redis` / `rediss` (carries the offending scheme).
    UnsupportedScheme(String),
    /// Required `name` field missing or empty.
    MissingName,
    /// Required `host` field missing or empty.
    MissingHost,
    /// Port is zero or out of range.
    InvalidPort,
    /// A Redis Insight payload was recognized but held no databases.
    EmptyRedisInsight,
}

impl RedisServer {
    pub fn from_form_data(id: &str, data: &IndexMap<String, String>) -> Self {
        let get_str = |k: &str| data.get(k).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

        let get_parsed = |k: &str| get_str(k).and_then(|s| s.parse().ok());

        let get_bool = |k: &str| get_str(k).map(|s| s == "true" || s == "1");
        let redis_url = parse_url(get_str("host").unwrap_or_default());
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
            sentinel_username: get_str("sentinel_username"),
            sentinel_password: get_str("sentinel_password"),
            description: get_str("description"),
            updated_at: None,

            client_cert: get_str("client_cert"),
            client_key: get_str("client_key"),
            root_cert: get_str("root_cert"),

            ssh_addr: get_str("ssh_addr"),
            ssh_username: get_str("ssh_username"),
            ssh_password: get_str("ssh_password"),
            ssh_key: get_str("ssh_key"),
            ssh_key_passphrase: get_str("ssh_key_passphrase"),

            server_type: get_parsed("server_type").map(|s| s as usize),

            tls,
            insecure: get_bool("insecure"),
            ssh_tunnel: get_bool("ssh_tunnel"),
            readonly: get_bool("readonly"),
            databases: get_str("databases")
                .and_then(|s| s.parse::<usize>().ok())
                .filter(|&n| n > 0),
            // No `> 0` filter here, unlike every other numeric override:
            // DB 0 is a real (and the most common) pin, so clearing the
            // field is the only way to unset it.
            default_db: get_str("default_db").and_then(|s| s.parse::<u16>().ok()),
            connection_timeout: get_str("connection_timeout")
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|&n| n > 0),
            response_timeout: get_str("response_timeout")
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|&n| n > 0),
            key_separator: get_str("key_separator"),
            key_scan_count: get_str("key_scan_count")
                .and_then(|s| s.parse::<usize>().ok())
                .filter(|&n| n > 0),
            max_key_tree_depth: get_str("max_key_tree_depth")
                .and_then(|s| s.parse::<usize>().ok())
                .filter(|&n| n > 0),
            auto_expand_threshold: get_str("auto_expand_threshold")
                .and_then(|s| s.parse::<usize>().ok())
                .filter(|&n| n > 0),
            // Radio index: 0 = inherit global, 1 = show, 2 = hide.
            show_key_tree_ttl: get_str("show_key_tree_ttl").and_then(|s| match s.as_str() {
                "1" | "true" | "show" => Some(true),
                "2" | "false" | "hide" => Some(false),
                _ => None,
            }),
            // The environment select is the single source of truth: derive the
            // display tag from the chosen preset rather than a separate
            // free-text field.
            tag: tag_color_from_form_value(get_str("tag_color").as_deref())
                .as_deref()
                .and_then(env_label_for_key)
                .map(String::from),
            tag_color: tag_color_from_form_value(get_str("tag_color").as_deref()),
            require_confirm_writes: get_bool("require_confirm_writes"),
            group: get_str("group"),
            // sort_order is owned by reorder buttons / drag-drop, not
            // the edit form. Preserve the existing value (the caller
            // fills it in via `upsert_server`).
            sort_order: None,
        }
    }

    /// Tree separator when set and non-empty; otherwise `None` (callers use `":"`).
    pub fn key_separator_override(&self) -> Option<&str> {
        self.key_separator.as_deref().map(str::trim).filter(|s| !s.is_empty())
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

    /// Key-tree separator for this server; defaults to `":"`.
    pub fn resolve_key_separator(&self) -> String {
        self.key_separator_override().unwrap_or(":").to_string()
    }

    /// DB to open this server on: the pinned `default_db` when set,
    /// otherwise `remembered` — the DB the caller last saw it on.
    pub fn resolve_default_db(&self, remembered: usize) -> usize {
        self.default_db.map(usize::from).unwrap_or(remembered)
    }

    /// SCAN page size; `global` is the Settings default when unset.
    pub fn resolve_key_scan_count(&self, global: usize) -> usize {
        self.key_scan_count
            .filter(|&n| n > 0)
            .unwrap_or(global)
            .clamp(10, 100_000)
    }

    /// Max tree depth; `global` is the Settings default when unset.
    pub fn resolve_max_key_tree_depth(&self, global: usize) -> usize {
        self.max_key_tree_depth.filter(|&n| n > 0).unwrap_or(global).max(1)
    }

    /// Auto-expand threshold; `global` is the Settings default when unset.
    pub fn resolve_auto_expand_threshold(&self, global: usize) -> usize {
        self.auto_expand_threshold.unwrap_or(global)
    }

    /// Whether to show TTL chips; `global` is the Settings default when unset.
    pub fn resolve_show_key_tree_ttl(&self, global: bool) -> bool {
        self.show_key_tree_ttl.unwrap_or(global)
    }

    /// Form radio index for `show_key_tree_ttl`: 0 inherit / 1 show / 2 hide.
    pub fn show_key_tree_ttl_form_index(&self) -> usize {
        match self.show_key_tree_ttl {
            None => 0,
            Some(true) => 1,
            Some(false) => 2,
        }
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
        let value = serde_json::to_value(self.export_clone(include_secrets))?;
        serde_json::to_string_pretty(&strip_null_fields(value))
    }

    /// Clean copy of this server for export: the transient identity bits
    /// (`id` / `updated_at` / `sort_order`) are blanked so the importer treats
    /// it as a fresh entry, and credentials are stripped unless
    /// `include_secrets`. The group label is kept — a useful organizational
    /// hint that often carries across teammates.
    fn export_clone(&self, include_secrets: bool) -> RedisServer {
        let mut clone = self.clone();
        clone.id = String::new();
        clone.updated_at = None;
        clone.sort_order = None;
        if !include_secrets {
            clone.password = None;
            clone.sentinel_password = None;
            clone.ssh_password = None;
            clone.ssh_key = None;
            clone.ssh_key_passphrase = None;
            clone.client_cert = None;
            clone.client_key = None;
            clone.root_cert = None;
        }
        clone
    }

    /// Export several servers as one pretty JSON **array**, round-trippable
    /// through [`Self::from_import_multi`]. Each entry is cleaned via
    /// [`Self::export_clone`].
    pub fn to_export_json_many(servers: &[RedisServer], include_secrets: bool) -> serde_json::Result<String> {
        let cleaned: Vec<RedisServer> = servers.iter().map(|s| s.export_clone(include_secrets)).collect();
        let value = serde_json::to_value(&cleaned)?;
        serde_json::to_string_pretty(&strip_null_fields(value))
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
    pub fn from_import_json(json: &str) -> Result<Self, ImportError> {
        let server: RedisServer = serde_json::from_str(json).map_err(|e| ImportError::InvalidJson(e.to_string()))?;
        Self::finalize_imported(server)
    }

    /// Validate a freshly-deserialized imported server and stamp it with a
    /// fresh `id` (so importing twice never clobbers an existing entry),
    /// dropping the sender's `updated_at` / `sort_order`. Shared by the single
    /// JSON path and the multi-server array path.
    /// Crate-visible alias so the other-client importers
    /// (`import_clients.rs`) share the same validation + fresh-id step.
    pub(crate) fn finalize_import(server: RedisServer) -> Result<Self, ImportError> {
        Self::finalize_imported(server)
    }

    fn finalize_imported(mut server: RedisServer) -> Result<Self, ImportError> {
        if server.name.trim().is_empty() {
            return Err(ImportError::MissingName);
        }
        if server.host.trim().is_empty() {
            return Err(ImportError::MissingHost);
        }
        if server.port == 0 {
            return Err(ImportError::InvalidPort);
        }
        server.id = Uuid::now_v7().to_string();
        server.updated_at = None;
        server.sort_order = None;
        Ok(server)
    }

    /// Smart import entry point used by the paste-to-import dialog.
    /// Accepts either a JSON config (as produced by
    /// [`Self::to_export_json`]) or a bare Redis connection URI
    /// (`redis://` / `rediss://`), dispatching on the leading token so
    /// a pasted connection string "just works" alongside the JSON form.
    pub fn from_import(input: &str) -> Result<Self, ImportError> {
        let trimmed = input.trim();
        if trimmed.starts_with("redis://") || trimmed.starts_with("rediss://") {
            Self::from_import_uri(trimmed)
        } else {
            Self::from_import_json(trimmed)
        }
    }

    /// Parse a Redis connection URI into a `RedisServer`. Supports the
    /// standard form `scheme://[username[:password]@]host[:port][/db]`,
    /// where the `rediss` scheme enables TLS. Username and password are
    /// percent-decoded (so e.g. `%40` round-trips to `@`). The friendly
    /// `name` defaults to the host since a bare URI carries no label,
    /// and the trailing `/db` segment becomes the entry's pinned
    /// `default_db` — it is the only place a pasted URI says which
    /// database it means.
    ///
    /// A comma-separated multi-host list (`host1:p1,host2:p2,…` — a
    /// client-specific extension, not standard URI syntax) is reduced to
    /// its first address: Zedis stores a single seed node and discovers
    /// cluster topology at connect time. Query parameters (`?slow=…`) are
    /// likewise client options with no Zedis equivalent and are ignored.
    ///
    /// Returns `Err` for a malformed URI, a non-`redis(s)` scheme, or a
    /// missing host. The entry always gets a fresh `id` so importing
    /// the same URI twice yields two distinct entries.
    pub fn from_import_uri(uri: &str) -> Result<Self, ImportError> {
        let parsed = Url::parse(&keep_first_host(uri.trim())).map_err(|e| ImportError::InvalidUri(e.to_string()))?;
        let scheme = parsed.scheme();
        if scheme != "redis" && scheme != "rediss" {
            return Err(ImportError::UnsupportedScheme(scheme.to_string()));
        }
        let host = parsed
            .host_str()
            .map(|h| strip_ipv6_brackets(h).to_string())
            .filter(|h| !h.is_empty())
            .ok_or(ImportError::MissingHost)?;
        let port = parsed.port().unwrap_or(6379);

        let decode = |s: &str| percent_decode_str(s).decode_utf8_lossy().into_owned();
        let username = {
            let raw = parsed.username();
            (!raw.is_empty()).then(|| decode(raw))
        };
        let password = parsed.password().map(decode).filter(|p| !p.is_empty());

        // A Redis URI's path is its database index (`redis://h:6379/3`) —
        // the one place a pasted URI states which DB it means, so keep it
        // as the pin instead of dropping it. Empty path ⇒ unpinned.
        let default_db = parsed.path().trim_start_matches('/').parse::<u16>().ok();

        Ok(Self {
            id: Uuid::now_v7().to_string(),
            name: host.clone(),
            host,
            port,
            username,
            password,
            default_db,
            tls: (scheme == "rediss").then_some(true),
            ..Default::default()
        })
    }

    /// Import **one or more** servers from pasted text — the entry point used
    /// by the paste-to-import dialog. Recognizes, in order:
    /// 1. a Redis connection URI (`redis://` / `rediss://`) → one server,
    /// 2. a **Redis Insight** database export (a JSON array — or a single
    ///    object — carrying a `connectionType` field) → one server per entry,
    /// 3. an **ARDM** (`connections.ano`) or **Tiny RDM**
    ///    (`connections.yaml`) export → one server per entry,
    /// 4. a Zedis export produced by [`Self::to_export_json`] → one server.
    ///
    /// Every returned server gets a fresh `id`, so importing twice never
    /// clobbers existing entries.
    pub fn from_import_multi(input: &str) -> Result<Vec<RedisServer>, ImportError> {
        let trimmed = input.trim();
        // Redis Insight payloads are detected first (JSON with a distinctive
        // `connectionType` marker); everything else — a Redis URI or a Zedis
        // export — is a single server handled by `from_import`.
        if let Some(servers) = Self::try_redis_insight_import(trimmed)? {
            return Ok(servers);
        }
        // Other GUIs' exports: ARDM's base64 `connections.ano` and Tiny RDM's
        // `connections.yaml`. Both are checked before the Zedis-JSON path —
        // their markers (a `connections` array / a YAML sequence) are
        // distinctive, so a Zedis export can't be mistaken for either.
        if let Some(servers) = crate::import_clients::try_ardm_import(trimmed)? {
            return Ok(servers);
        }
        if let Some(servers) = crate::import_clients::try_tinyrdm_import(trimmed)? {
            return Ok(servers);
        }
        // A Zedis export of multiple servers is a plain JSON array of server
        // objects (Redis Insight arrays were already handled above).
        if trimmed.starts_with('[') {
            let list: Vec<RedisServer> =
                serde_json::from_str(trimmed).map_err(|e| ImportError::InvalidJson(e.to_string()))?;
            return list.into_iter().map(Self::finalize_imported).collect();
        }
        Self::from_import(trimmed).map(|s| vec![s])
    }

    /// Detect and convert a Redis Insight database export. Returns `Ok(None)`
    /// when the input is not a Redis Insight payload (so the caller falls
    /// through to the Zedis-JSON path), `Ok(Some(..))` on success, and `Err`
    /// when it *is* a Redis Insight payload but an entry is unusable (missing
    /// host / bad port).
    ///
    /// The distinguishing marker is the `connectionType` string field, which
    /// Redis Insight writes on every database and Zedis exports never carry.
    /// Both the multi-database array form and a bare single object are taken.
    fn try_redis_insight_import(input: &str) -> Result<Option<Vec<RedisServer>>, ImportError> {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(input) else {
            // Not JSON at all — let the caller surface the Zedis-JSON error.
            return Ok(None);
        };
        let objects: Vec<&serde_json::Value> = match &value {
            serde_json::Value::Array(arr) => arr.iter().collect(),
            serde_json::Value::Object(_) => vec![&value],
            _ => return Ok(None),
        };
        let looks_like_ri = objects
            .first()
            .and_then(|o| o.get("connectionType"))
            .and_then(|v| v.as_str())
            .is_some();
        if !looks_like_ri {
            return Ok(None);
        }
        let mut servers = Vec::with_capacity(objects.len());
        for obj in objects {
            servers.push(Self::from_redis_insight_object(obj)?);
        }
        if servers.is_empty() {
            return Err(ImportError::EmptyRedisInsight);
        }
        Ok(Some(servers))
    }

    /// Convert a single Redis Insight database object into a `RedisServer`,
    /// mapping its field names and nested cert / SSH objects onto the flat
    /// Zedis shape. Best-effort: unknown or empty fields fall back to `None`.
    /// Its selected `db` index becomes the entry's pinned `default_db`.
    /// Redis Insight `tags` / `modules` are intentionally dropped — Zedis
    /// ties its tag to an environment preset, so there is nothing to map onto.
    fn from_redis_insight_object(obj: &serde_json::Value) -> Result<Self, ImportError> {
        let get_str = |k: &str| {
            obj.get(k)
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .filter(|s| !s.is_empty())
        };
        let get_bool = |k: &str| obj.get(k).and_then(serde_json::Value::as_bool);
        // Pull a string out of a nested `{ "<field>": "..." }` blob (cert / ssh).
        let nested_str = |parent: &str, field: &str| {
            obj.get(parent)
                .and_then(|p| p.get(field))
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .filter(|s| !s.is_empty())
        };

        let host = get_str("host").ok_or(ImportError::MissingHost)?;
        let port = obj
            .get("port")
            .and_then(serde_json::Value::as_u64)
            .filter(|&p| p > 0 && p <= u16::MAX as u64)
            .map(|p| p as u16)
            .ok_or(ImportError::InvalidPort)?;
        let name = get_str("name").unwrap_or_else(|| format!("{host}:{port}"));

        // connectionType → server_type. STANDALONE becomes Auto rather than
        // the pinned Standalone, so topology detection still runs on it.
        let server_type = match obj.get("connectionType").and_then(|v| v.as_str()) {
            Some("CLUSTER") => Some(SERVER_TYPE_CLUSTER),
            Some("SENTINEL") => Some(SERVER_TYPE_SENTINEL),
            _ => Some(SERVER_TYPE_AUTO),
        };

        // For a Sentinel entry Redis Insight's top-level credentials are the
        // *sentinel's*; the master's own live under `sentinelMaster`. Zedis
        // keeps the data-node credentials in `username` / `password` and the
        // sentinel's beside them — but only when the export really carries
        // both, so an entry with one set of credentials keeps them for the
        // data nodes as before.
        let sentinel_master_username = nested_str("sentinelMaster", "username");
        let sentinel_master_password = nested_str("sentinelMaster", "password");
        let (username, password, sentinel_username, sentinel_password) = if server_type == Some(SERVER_TYPE_SENTINEL)
            && (sentinel_master_username.is_some() || sentinel_master_password.is_some())
        {
            (
                sentinel_master_username,
                sentinel_master_password,
                get_str("username"),
                get_str("password"),
            )
        } else {
            (get_str("username"), get_str("password"), None, None)
        };

        let tls = get_bool("tls");
        // Redis Insight `verifyServerCert == false` ⇒ skip verification, which
        // is Zedis `insecure == true`. Only meaningful when TLS is enabled.
        let insecure = (tls == Some(true) && get_bool("verifyServerCert") == Some(false)).then_some(true);

        // SSH tunnel: `ssh` toggle + nested `sshOptions { host, port, username,
        // password, privateKey }`. Zedis stores the endpoint as `host:port`.
        let ssh_addr = nested_str("sshOptions", "host").map(|h| {
            match obj
                .get("sshOptions")
                .and_then(|o| o.get("port"))
                .and_then(serde_json::Value::as_u64)
            {
                Some(p) => format!("{h}:{p}"),
                None => format!("{h}:22"),
            }
        });

        Ok(Self {
            id: Uuid::now_v7().to_string(),
            name,
            host,
            port,
            username,
            password,
            server_type,
            sentinel_username,
            sentinel_password,
            default_db: obj
                .get("db")
                .and_then(serde_json::Value::as_u64)
                .and_then(|db| u16::try_from(db).ok()),
            // Sentinel master name, when Redis Insight recorded one.
            master_name: nested_str("sentinelMaster", "name"),
            tls,
            insecure,
            // caCert / clientCert are nested objects holding PEM strings.
            root_cert: nested_str("caCert", "certificate"),
            client_cert: nested_str("clientCert", "certificate"),
            client_key: nested_str("clientCert", "key"),
            ssh_tunnel: get_bool("ssh"),
            ssh_addr,
            ssh_username: nested_str("sshOptions", "username"),
            ssh_password: nested_str("sshOptions", "password"),
            ssh_key: nested_str("sshOptions", "privateKey"),
            ssh_key_passphrase: nested_str("sshOptions", "passphrase"),
            ..Default::default()
        })
    }

    pub fn tag_label(&self) -> Option<&str> {
        self.tag.as_deref().map(|s| s.trim()).filter(|s| !s.is_empty())
    }
    /// Returns true when the tag implies production-grade caution: typed-name confirm,
    /// no "remember choice" shortcut. Driven by tag_color preset key, not by tag text.
    /// `magenta` is the current production color; `red` is honored as a legacy alias
    /// so servers tagged before the palette change keep their safety escalation.
    pub fn is_high_risk_tag(&self) -> bool {
        matches!(self.tag_color.as_deref(), Some("magenta") | Some("red"))
    }
    /// Whether the Sentinel nodes carry credentials of their own.
    pub fn has_sentinel_credentials(&self) -> bool {
        let set = |v: &Option<String>| v.as_deref().is_some_and(|s| !s.trim().is_empty());
        set(&self.sentinel_username) || set(&self.sentinel_password)
    }

    /// This entry with the Sentinel credentials swapped in — what the
    /// sentinel nodes are dialed with. The data nodes keep `username` /
    /// `password`.
    pub fn sentinel_login(&self) -> RedisServer {
        let mut login = self.clone();
        login.username = self.sentinel_username.clone();
        login.password = self.sentinel_password.clone();
        login
    }

    /// Generates the connection URL based on host, port, and optional password.
    /// An IPv6 literal host is bracketed, as a URL requires (`redis://[::1]:6379`).
    pub fn get_connection_url(&self) -> String {
        let tls = self.tls.unwrap_or(false);
        let scheme = if tls { "rediss" } else { "redis" };

        let safe_pwd = self.password.as_deref().filter(|s| !s.trim().is_empty());
        let safe_usr = self.username.as_deref().filter(|s| !s.trim().is_empty());
        let host = bracket_ipv6(&self.host);

        let url = match (safe_pwd, safe_usr) {
            (Some(pwd), Some(username)) => {
                let pwd_enc = utf8_percent_encode(pwd, NON_ALPHANUMERIC).to_string();
                let username_enc = utf8_percent_encode(username, NON_ALPHANUMERIC).to_string();
                format!("{scheme}://{username_enc}:{pwd_enc}@{host}:{}", self.port)
            }
            (Some(pwd), None) => {
                let pwd_enc = utf8_percent_encode(pwd, NON_ALPHANUMERIC).to_string();
                format!("{scheme}://:{pwd_enc}@{host}:{}", self.port)
            }
            _ => format!("{scheme}://{host}:{}", self.port),
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
    // A damaged file is quarantined and the `.bak` restored (or the list left
    // empty) — never parsed-as-empty, which the next save would then write
    // over the user's connections. The UI reports the recovery at startup.
    let loaded = load_config_with_recovery(&path, |text| {
        toml::from_str::<RedisServers>(text).map_err(|e| e.to_string())
    })?;
    match &loaded.recovery {
        Some(ConfigRecovery::RestoredFromBackup { corrupt_path, .. }) => {
            warn!(corrupt = %corrupt_path.display(), "redis-servers.toml was unreadable; restored from backup")
        }
        Some(ConfigRecovery::Reset { corrupt_path, .. }) => {
            error!(corrupt = %corrupt_path.display(), "redis-servers.toml was unreadable and no backup parsed; starting with no servers")
        }
        None => {}
    }
    let Some(configs) = loaded.value else {
        return Ok(vec![]);
    };
    let mut servers = configs.servers;
    let mut configs = HashMap::new();
    for server in servers.iter_mut() {
        if let Some(password) = &server.password {
            server.password = Some(decrypt(password).unwrap_or(password.clone()));
        }
        if let Some(password) = &server.sentinel_password {
            server.sentinel_password = Some(decrypt(password).unwrap_or(password.clone()));
        }
        if let Some(ssh_password) = &server.ssh_password {
            server.ssh_password = Some(decrypt(ssh_password).unwrap_or(ssh_password.clone()));
        }
        if let Some(ssh_key) = &server.ssh_key {
            server.ssh_key = Some(decrypt(ssh_key).unwrap_or(ssh_key.clone()));
        }
        if let Some(passphrase) = &server.ssh_key_passphrase {
            server.ssh_key_passphrase = Some(decrypt(passphrase).unwrap_or(passphrase.clone()));
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
        if let Some(password) = &server.sentinel_password {
            server.sentinel_password = Some(encrypt(password)?);
        }
        if let Some(ssh_password) = &server.ssh_password {
            server.ssh_password = Some(encrypt(ssh_password)?);
        }
        if let Some(ssh_key) = &server.ssh_key {
            server.ssh_key = Some(encrypt(ssh_key)?);
        }
        if let Some(passphrase) = &server.ssh_key_passphrase {
            server.ssh_key_passphrase = Some(encrypt(passphrase)?);
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
    // Atomic replace plus a rolling `.bak` — the server list carries the
    // encrypted secrets, so a half-written file here is unrecoverable.
    unblock(move || write_file_atomic_with_backup(&path, value.as_bytes())).await?;
    Ok(())
}

/// The saved server list with every secret (passwords, SSH key and
/// passphrase, client key) replaced by `<redacted>` — what the diagnostics
/// bundle ships instead of `redis-servers.toml` itself.
pub fn servers_toml_redacted() -> Result<String> {
    redact_servers(get_servers()?)
}

fn redact_servers(mut servers: Vec<RedisServer>) -> Result<String> {
    for server in servers.iter_mut() {
        for secret in [
            &mut server.password,
            &mut server.sentinel_password,
            &mut server.ssh_password,
            &mut server.ssh_key,
            &mut server.ssh_key_passphrase,
            &mut server.client_key,
        ] {
            if secret.is_some() {
                *secret = Some("<redacted>".to_string());
            }
        }
    }
    toml::to_string(&RedisServers { servers }).map_err(|e| Error::Invalid { message: e.to_string() })
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

/// Real `redis-servers.toml` files from earlier releases: every entry must
/// keep parsing (an upgrade that loses connections is the worst regression
/// this file can have) and every field that still exists must survive.
#[cfg(test)]
mod upgrade_fixtures {
    use super::*;

    const V0_4_0: &str = include_str!("fixtures/redis-servers-v0.4.0.toml");
    const V0_7_0: &str = include_str!("fixtures/redis-servers-v0.7.0.toml");

    fn load(fixture: &str) -> Vec<RedisServer> {
        toml::from_str::<RedisServers>(fixture)
            .expect("an old redis-servers.toml must keep parsing")
            .servers
    }

    #[test]
    fn a_minimal_entry_parses() {
        // The contract behind `#[serde(default)]`: only id / name / host /
        // port have ever been written unconditionally.
        let servers = load("[[servers]]\nid = \"a\"\nname = \"a\"\nhost = \"h\"\nport = 1\n");
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].port, 1);
        assert!(servers[0].password.is_none());
    }

    #[test]
    fn v0_4_0_entries_survive() {
        let servers = load(V0_4_0);
        assert_eq!(servers.len(), 2);
        let prod = &servers[0];
        assert_eq!(prod.name, "prod-cache");
        assert_eq!(prod.username.as_deref(), Some("admin"));
        assert_eq!(prod.password.as_deref(), Some("supersecret"));
        assert_eq!(prod.tls, Some(true));
        assert!(
            prod.root_cert
                .as_deref()
                .is_some_and(|c| c.starts_with("-----BEGIN CERTIFICATE-----"))
        );
        assert_eq!(prod.ssh_tunnel, Some(true));
        assert_eq!(prod.ssh_addr.as_deref(), Some("bastion.example.com:22"));
        assert_eq!(prod.readonly, Some(true));
        assert_eq!(prod.tag.as_deref(), Some("PROD"));
        assert_eq!(prod.group.as_deref(), Some("Team A"));
        assert_eq!(prod.sort_order, Some(1));
        // Fields added after 0.4 default cleanly.
        assert!(prod.ssh_key_passphrase.is_none());
        assert!(prod.default_db.is_none());
        assert!(prod.databases.is_none());
        assert_eq!(servers[1].id, "srv-2");
        assert!(servers[1].tls.is_none());
    }

    #[test]
    fn v0_7_0_entries_survive() {
        let servers = load(V0_7_0);
        assert_eq!(servers.len(), 1);
        let s = &servers[0];
        assert_eq!(s.server_type, Some(SERVER_TYPE_SENTINEL));
        assert_eq!(s.master_name.as_deref(), Some("mymaster"));
        assert_eq!(s.databases, Some(16));
        assert_eq!(s.connection_timeout, Some(5));
        assert_eq!(s.key_separator.as_deref(), Some("/"));
        assert_eq!(s.key_scan_count, Some(2000));
        assert_eq!(s.ssh_key_passphrase.as_deref(), Some("pass"));
        assert!(s.default_db.is_none());
    }

    #[test]
    fn old_files_round_trip_through_the_current_writer() {
        for fixture in [V0_4_0, V0_7_0] {
            let servers = load(fixture);
            let written = toml::to_string(&RedisServers {
                servers: servers.clone(),
            })
            .expect("serialize");
            let again = load(&written);
            assert_eq!(again, servers);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv6_hosts_are_bracketed_in_urls_and_stripped_on_import() {
        let mut s = RedisServer {
            host: "::1".into(),
            port: 6379,
            ..Default::default()
        };
        assert_eq!(s.get_connection_url(), "redis://[::1]:6379");
        s.password = Some("pw".into());
        assert_eq!(s.get_connection_url(), "redis://:pw@[::1]:6379");
        s.username = Some("app".into());
        assert_eq!(s.get_connection_url(), "redis://app:pw@[::1]:6379");
        s.host = "[::1]".into();
        assert_eq!(
            s.get_connection_url(),
            "redis://app:pw@[::1]:6379",
            "an already bracketed host is not wrapped twice"
        );
        // Names and IPv4 are untouched.
        s.host = "redis.example.com".into();
        assert_eq!(s.get_connection_url(), "redis://app:pw@redis.example.com:6379");

        // A pasted URI keeps the bare literal; the brackets come back only
        // where a URL is built.
        let imported = RedisServer::from_import_uri("redis://[2001:db8::1]:6380/2").expect("uri");
        assert_eq!(imported.host, "2001:db8::1");
        assert_eq!(imported.port, 6380);
        assert_eq!(imported.default_db, Some(2));
        assert_eq!(imported.get_connection_url(), "redis://[2001:db8::1]:6380");
        // The form's host field takes a URL too.
        let form = parse_url("redis://[::1]:6390".to_string());
        assert_eq!(form.host, "::1");
        assert_eq!(form.port, Some(6390));
    }

    #[test]
    fn redis_insight_sentinel_entries_keep_both_logins() {
        // Redis Insight: top-level credentials are the sentinel's, the
        // master's own sit under `sentinelMaster`.
        let ri = r#"[{
            "host": "sentinel.example.com", "port": 26379, "name": "s", "connectionType": "SENTINEL",
            "username": "sentinel-user", "password": "sentinel-pw",
            "sentinelMaster": { "name": "mymaster", "username": "app", "password": "master-pw" }
        }]"#;
        let servers = RedisServer::from_import_multi(ri).expect("ri import");
        let s = &servers[0];
        assert_eq!(s.server_type, Some(SERVER_TYPE_SENTINEL));
        assert_eq!(s.master_name.as_deref(), Some("mymaster"));
        assert_eq!(s.username.as_deref(), Some("app"));
        assert_eq!(s.password.as_deref(), Some("master-pw"));
        assert_eq!(s.sentinel_username.as_deref(), Some("sentinel-user"));
        assert_eq!(s.sentinel_password.as_deref(), Some("sentinel-pw"));
        assert!(s.has_sentinel_credentials());
        let login = s.sentinel_login();
        assert_eq!(login.username.as_deref(), Some("sentinel-user"));
        assert_eq!(login.password.as_deref(), Some("sentinel-pw"));
        assert_eq!(login.host, s.host, "only the credentials change");

        // One set of credentials stays with the data nodes, as before.
        let ri = r#"[{
            "host": "h", "port": 26379, "name": "s", "connectionType": "SENTINEL", "password": "pw",
            "sentinelMaster": { "name": "mymaster" }
        }]"#;
        let servers = RedisServer::from_import_multi(ri).expect("ri import");
        let s = &servers[0];
        assert_eq!(s.password.as_deref(), Some("pw"));
        assert!(s.sentinel_username.is_none() && s.sentinel_password.is_none());
        assert!(!s.has_sentinel_credentials());
    }

    #[test]
    fn export_strips_the_sentinel_password_with_the_others() {
        let s = RedisServer {
            host: "h".into(),
            port: 1,
            password: Some("data-secret".into()),
            sentinel_password: Some("sentinel-secret".into()),
            ..Default::default()
        };
        let json = s.to_export_json(false).expect("json");
        assert!(!json.contains("data-secret") && !json.contains("sentinel-secret"));
        let json = s.to_export_json(true).expect("json");
        assert!(json.contains("sentinel-secret"));
        let redacted = redact_servers(vec![s]).expect("toml");
        assert!(redacted.contains("sentinel_password = \"<redacted>\""));
        assert!(!redacted.contains("sentinel-secret"));
    }

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
            tag_color: Some("magenta".into()),
            ..Default::default()
        }
    }

    #[test]
    fn redacted_toml_keeps_identity_and_hides_every_secret() {
        let mut server = sample_server();
        server.ssh_password = Some("ssh-pw-9f1".into());
        server.ssh_key_passphrase = Some("unlock-7c2".into());
        server.client_key = Some("-----BEGIN PRIVATE KEY-----".into());
        let redacted = redact_servers(vec![server]).expect("redact");
        assert!(redacted.contains("prod-cache") && redacted.contains("10.0.0.5") && redacted.contains("bastion:22"));
        // Values, not field names: `ssh_key_passphrase` itself must stay.
        for secret in ["supersecret", "ssh-pw-9f1", "unlock-7c2", "PRIVATE KEY"] {
            assert!(!redacted.contains(secret), "{secret} leaked");
        }
        assert_eq!(redacted.matches("<redacted>").count(), 5);
        // The certificate (public) is not a secret and stays readable.
        assert!(redacted.contains("ssh_key_passphrase = \"<redacted>\""));
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
    fn import_uri_parses_rediss_with_credentials() {
        let s = RedisServer::from_import_uri("rediss://default:s3cr3t@unified-moccasin-144033.upstash.io:6379")
            .expect("import uri");
        assert_eq!(s.host, "unified-moccasin-144033.upstash.io");
        assert_eq!(s.port, 6379);
        assert_eq!(s.username.as_deref(), Some("default"));
        assert_eq!(s.password.as_deref(), Some("s3cr3t"));
        assert_eq!(s.tls, Some(true));
        // Name defaults to the host; id is freshly allocated.
        assert_eq!(s.name, "unified-moccasin-144033.upstash.io");
        assert!(!s.id.is_empty());
    }

    #[test]
    fn import_uri_plain_redis_defaults_port_and_no_tls() {
        let s = RedisServer::from_import_uri("redis://cache.internal").expect("import uri");
        assert_eq!(s.host, "cache.internal");
        assert_eq!(s.port, 6379);
        assert!(s.username.is_none());
        assert!(s.password.is_none());
        assert!(s.tls.is_none());
    }

    #[test]
    fn import_uri_percent_decodes_and_allows_password_only() {
        // Password-only (empty username) and a percent-encoded `@`.
        let s = RedisServer::from_import_uri("redis://:p%40ss@10.0.0.5:6380").expect("import uri");
        assert!(s.username.is_none());
        assert_eq!(s.password.as_deref(), Some("p@ss"));
        assert_eq!(s.port, 6380);
    }

    #[test]
    fn import_uri_multi_host_takes_first_seed_and_ignores_query() {
        // The comma-separated multi-host + client-options form some clients
        // emit for clusters. Only the first address is kept — topology is
        // discovered at connect — and the query options are dropped.
        let s = RedisServer::from_import_uri(
            "redis://:pass@192.168.1.10:31545,192.168.1.11:31167,192.168.1.12:30105/?slow=200ms&maxProcessing=1000",
        )
        .expect("import uri");
        assert_eq!(s.host, "192.168.1.10");
        assert_eq!(s.port, 31545);
        assert!(s.username.is_none());
        assert_eq!(s.password.as_deref(), Some("pass"));
        assert!(s.tls.is_none());
    }

    #[test]
    fn import_uri_comma_in_password_is_not_a_host_list() {
        // A raw comma is legal inside userinfo — only commas after the
        // last `@` (the host part) trigger the multi-host reduction.
        let s = RedisServer::from_import_uri("redis://user:pa,ss@10.0.0.5:6380").expect("import uri");
        assert_eq!(s.host, "10.0.0.5");
        assert_eq!(s.port, 6380);
        assert_eq!(s.password.as_deref(), Some("pa,ss"));
    }

    #[test]
    fn import_uri_pins_the_path_database() {
        let pinned = RedisServer::from_import_uri("redis://h:6379/3").expect("uri");
        assert_eq!(pinned.default_db, Some(3));
        // DB 0 is a pin, not an absent one.
        let zero = RedisServer::from_import_uri("redis://h:6379/0").expect("uri");
        assert_eq!(zero.default_db, Some(0));
        // No path (with or without the trailing slash) leaves it unpinned.
        assert_eq!(
            RedisServer::from_import_uri("redis://h:6379").expect("uri").default_db,
            None
        );
        assert_eq!(
            RedisServer::from_import_uri("redis://h:6379/").expect("uri").default_db,
            None
        );
        // A query string is a client option, not part of the index.
        assert_eq!(
            RedisServer::from_import_uri("redis://h:6379/7?slow=1")
                .expect("uri")
                .default_db,
            Some(7)
        );
    }

    #[test]
    fn import_uri_rejects_bad_scheme_and_missing_host() {
        assert!(RedisServer::from_import_uri("http://example.com:6379").is_err());
        assert!(RedisServer::from_import_uri("not a uri").is_err());
    }

    #[test]
    fn import_dispatches_uri_vs_json() {
        // Leading redis:// routes to the URI parser.
        let from_uri = RedisServer::from_import("  rediss://h:6379  ").expect("uri branch");
        assert_eq!(from_uri.host, "h");
        assert_eq!(from_uri.tls, Some(true));
        // Anything else routes to the JSON parser.
        let json = sample_server().to_export_json(false).expect("serialize");
        let from_json = RedisServer::from_import(&json).expect("json branch");
        assert_eq!(from_json.name, "prod-cache");
    }

    #[test]
    fn import_redis_insight_standalone_sample() {
        // The exact shape Redis Insight writes for "export databases".
        let ri = r#"[
          {
            "id": "e8ee0846-71df-4f60-95dc-9fb19285d7ab",
            "host": "127.0.0.1",
            "port": 6379,
            "name": "127.0.0.1:6379",
            "db": null,
            "username": null,
            "password": null,
            "connectionType": "STANDALONE",
            "tls": null,
            "verifyServerCert": null,
            "caCert": null,
            "clientCert": null,
            "ssh": null,
            "sshOptions": null
          }
        ]"#;
        let servers = RedisServer::from_import_multi(ri).expect("ri import");
        assert_eq!(servers.len(), 1);
        let s = &servers[0];
        assert_eq!(s.host, "127.0.0.1");
        assert_eq!(s.port, 6379);
        assert_eq!(s.name, "127.0.0.1:6379");
        assert_eq!(s.server_type, Some(SERVER_TYPE_AUTO)); // STANDALONE → detect
        assert!(s.username.is_none() && s.password.is_none());
        assert!(s.tls.is_none() && s.insecure.is_none());
        assert!(s.ssh_tunnel.is_none() && s.ssh_addr.is_none());
        assert_eq!(s.default_db, None); // "db": null ⇒ unpinned
        // A fresh id is allocated, not the Redis Insight UUID.
        assert!(!s.id.is_empty());
        assert_ne!(s.id, "e8ee0846-71df-4f60-95dc-9fb19285d7ab");
    }

    #[test]
    fn import_redis_insight_pins_selected_db() {
        let ri = r#"[
          { "host": "h", "port": 6379, "name": "n", "db": 5, "connectionType": "STANDALONE" }
        ]"#;
        let servers = RedisServer::from_import_multi(ri).expect("ri import");
        assert_eq!(servers[0].default_db, Some(5));
    }

    #[test]
    fn import_redis_insight_maps_tls_ssh_and_cluster() {
        let ri = r#"[
          {
            "host": "redis.example.com",
            "port": 6380,
            "name": "prod",
            "username": "app",
            "password": "pw",
            "connectionType": "CLUSTER",
            "tls": true,
            "verifyServerCert": false,
            "caCert": { "name": "ca", "certificate": "-----BEGIN CERTIFICATE-----CA-----END CERTIFICATE-----" },
            "clientCert": { "name": "cli", "certificate": "-----BEGIN CERTIFICATE-----CLI-----END CERTIFICATE-----", "key": "-----BEGIN PRIVATE KEY-----K-----END PRIVATE KEY-----" },
            "ssh": true,
            "sshOptions": { "host": "bastion.example.com", "port": 2222, "username": "ops", "password": "sshpw", "privateKey": "-----BEGIN OPENSSH PRIVATE KEY-----PK", "passphrase": "keypw" }
          }
        ]"#;
        let servers = RedisServer::from_import_multi(ri).expect("ri import");
        let s = &servers[0];
        // Regression: this was `Some(1)` — the *Standalone* pin, which also
        // switches off topology detection — so every imported Redis Insight
        // cluster opened as a single node.
        assert_eq!(s.server_type, Some(SERVER_TYPE_CLUSTER));
        assert_ne!(s.server_type, Some(SERVER_TYPE_STANDALONE));
        assert_eq!(s.username.as_deref(), Some("app"));
        assert_eq!(s.password.as_deref(), Some("pw"));
        assert_eq!(s.tls, Some(true));
        assert_eq!(s.insecure, Some(true)); // verifyServerCert=false → skip verify
        assert!(s.root_cert.as_deref().expect("ca").contains("CA"));
        assert!(s.client_cert.as_deref().expect("client cert").contains("CLI"));
        assert!(s.client_key.as_deref().expect("client key").contains("PRIVATE KEY"));
        assert_eq!(s.ssh_tunnel, Some(true));
        assert_eq!(s.ssh_addr.as_deref(), Some("bastion.example.com:2222"));
        assert_eq!(s.ssh_username.as_deref(), Some("ops"));
        assert_eq!(s.ssh_password.as_deref(), Some("sshpw"));
        assert!(s.ssh_key.as_deref().expect("ssh key").contains("OPENSSH"));
        assert_eq!(s.ssh_key_passphrase.as_deref(), Some("keypw"));
    }

    #[test]
    fn import_redis_insight_multiple_entries_get_distinct_ids() {
        let ri = r#"[
          { "host": "a", "port": 6379, "name": "A", "connectionType": "STANDALONE" },
          { "host": "b", "port": 6380, "name": "B", "connectionType": "SENTINEL" }
        ]"#;
        let servers = RedisServer::from_import_multi(ri).expect("ri import");
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].host, "a");
        assert_eq!(servers[1].server_type, Some(SERVER_TYPE_SENTINEL));
        assert_ne!(servers[0].id, servers[1].id);
    }

    #[test]
    fn import_multi_still_handles_zedis_json_and_uri() {
        // Non-Redis-Insight JSON (no connectionType) routes to the Zedis path.
        let json = sample_server().to_export_json(true).expect("serialize");
        let from_json = RedisServer::from_import_multi(&json).expect("zedis json");
        assert_eq!(from_json.len(), 1);
        assert_eq!(from_json[0].name, "prod-cache");
        // A bare URI still yields a single server.
        let from_uri = RedisServer::from_import_multi("redis://h:6379").expect("uri");
        assert_eq!(from_uri.len(), 1);
        assert_eq!(from_uri[0].host, "h");
    }

    #[test]
    fn export_many_round_trips_through_import() {
        let a = sample_server();
        let mut b = sample_server();
        b.name = "second".into();
        b.host = "10.0.0.6".into();
        let json = RedisServer::to_export_json_many(&[a, b], true).expect("export many");
        assert!(json.trim_start().starts_with('['), "expected a JSON array");
        let imported = RedisServer::from_import_multi(&json).expect("import array");
        assert_eq!(imported.len(), 2);
        assert_eq!(imported[0].name, "prod-cache");
        assert_eq!(imported[1].name, "second");
        // Fresh, distinct ids — never the source id, never empty.
        assert_ne!(imported[0].id, imported[1].id);
        assert!(imported.iter().all(|s| !s.id.is_empty() && s.id != "src-id"));
    }

    #[test]
    fn export_strips_install_local_sort_order_but_keeps_group() {
        let mut s = sample_server();
        s.group = Some("Team A".into());
        s.sort_order = Some(42);
        let json = s.to_export_json(false).expect("serialize");
        assert!(json.contains("\"Team A\""));
        // Null (None) fields are stripped from the export entirely — including
        // the blanked sort_order, which the receiver's upsert reassigns.
        assert!(!json.contains("sort_order"));
        assert!(!json.contains(": null"), "exported JSON should contain no null fields");
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

    #[test]
    fn key_separator_defaults_to_colon() {
        let mut s = sample_server();
        assert_eq!(s.resolve_key_separator(), ":");
        s.key_separator = Some("  ".into());
        assert_eq!(s.resolve_key_separator(), ":");
        s.key_separator = Some("_".into());
        assert_eq!(s.resolve_key_separator(), "_");
        assert_eq!(s.key_separator_override(), Some("_"));
    }

    #[test]
    fn key_behavior_overrides_fall_back_to_global() {
        let mut s = sample_server();
        assert_eq!(s.resolve_key_scan_count(10_000), 10_000);
        assert_eq!(s.resolve_max_key_tree_depth(5), 5);
        assert_eq!(s.resolve_auto_expand_threshold(100), 100);
        assert!(s.resolve_show_key_tree_ttl(true));
        assert!(!s.resolve_show_key_tree_ttl(false));
        assert_eq!(s.show_key_tree_ttl_form_index(), 0);

        s.key_scan_count = Some(500);
        s.max_key_tree_depth = Some(3);
        s.auto_expand_threshold = Some(50);
        s.show_key_tree_ttl = Some(false);
        assert_eq!(s.resolve_key_scan_count(10_000), 500);
        assert_eq!(s.resolve_max_key_tree_depth(5), 3);
        assert_eq!(s.resolve_auto_expand_threshold(100), 50);
        assert!(!s.resolve_show_key_tree_ttl(true));
        assert_eq!(s.show_key_tree_ttl_form_index(), 2);
    }

    #[test]
    fn default_db_pin_beats_the_remembered_db() {
        let mut s = sample_server();
        assert_eq!(s.resolve_default_db(7), 7);
        s.default_db = Some(3);
        assert_eq!(s.resolve_default_db(7), 3);
        // DB 0 is a pin like any other, not "unset".
        s.default_db = Some(0);
        assert_eq!(s.resolve_default_db(7), 0);
    }

    #[test]
    fn form_data_parses_default_db() {
        let form = |value: &str| {
            let mut data = IndexMap::new();
            data.insert("name".to_string(), "srv".to_string());
            data.insert("host".to_string(), "127.0.0.1".to_string());
            data.insert("default_db".to_string(), value.to_string());
            RedisServer::from_form_data("id", &data).default_db
        };
        assert_eq!(form("4"), Some(4));
        // The `> 0` filter the other numeric overrides use would wrongly
        // drop this one — DB 0 is the most common pin of all.
        assert_eq!(form("0"), Some(0));
        assert_eq!(form("65535"), Some(65535));
        // Clearing the field unpins.
        assert_eq!(form(""), None);
        assert_eq!(form("   "), None);
        // Out of u16 range / not a number: rejected by the form's validator
        // before it gets here, and ignored if it ever does.
        assert_eq!(form("65536"), None);
        assert_eq!(form("-1"), None);
        assert_eq!(form("abc"), None);
    }
}
