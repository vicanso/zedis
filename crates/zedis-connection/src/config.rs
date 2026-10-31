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
#[cfg(not(target_family = "wasm"))]
use crate::string::{decrypt, encrypt};
use arc_swap::ArcSwap;
use indexmap::IndexMap;
use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
#[cfg(not(target_family = "wasm"))]
use redis::{ClientTlsConfig, TlsCertificates};
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(not(target_family = "wasm"))]
use smol::unblock;
use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
#[cfg(not(target_family = "wasm"))]
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(not(target_family = "wasm"))]
use tracing::{debug, error, info, warn};
use url::Url;
use uuid::Uuid;
#[cfg(not(target_family = "wasm"))]
use zedis_core::env::is_development;
// The saved server list lives in a file. In the browser there is none: the
// bridge holds the list and the page asks it over HTTP (ADR 9).
#[cfg(not(target_family = "wasm"))]
use zedis_core::fs::{
    ConfigRecovery, get_or_create_config_dir, load_config_with_recovery, resolve_path, write_file_atomic_with_backup,
};
use zedis_core::string::{bracket_ipv6, split_host_port_or, strip_ipv6_brackets};

type Result<T, E = Error> = std::result::Result<T, E>;

// Reading TLS material — PEM from a field or a file, an encrypted client
// key — is the desktop's and the bridge's business: the browser build never
// dials TLS itself. One gated module rather than a gate per item.
#[cfg(not(target_family = "wasm"))]
mod tls;

mod form;
mod store;
mod transfer;

pub use form::{TAG_ENV_LABELS, tag_color_index, writes_index};
pub use store::{get_server, get_server_groups, get_servers, save_servers, set_servers_cache};
#[cfg(not(target_family = "wasm"))]
pub use store::{open_server_secrets, read_servers_file, seal_server_secrets, servers_toml_redacted};
pub use transfer::ImportError;

/// Keys asked for per `SCAN` when neither the entry nor Settings names a
/// number.
pub const DEFAULT_KEY_SCAN_COUNT: usize = 10_000;
/// Floor for a typed SCAN count. Below this a page is too small to be worth
/// the round trip; Settings and `resolve_key_scan_count` clamp to it.
pub const MIN_KEY_SCAN_COUNT: usize = 1_000;
/// Cap for a typed SCAN count so a typo cannot ask the server for the whole
/// keyspace in one `COUNT`.
pub const MAX_KEY_SCAN_COUNT: usize = 100_000;
/// The same default on an entry tagged as production. `SCAN` holds the
/// server's one command thread for the whole count: 10,000 measured 43–50 ms
/// a call on a 100,000-key database — a pause for every other client, and a
/// line in the slow log, each time the tree loads a page. A tenth of it keeps
/// a page a few milliseconds; the tree makes up the difference in rounds.
pub const PRODUCTION_KEY_SCAN_COUNT: usize = MIN_KEY_SCAN_COUNT;

/// The connection-string schemes Zedis reads, and whether each means TLS.
/// `valkey://` / `valkeys://` are Valkey's spelling of the same format — what
/// `valkey-cli -u` takes and what Aiven's console hands out — so a pasted
/// Valkey URI reads exactly like its `redis(s)://` twin. What Zedis *writes*
/// (`get_connection_url`) stays `redis(s)://`: that string only goes to
/// redis-rs, never to a person.
const URI_SCHEMES: [(&str, bool); 4] = [("redis", false), ("rediss", true), ("valkey", false), ("valkeys", true)];

/// `Some(tls)` for a connection-string scheme, `None` for any other.
/// Case-insensitive, as URI schemes are.
fn scheme_tls(scheme: &str) -> Option<bool> {
    URI_SCHEMES
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(scheme))
        .map(|(_, tls)| *tls)
}

