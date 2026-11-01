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

//! The server list: `redis-servers.toml` on the desktop, read once into the
//! in-memory cache every lookup answers from, written with its secrets sealed;
//! in the browser, the list the bridge answered, saved through it.

use super::*;

/// Wrapper struct to match the TOML `[[servers]]` structure.
#[cfg(not(target_family = "wasm"))]
#[derive(Debug, Default, Deserialize, Clone, Serialize)]
pub(crate) struct RedisServers {
    pub(super) servers: Vec<RedisServer>,
}

/// Gets or creates the path to the server configuration file.
#[cfg(not(target_family = "wasm"))]
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

/// Whether `SERVER_CONFIG_MAP` holds the list — read, saved or handed over —
/// rather than the empty map it starts as. An empty map is also the list
/// once the last connection is deleted, and taking it for "not read yet"
/// read the file again: a read in the moment between the save replacing the
/// map and its write landing put the deleted entry back, for the rest of
/// the session, in every list and lookup (#209).
static SERVER_CONFIG_LOADED: AtomicBool = AtomicBool::new(false);

/// Make `configs` the list, as read or as saved.
fn store_server_configs(configs: HashMap<String, RedisServer>) {
    SERVER_CONFIG_MAP.store(Arc::new(configs));
    SERVER_CONFIG_LOADED.store(true, Ordering::Release);
}

/// Returns the canonical sort order: group label A→Z (case-insensitive,
/// empty/None group sorts last under the "Ungrouped" header), then
/// within each group by `sort_order` ascending, then by `name`
/// case-insensitive as a stable tiebreaker. A `sort_order` of `None`
/// is treated as `i64::MAX` so brand-new servers (not yet renumbered)
/// land at the tail rather than at the head.
pub(super) fn server_sort_key(server: &RedisServer) -> (u8, String, i64, String) {
    let group = server.group.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let bucket = if group.is_some() { 0 } else { 1 };
    let group_key = group.map(|s| s.to_ascii_lowercase()).unwrap_or_default();
    let order = server.sort_order.unwrap_or(i64::MAX);
    let name_key = server.name.to_ascii_lowercase();
    (bucket, group_key, order, name_key)
}

/// The saved server list.
///
/// The in-memory map is the source both builds read. Behind it the desktop
/// has `redis-servers.toml`; the browser has the bridge, which fills the map
/// through [`set_servers_cache`] because it is the only process that may hold
/// the credentials (ADR 9).
pub fn get_servers() -> Result<Vec<RedisServer>> {
    if SERVER_CONFIG_LOADED.load(Ordering::Acquire) || !SERVER_CONFIG_MAP.load().is_empty() {
        let mut servers: Vec<RedisServer> = SERVER_CONFIG_MAP.load().values().cloned().collect();
        servers.sort_by_key(server_sort_key);
        return Ok(servers);
    }
    // No file to fall back to, and an empty list is the honest answer until
    // the bridge has answered.
    #[cfg(target_family = "wasm")]
    return Ok(Vec::new());
    #[cfg(not(target_family = "wasm"))]
    {
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
            // A value that does not decrypt was written in the clear (by an
            // older build, or by hand) and is taken as it is; the next save
            // encrypts it.
            let _ = map_secrets(server, |value| Ok(decrypt(value).unwrap_or_else(|_| value.to_string())));
            configs.insert(server.id.clone(), server.clone());
        }
        store_server_configs(configs);
        servers.sort_by_key(server_sort_key);
        Ok(servers)
    }
}

/// Replace the in-memory list with one a store other than
/// `redis-servers.toml` keeps — the bridge's database (ADR 18): what
/// `get_servers` / `get_server` answer, and the pool dials from, from now on.
/// Secrets opened ([`open_server_secrets`]). The file is neither read nor
/// written afterwards, and the desktop never calls this.
#[cfg(not(target_family = "wasm"))]
pub fn set_servers_cache(servers: Vec<RedisServer>) {
    let configs = servers
        .into_iter()
        .map(|server| (server.id.clone(), server))
        .collect::<HashMap<_, _>>();
    store_server_configs(configs);
}

