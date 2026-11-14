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

//! Entries in and out of Zedis: the export (JSON, one or many, secrets
//! optional), and the imports — Zedis's own JSON, a connection URI, several
//! pasted at once, a Redis Insight export.

use super::*;

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
    /// URI scheme other than `redis(s)` / `valkey(s)` (carries the offending
    /// scheme).
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
            clone.client_key_passphrase = None;
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
    /// [`Self::to_export_json`]) or a bare connection URI (see
    /// [`is_connection_uri`]), dispatching on the leading token so a pasted
    /// connection string "just works" alongside the JSON form.
    pub fn from_import(input: &str) -> Result<Self, ImportError> {
        let trimmed = input.trim();
        if is_connection_uri(trimmed) {
            Self::from_import_uri(trimmed)
        } else {
            Self::from_import_json(trimmed)
        }
    }

    /// Parse a Redis connection URI into a `RedisServer`. Supports the
    /// standard form `scheme://[username[:password]@]host[:port][/db]`,
    /// where the scheme is `redis` / `valkey`, or `rediss` / `valkeys` to
    /// enable TLS. Username and password are
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
    /// Returns `Err` for a malformed URI, a scheme other than those four, or
    /// a missing host. The entry always gets a fresh `id` so importing
    /// the same URI twice yields two distinct entries.
    pub fn from_import_uri(uri: &str) -> Result<Self, ImportError> {
        let parsed = Url::parse(&keep_first_host(uri.trim())).map_err(|e| ImportError::InvalidUri(e.to_string()))?;
        let Some(tls) = scheme_tls(parsed.scheme()) else {
            return Err(ImportError::UnsupportedScheme(parsed.scheme().to_string()));
        };
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
        let resp3 = parsed
            .query_pairs()
            .any(|(k, v)| k.eq_ignore_ascii_case("protocol") && v.eq_ignore_ascii_case("resp3"))
            .then_some(true);

        Ok(Self {
            id: Uuid::now_v7().to_string(),
            name: host.clone(),
            host,
            port,
            username,
            password,
            default_db,
            tls: tls.then_some(true),
            resp3,
            ..Default::default()
        })
    }

    /// Import **one or more** servers from pasted text — the entry point used
    /// by the paste-to-import dialog. Recognizes, in order:
    /// 1. a connection URI (`redis://` / `rediss://` / `valkey://` /
    ///    `valkeys://`) → one server,
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
}