/// `true` when `input` is a connection string — `redis://`, `rediss://`,
/// `valkey://` or `valkeys://` — which is how the import dialog and the
/// command line tell a link from everything else they are handed.
pub fn is_connection_uri(input: &str) -> bool {
    input
        .trim()
        .split_once("://")
        .is_some_and(|(scheme, _)| scheme_tls(scheme).is_some())
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
    // A Unix socket path is the host as it is (`is_unix_socket`); parsed as
    // `redis:///var/run/redis.sock` it had no host and the path was lost.
    if host.starts_with('/') {
        return RedisUrl {
            host,
            ..Default::default()
        };
    }
    let input_to_parse = if host.contains("://") {
        host.to_string()
    } else {
        format!("redis://{host}")
    };
    if let Ok(u) = Url::parse(input_to_parse.as_str()) {
        // Userinfo arrives percent-encoded (`p%40ss` for `p@ss`); stored as
        // it came, the password was encoded a second time when the URL was
        // built again and the server saw `p%40ss`. The import path already
        // decodes the same way.
        let decode = |s: &str| percent_decode_str(s).decode_utf8_lossy().into_owned();
        let username = decode(u.username());
        let password = u.password().map(decode);
        // `redis+unix:///path` / `unix:///path`: the socket is the path.
        if u.scheme().eq_ignore_ascii_case("unix") || u.scheme().eq_ignore_ascii_case("redis+unix") {
            return RedisUrl {
                host: u.path().to_string(),
                username,
                password,
                ..Default::default()
            };
        }
        // `url` keeps the brackets on an IPv6 host; the stored host is the
        // bare literal, bracketed again only where a URL is built.
        let host = strip_ipv6_brackets(u.host_str().unwrap_or(""));
        let port = u.port();
        RedisUrl {
            host: host.to_string(),
            port,
            username,
            password,
            tls: scheme_tls(u.scheme()).unwrap_or(false),
        }
    } else {
        RedisUrl {
            host: host.to_string(),
            ..Default::default()
        }
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
    /// Whose entry this is on an HTTP bridge with accounts: a name, or `None`
    /// for an entry every account sees. Only the bridge gives it meaning —
    /// it lists an entry to its owner and to nobody else — and only the
    /// bridge assigns it, from who is signed in, never from what a request
    /// claims. The desktop app has no accounts: it never sets this and must
    /// never drop it, or a private entry in a bridge's file would become
    /// everyone's the first time the file was saved from a desktop (ADR 9).
    pub owner: Option<String>,
    pub tls: Option<bool>,
    pub insecure: Option<bool>,
    /// Name the server's certificate is verified against and sent as SNI,
    /// when it differs from `host` — an endpoint reached by IP or through
    /// an SSH tunnel presents a certificate for its DNS name, not for the
    /// address dialed. Standalone and tunnelled connections only: cluster
    /// nodes are dialed by the addresses `CLUSTER NODES` reports.
    pub tls_server_name: Option<String>,
    /// PEM content *or* a file path — every certificate field takes both.
    pub client_cert: Option<String>,
    pub client_key: Option<String>,
    /// Passphrase of a PKCS#8-encrypted `client_key` (`BEGIN ENCRYPTED
    /// PRIVATE KEY`). Encrypted at rest like the other secrets.
    pub client_key_passphrase: Option<String>,
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
    /// Jump host between this machine and `ssh_addr`, as `[user@]host[:port]`
    /// (OpenSSH `ProxyJump`, one hop). Authenticates with the same key /
    /// password / agent as the tunnel host; user defaults to `ssh_username`.
    pub ssh_jump: Option<String>,
    /// Cluster only: let reads (not scans, which fan out to masters) be
    /// served by replicas, round-robin. Off keeps every command on masters.
    pub cluster_read_replicas: Option<bool>,
    pub tag: Option<String>,
    pub tag_color: Option<String>,
    pub require_confirm_writes: Option<bool>,
    /// Whether threshold alerts are enabled for this server. Off by default
    /// (`None` / `Some(false)`): no rule is evaluated unless the user opts in
    /// on this connection.
    pub alerts_enabled: Option<bool>,
    /// Whether writes start locked, to be unlocked for a while at a time
    /// (`WRITE_UNLOCK_SECS`). `None` follows the tag: a Prod entry is
    /// locked, any other is not — see [`Self::write_locked`].
    pub write_lock: Option<bool>,
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
    /// Tree separator when set and non-empty; otherwise `None` (callers use `":"`).
    pub fn key_separator_override(&self) -> Option<&str> {
        self.key_separator.as_deref().map(str::trim).filter(|s| !s.is_empty())
    }
    /// What a browser writes in `owner` to say "private to me". It does not
    /// know the account's name and is not trusted to assign one: the bridge
    /// reads this — or any other name — as *private* and stores the name of
    /// the caller that is signed in.
    pub const OWNER_SELF: &'static str = "@me";

    /// What a browser writes in `owner` to say "everyone's". Spelled out
    /// rather than left empty, because empty has to mean *nobody said*: an
    /// entry that reaches the bridge with no owner came from somewhere with
    /// no checkbox — an import, a `redis://` link — and the bridge then keeps
    /// a new entry private and an edited one as it was. Reading empty as
    /// "shared" published every imported entry, credentials and all, to
    /// every account. Never stored: the bridge turns it into no owner.
    pub const OWNER_SHARED: &'static str = "@shared";

    /// The fields that are secrets, by their serialized names: what is
    /// encrypted at rest, what a diagnostics bundle redacts, and what the
    /// HTTP bridge never sends to a browser. One list, so a new secret is
    /// added in one place (plus `secret_mut` below, which the compiler then
    /// does not let drift: every name here must resolve there).
    pub const SECRET_FIELDS: [&'static str; 7] = [
        "password",
        "sentinel_password",
        "ssh_password",
        "ssh_key",
        "ssh_key_passphrase",
        "client_key",
        "client_key_passphrase",
    ];

    /// The secret field called `name`, or `None` for a name that is not one.
    pub fn secret_mut(&mut self, name: &str) -> Option<&mut Option<String>> {
        match name {
            "password" => Some(&mut self.password),
            "sentinel_password" => Some(&mut self.sentinel_password),
            "ssh_password" => Some(&mut self.ssh_password),
            "ssh_key" => Some(&mut self.ssh_key),
            "ssh_key_passphrase" => Some(&mut self.ssh_key_passphrase),
            "client_key" => Some(&mut self.client_key),
            "client_key_passphrase" => Some(&mut self.client_key_passphrase),
            _ => None,
        }
    }

    /// Take every secret out, answering which of them held a value — what
    /// the bridge lists for a browser: the settings, and *that* a password
    /// is stored, never the password.
    pub fn take_secrets(&mut self) -> Vec<&'static str> {
        let mut set = Vec::new();
        for name in Self::SECRET_FIELDS {
            if let Some(field) = self.secret_mut(name)
                && field.take().is_some_and(|value| !value.is_empty())
            {
                set.push(name);
            }
        }
        set
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

    /// SCAN page size: the entry's own, else the one chosen in Settings
    /// (`global`, `None` while unset), else the default — the smaller one on
    /// a production-tagged entry. Only the default differs by tag. Typed
    /// values are clamped to `MIN_KEY_SCAN_COUNT..=MAX_KEY_SCAN_COUNT`.
    pub fn resolve_key_scan_count(&self, global: Option<usize>) -> usize {
        let default = if self.is_high_risk_tag() {
            PRODUCTION_KEY_SCAN_COUNT
        } else {
            DEFAULT_KEY_SCAN_COUNT
        };
        self.key_scan_count
            .filter(|&n| n > 0)
            .or(global.filter(|&n| n > 0))
            .unwrap_or(default)
            .clamp(MIN_KEY_SCAN_COUNT, MAX_KEY_SCAN_COUNT)
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
    /// Whether writes to this entry start locked — the app's `SafeMode`, to
    /// be unlocked for `WRITE_UNLOCK_SECS` at a time and re-engaged after —
    /// and, through the bridge, refused outside such a window. Set by the
    /// entry, else by its tag: production is locked by default, which is
    /// what the tag is for (ADR 14).
    pub fn write_locked(&self) -> bool {
        // Read-only is the stronger answer: an entry that may not write at
        // all has no window to open.
        !self.readonly.unwrap_or(false) && self.write_lock.unwrap_or_else(|| self.is_high_risk_tag())
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

    /// A `host` that is a filesystem path is a Unix domain socket
    /// (`/var/run/redis.sock`): no TCP, no TLS, no tunnel.
    pub fn is_unix_socket(&self) -> bool {
        self.host.starts_with('/')
    }

    /// The addresses to dial first, in order. `host` normally holds one
    /// name; a Sentinel entry may list several seeds as
    /// `host[:port], host[:port], …` — an entry without a port takes
    /// `port` — and discovery walks them until one answers. Everything
    /// after the seed (masters, cluster nodes) comes from the server.
    pub fn seed_endpoints(&self) -> Vec<(String, u16)> {
        if self.is_unix_socket() || !self.host.contains(',') {
            return vec![(self.host.clone(), self.port)];
        }
        self.host
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|entry| {
                let (host, port) = split_host_port_or(entry, self.port);
                (host.to_string(), port)
            })
            .collect()
    }

    /// The first seed — what single-address code paths dial.
    pub fn primary_endpoint(&self) -> (String, u16) {
        self.seed_endpoints()
            .into_iter()
            .next()
            .unwrap_or_else(|| (self.host.clone(), self.port))
    }

    /// Generates the connection URL based on host, port, and optional password.
    /// An IPv6 literal host is bracketed, as a URL requires
    /// (`redis://[::1]:6379`); a Unix socket path becomes
    /// `redis+unix:///path?user=…&pass=…`.
    pub fn get_connection_url(&self) -> String {
        let tls = self.tls.unwrap_or(false);
        let scheme = if tls { "rediss" } else { "redis" };

        let safe_pwd = self.password.as_deref().filter(|s| !s.trim().is_empty());
        let safe_usr = self.username.as_deref().filter(|s| !s.trim().is_empty());

        if self.is_unix_socket() {
            let mut query = Vec::new();
            if let Some(user) = safe_usr {
                query.push(format!("user={}", utf8_percent_encode(user, NON_ALPHANUMERIC)));
            }
            if let Some(pwd) = safe_pwd {
                query.push(format!("pass={}", utf8_percent_encode(pwd, NON_ALPHANUMERIC)));
            }
            let query = if query.is_empty() {
                String::new()
            } else {
                format!("?{}", query.join("&"))
            };
            return format!("redis+unix://{}{query}", self.host);
        }

        let (primary_host, port) = self.primary_endpoint();
        let host = bracket_ipv6(&primary_host);

        let url = match (safe_pwd, safe_usr) {
            (Some(pwd), Some(username)) => {
                let pwd_enc = utf8_percent_encode(pwd, NON_ALPHANUMERIC).to_string();
                let username_enc = utf8_percent_encode(username, NON_ALPHANUMERIC).to_string();
                format!("{scheme}://{username_enc}:{pwd_enc}@{host}:{port}")
            }
            (Some(pwd), None) => {
                let pwd_enc = utf8_percent_encode(pwd, NON_ALPHANUMERIC).to_string();
                format!("{scheme}://:{pwd_enc}@{host}:{port}")
            }
            _ => format!("{scheme}://{host}:{port}"),
        };
        if tls && self.insecure.unwrap_or(false) {
            return format!("{url}/#insecure");
        }

        url
    }
}

/// Real `redis-servers.toml` files from earlier releases: every entry must
/// keep parsing (an upgrade that loses connections is the worst regression
/// this file can have) and every field that still exists must survive.
#[cfg(test)]
mod host_field_tests {
    use super::parse_url;

    #[test]
    fn a_pasted_url_gives_its_credentials_decoded() {
        let url = parse_url("redis://app:p%40ss%3Aw0rd@10.0.0.5:6380".to_string());
        assert_eq!((url.host.as_str(), url.port), ("10.0.0.5", Some(6380)));
        assert_eq!(url.username, "app");
        assert_eq!(url.password.as_deref(), Some("p@ss:w0rd"));
    }

    #[test]
    fn a_socket_path_stays_the_host() {
        assert_eq!(parse_url("/var/run/redis.sock".to_string()).host, "/var/run/redis.sock");
        let url = parse_url("redis+unix:///tmp/redis.sock".to_string());
        assert_eq!(url.host, "/tmp/redis.sock");
        assert_eq!(parse_url("unix:///tmp/r.sock".to_string()).host, "/tmp/r.sock");
    }
}

#[cfg(all(test, not(target_family = "wasm")))]
mod secret_tests {
    use super::store::map_secrets;
    use super::*;

    #[test]
    fn every_secret_field_is_encrypted_at_rest() {
        let mut server = RedisServer::default();
        for name in RedisServer::SECRET_FIELDS {
            *server.secret_mut(name).expect("a secret field") = Some(format!("plain-{name}"));
        }
        map_secrets(&mut server, |value| Ok(format!("enc({value})"))).expect("map");
        for name in RedisServer::SECRET_FIELDS {
            let stored = server.secret_mut(name).expect("a secret field").clone();
            assert_eq!(
                stored,
                Some(format!("enc(plain-{name})")),
                "{name} must not be written in the clear"
            );
        }
        // An unset secret stays unset.
        let mut bare = RedisServer::default();
        map_secrets(&mut bare, |value| Ok(format!("enc({value})"))).expect("map");
        assert!(bare.client_key.is_none());
    }
}

#[cfg(test)]
mod upgrade_fixtures {
    use super::store::RedisServers;
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
    use super::form::{write_lock_from_form_value, writes_from_form_value};
    use super::store::{redact_servers, server_sort_key};
    use super::*;

    /// What `import-local` reads: no file and an empty one are no entries,
    /// a damaged one is an error, and a file's entries come back as written.
    #[test]
    fn a_server_file_is_read_without_touching_the_list() {
        let dir = std::env::temp_dir().join(format!("zedis-read-servers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("redis-servers.toml");
        assert!(read_servers_file(&path).expect("no file").is_empty());
        std::fs::write(&path, "\n").expect("write");
        assert!(read_servers_file(&path).expect("empty file").is_empty());
        std::fs::write(
            &path,
            "[[servers]]\nid = \"a\"\nname = \"one\"\nhost = \"h\"\nport = 1\n",
        )
        .expect("write");
        let servers = read_servers_file(&path).expect("file");
        assert_eq!((servers.len(), servers[0].name.as_str()), (1, "one"));
        std::fs::write(&path, "not toml [").expect("write");
        assert!(read_servers_file(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_listed_secret_is_a_field_and_taking_them_leaves_none() {
        let mut server = RedisServer::default();
        for name in RedisServer::SECRET_FIELDS {
            let field = server
                .secret_mut(name)
                .expect("a listed secret must resolve to a field");
            *field = Some(format!("value-of-{name}"));
        }
        assert!(server.secret_mut("host").is_none(), "a setting is not a secret");
        // An empty string is what a cleared form field leaves: not "set".
        server.ssh_password = Some(String::new());

        let set = server.take_secrets();
        assert_eq!(set.len(), RedisServer::SECRET_FIELDS.len() - 1);
        assert!(!set.contains(&"ssh_password"));
        let json = serde_json::to_string(&server).expect("json");
        assert!(!json.contains("value-of-"), "no secret survives take_secrets: {json}");
    }

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
    fn import_uri_reads_valkey_schemes_like_their_redis_twins() {
        // What Aiven's console hands out and `valkey-cli -u` takes.
        let tls = RedisServer::from_import_uri("valkeys://default:s3cr3t@valkey-demo.aivencloud.com:12691/2")
            .expect("valkeys uri");
        assert_eq!(tls.host, "valkey-demo.aivencloud.com");
        assert_eq!(tls.port, 12691);
        assert_eq!(tls.username.as_deref(), Some("default"));
        assert_eq!(tls.password.as_deref(), Some("s3cr3t"));
        assert_eq!(tls.default_db, Some(2));
        assert_eq!(tls.tls, Some(true));
        // Dialed as the format redis-rs knows, whatever it was pasted as.
        assert!(tls.get_connection_url().starts_with("rediss://"));

        let plain = RedisServer::from_import_uri("valkey://h:6380").expect("valkey uri");
        assert_eq!(plain.port, 6380);
        assert!(plain.tls.is_none());

        // Schemes are case-insensitive, and the dispatch agrees with the parser.
        let shouted = RedisServer::from_import("  VALKEYS://h:6379  ").expect("uri branch");
        assert_eq!(shouted.tls, Some(true));
        // The server form's host field takes one too.
        assert!(parse_url("valkeys://h:6379".to_string()).tls);
    }

    #[test]
    fn the_write_lock_follows_the_tag_unless_the_entry_says() {
        let mut server = RedisServer::default();
        assert!(!server.write_locked(), "an untagged entry is not locked");
        server.tag_color = Some("magenta".to_string());
        assert!(server.write_locked(), "production is, by its tag");
        server.write_lock = Some(false);
        assert!(!server.write_locked(), "unless it opts out");
        server.tag_color = None;
        server.write_lock = Some(true);
        assert!(server.write_locked(), "and any entry may opt in");
        server.readonly = Some(true);
        assert!(
            !server.write_locked(),
            "read-only is the stronger answer: nothing to unlock"
        );
        server.readonly = None;
        // The form's three answers, and what an older config or an import says.
        for (given, want) in [
            (None, None),
            (Some("0"), None),
            (Some("default"), None),
            (Some("1"), Some(true)),
            (Some("locked"), Some(true)),
            (Some("true"), Some(true)),
            (Some("2"), Some(false)),
            (Some("unlocked"), Some(false)),
            (Some("false"), Some(false)),
        ] {
            assert_eq!(write_lock_from_form_value(given), want, "{given:?}");
        }
        // The form's one choice, what it stores, and the way back.
        for (choice, want) in [
            ("0", (None, None)),
            ("1", (None, Some(false))),
            ("2", (None, Some(true))),
            ("3", (Some(true), None)),
        ] {
            assert_eq!(writes_from_form_value(choice), want, "{choice}");
            let (readonly, write_lock) = want;
            assert_eq!(
                writes_index(readonly, write_lock).to_string(),
                choice,
                "{choice} round-trips"
            );
        }
        assert_eq!(writes_from_form_value("readonly"), (Some(true), None));
        assert_eq!(writes_from_form_value("nonsense"), (None, None));
        assert_eq!(writes_index(Some(true), Some(true)), 3, "read-only wins over the lock");
        assert_eq!(writes_index(Some(false), Some(true)), 2);
    }

    #[test]
    fn only_the_four_schemes_count_as_connection_uris() {
        for uri in ["redis://h", "rediss://h", "valkey://h", "valkeys://h", " Redis://h "] {
            assert!(is_connection_uri(uri), "{uri}");
        }
        for other in [
            "http://h",
            "redis:h",
            "h:6379",
            r#"{"host":"redis://h"}"#,
            "--port=6379",
        ] {
            assert!(!is_connection_uri(other), "{other}");
        }
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

    /// Only the *default* is smaller on production: a count set in Settings
    /// or on the entry is what the user asked for, tag or no tag.
    #[test]
    fn a_production_entry_scans_in_smaller_pages_by_default() {
        let mut s = sample_server();
        s.tag_color = Some("magenta".into());
        assert_eq!(s.resolve_key_scan_count(None), PRODUCTION_KEY_SCAN_COUNT);
        assert_eq!(s.resolve_key_scan_count(Some(20_000)), 20_000);
        s.key_scan_count = Some(5_000);
        assert_eq!(s.resolve_key_scan_count(None), 5_000);
        // Any other tag is not production.
        s.key_scan_count = None;
        s.tag_color = Some("teal".into());
        assert_eq!(s.resolve_key_scan_count(None), DEFAULT_KEY_SCAN_COUNT);
    }

    #[test]
    fn a_typed_scan_count_is_clamped() {
        let mut s = sample_server();
        s.key_scan_count = Some(5);
        assert_eq!(s.resolve_key_scan_count(None), MIN_KEY_SCAN_COUNT);
        s.key_scan_count = Some(500);
        assert_eq!(s.resolve_key_scan_count(Some(2_000)), MIN_KEY_SCAN_COUNT);
        s.key_scan_count = Some(1_000_000);
        assert_eq!(s.resolve_key_scan_count(None), MAX_KEY_SCAN_COUNT);
        s.key_scan_count = None;
        assert_eq!(s.resolve_key_scan_count(Some(5)), MIN_KEY_SCAN_COUNT);
        assert_eq!(s.resolve_key_scan_count(Some(1_000_000)), MAX_KEY_SCAN_COUNT);
    }

    #[test]
    fn key_behavior_overrides_fall_back_to_global() {
        let mut s = sample_server();
        // The sample is tagged production, so its default is that one.
        assert_eq!(s.resolve_key_scan_count(None), PRODUCTION_KEY_SCAN_COUNT);
        assert_eq!(s.resolve_key_scan_count(Some(2_000)), 2_000);
        assert_eq!(s.resolve_max_key_tree_depth(5), 5);
        assert_eq!(s.resolve_auto_expand_threshold(100), 100);
        assert!(s.resolve_show_key_tree_ttl(true));
        assert!(!s.resolve_show_key_tree_ttl(false));
        assert_eq!(s.show_key_tree_ttl_form_index(), 0);

        s.key_scan_count = Some(5_000);
        s.max_key_tree_depth = Some(3);
        s.auto_expand_threshold = Some(50);
        s.show_key_tree_ttl = Some(false);
        assert_eq!(s.resolve_key_scan_count(Some(2_000)), 5_000);
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