/// The entries of the `redis-servers.toml` at `path`, secrets opened — what
/// `zedis-bridge import-local` copies into a database (ADR 18). Only read:
/// unlike the list's own loader, a damaged file is an error here and is
/// left where it is, and the cache is not touched. No file, or an empty
/// one — what a bridge that never had an entry leaves — is no entries.
#[cfg(not(target_family = "wasm"))]
pub fn read_servers_file(path: &std::path::Path) -> Result<Vec<RedisServer>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut servers = toml::from_str::<RedisServers>(&text)
        .map_err(|e| Error::Invalid { message: e.to_string() })?
        .servers;
    for server in servers.iter_mut() {
        open_server_secrets(server);
    }
    Ok(servers)
}

/// `server` as a store keeps it: each secret sealed with the master key, as
/// [`save_servers`] seals them for the file.
#[cfg(not(target_family = "wasm"))]
pub fn seal_server_secrets(server: &mut RedisServer) -> Result<()> {
    map_secrets(server, encrypt)
}

/// `server` as a store gave it back, secrets opened. A value that does not
/// open was written in the clear and is taken as it is — the file's reader
/// does the same.
#[cfg(not(target_family = "wasm"))]
pub fn open_server_secrets(server: &mut RedisServer) {
    let _ = map_secrets(server, |value| Ok(decrypt(value).unwrap_or_else(|_| value.to_string())));
}

/// Replace the in-memory list, for the browser build.
///
/// What the bridge answered to `GET /v1/servers`: ids and display names, and
/// deliberately no hosts and no secrets. Everything the UI does afterwards
/// names a server by id, and the bridge resolves it.
#[cfg(target_family = "wasm")]
pub fn set_servers_cache(servers: Vec<RedisServer>) {
    let configs = servers
        .into_iter()
        .map(|server| (server.id.clone(), server))
        .collect::<HashMap<_, _>>();
    store_server_configs(configs);
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

/// Saves the server list through the bridge.
///
/// The encryption and the file both live on the other side, so this only
/// updates what the UI reads and hands the list over. Without an installed
/// store it fails loudly rather than appearing to save (ADR 9).
#[cfg(target_family = "wasm")]
pub async fn save_servers(servers: Vec<RedisServer>) -> Result<()> {
    let store = crate::bridge::bridge_server_store().ok_or_else(|| Error::Invalid {
        message: "no bridge server store is installed; the server list is read-only".to_string(),
    })?;
    // The bridge's answer, not this side's list: a new entry comes back with
    // the id the bridge stamped, and the browser's copies of the rest carry
    // no credentials to be trusted as the record.
    let saved = store
        .save(servers)
        .await
        .map_err(|e| Error::Invalid { message: e.to_string() })?;
    set_servers_cache(saved);
    Ok(())
}

/// Pass every secret of `server` — each name in `RedisServer::SECRET_FIELDS`
/// — through `transform`: `encrypt` on the way to disk, `decrypt` on the way
/// back. Walking the list rather than naming fields is the point: the two
/// hand-written passes this replaced both skipped `client_key`, so a pasted
/// PEM private key was written to `redis-servers.toml` in the clear while
/// every other secret was encrypted.
#[cfg(not(target_family = "wasm"))]
pub(super) fn map_secrets(server: &mut RedisServer, mut transform: impl FnMut(&str) -> Result<String>) -> Result<()> {
    for name in RedisServer::SECRET_FIELDS {
        if let Some(slot) = server.secret_mut(name)
            && let Some(value) = slot.as_deref()
        {
            let mapped = transform(value)?;
            *slot = Some(mapped);
        }
    }
    Ok(())
}

/// Saves the server configuration to the file.
#[cfg(not(target_family = "wasm"))]
pub async fn save_servers(mut servers: Vec<RedisServer>) -> Result<()> {
    let mut configs = HashMap::new();
    for server in servers.iter_mut() {
        configs.insert(server.id.clone(), server.clone());
        map_secrets(server, encrypt)?;
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

    store_server_configs(configs);
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
#[cfg(not(target_family = "wasm"))]
pub fn servers_toml_redacted() -> Result<String> {
    redact_servers(get_servers()?)
}

#[cfg(not(target_family = "wasm"))]
pub(super) fn redact_servers(mut servers: Vec<RedisServer>) -> Result<String> {
    for server in servers.iter_mut() {
        for name in RedisServer::SECRET_FIELDS {
            if let Some(secret) = server.secret_mut(name)
                && secret.is_some()
            {
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
