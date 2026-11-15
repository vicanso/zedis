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

//! The bridge's stores in Redis or Valkey (`--store redis://…` or
//! `rediss://…` / `valkey://…` / `valkeys://…`): every trait of `store.rs`
//! under a key prefix (`zedis:` by default, or `?prefix=…`), using a hybrid
//! TTL strategy:
//! - Core configuration and authored data (`zedis:meta`, `zedis:versions`,
//!   `zedis:servers`, `zedis:accounts`, `zedis:scripts`,
//!   `zedis:page_rows:{account}`, `zedis:key_notes:{server_id}`) are kept
//!   without a TTL and synchronized across bridge processes via
//!   `zedis:versions` (`store::keep_in_step`).
//! - Session leases, temporary tokens and operational history (`zedis:logins`,
//!   `zedis:invites`, `zedis:signups`, `zedis:setup_token`, `zedis:throttle`,
//!   `zedis:trash:{server_id}`, `zedis:metrics:{server_id}`, `zedis:audit`)
//!   refresh a sliding TTL on every write so idle or abandoned ephemeral keys
//!   reclaim themselves automatically.

mod auth;
mod data;

pub(crate) use data::RedisAudit;

use crate::auth::hex;
use crate::metrics::{self, RETENTION_DAYS};
use crate::sql::redacted;
use crate::store::Stores;
use crate::trash::{Meta, RETENTION};
use redis::aio::MultiplexedConnection;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use std::io;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use zedis_connection::string::{decrypt, encrypt};

const DEFAULT_PREFIX: &str = "zedis";
const MASTER_KEY_CHECK: &str = "master_key_check";
const MASTER_KEY_CHECK_TEXT: &str = "zedis-bridge master key check";

/// Sliding TTLs (in seconds) refreshed whenever ephemeral or historical stores
/// are written.
const LOGINS_TTL_SECS: i64 = 30 * 86_400;
const INVITES_SIGNUPS_TTL_SECS: i64 = 30 * 86_400;
const SETUP_TTL_SECS: i64 = 7 * 86_400;
const THROTTLE_TTL_SECS: i64 = 3_600;
const PRESENCE_TTL_SECS: i64 = 600;
const TRASH_TTL_SECS: i64 = RETENTION.as_secs() as i64 + 3_600;
const METRICS_TTL_SECS: i64 = (RETENTION_DAYS + 1) * 86_400;
const AUDIT_TTL_SECS: i64 = 90 * 86_400;
const AUDIT_MAX_ENTRIES: isize = 50_000;

/// Atomic optimistic-concurrency update for `AccountStore::apply`.
///
/// KEYS: `[versions, accounts, invites, signups]`
/// ARGV: `[seen, ttl_secs, op_kind, key, doc, ...]`
const ACCOUNT_APPLY_LUA: &str = r#"
local cur = tonumber(redis.call('HGET', KEYS[1], 'accounts') or '0')
if cur ~= tonumber(ARGV[1]) then
    return -1
end
local next_ver = cur + 1
redis.call('HSET', KEYS[1], 'accounts', tostring(next_ver))
local ttl = tonumber(ARGV[2])
local i = 3
while i <= #ARGV do
    local op = ARGV[i]
    local k = ARGV[i + 1]
    local v = ARGV[i + 2]
    if op == 'put_account' then
        redis.call('HSET', KEYS[2], k, v)
    elseif op == 'rem_account' then
        redis.call('HDEL', KEYS[2], k)
    elseif op == 'put_invite' then
        redis.call('HSET', KEYS[3], k, v)
    elseif op == 'rem_invite' then
        redis.call('HDEL', KEYS[3], k)
    elseif op == 'put_signup' then
        redis.call('HSET', KEYS[4], k, v)
    elseif op == 'rem_signup' then
        redis.call('HDEL', KEYS[4], k)
    end
    i = i + 3
end
if redis.call('HLEN', KEYS[3]) > 0 then
    redis.call('EXPIRE', KEYS[3], ttl)
end
if redis.call('HLEN', KEYS[4]) > 0 then
    redis.call('EXPIRE', KEYS[4], ttl)
end
return next_ver
"#;

/// Atomic batch save for `LoginStore::save`.
///
/// KEYS: `[logins, versions]`
/// ARGV: `[ttl_secs, op_kind, login_key, last_used_secs, doc, ...]`
const LOGIN_SAVE_LUA: &str = r#"
local ttl = tonumber(ARGV[1])
local touched = false
local i = 2
while i <= #ARGV do
    local op = ARGV[i]
    local key = ARGV[i + 1]
    local ts = tonumber(ARGV[i + 2])
    local doc = ARGV[i + 3]
    if op == 'open' then
        local cur = redis.call('HGET', KEYS[1], key)
        if cur then
            local ok, parsed = pcall(cjson.decode, cur)
            if ok and type(parsed) == 'table' and tonumber(parsed.last_used or 0) > ts then
                local ok_new, next_obj = pcall(cjson.decode, doc)
                if ok_new and type(next_obj) == 'table' then
                    next_obj.last_used = parsed.last_used
                    doc = cjson.encode(next_obj)
                end
            end
        end
        redis.call('HSET', KEYS[1], key, doc)
        touched = true
    elseif op == 'use' then
        local cur = redis.call('HGET', KEYS[1], key)
        if cur then
            local ok, parsed = pcall(cjson.decode, cur)
            if ok and type(parsed) == 'table' then
                if ts > tonumber(parsed.last_used or 0) then
                    parsed.last_used = ts
                    redis.call('HSET', KEYS[1], key, cjson.encode(parsed))
                end
                touched = true
            end
        end
    elseif op == 'close' then
        if redis.call('HDEL', KEYS[1], key) > 0 then
            redis.call('HINCRBY', KEYS[2], 'logins_revoked', 1)
        end
    end
    i = i + 4
end
if touched and redis.call('HLEN', KEYS[1]) > 0 then
    redis.call('EXPIRE', KEYS[1], ttl)
end
return 1
"#;

/// Atomic clear of throttle entries matching `key_sha256`.
///
/// KEYS: `[throttle]`
/// ARGV: `[key_sha256]`
const THROTTLE_CLEAR_LUA: &str = r#"
local members = redis.call('ZRANGE', KEYS[1], 0, -1)
local removed = 0
for _, m in ipairs(members) do
    local ok, parsed = pcall(cjson.decode, m)
    if ok and type(parsed) == 'table' and parsed.h == ARGV[1] then
        redis.call('ZREM', KEYS[1], m)
        removed = removed + 1
    end
end
return removed
"#;

/// Atomic metric sample insertion (at most one per minute, rejecting late
/// samples for an earlier minute).
///
/// KEYS: `[metrics_server_key, metric_servers_set]`
/// ARGV: `[server_id, minute, ts_ms, member, ttl_secs]`
const METRIC_ADD_LUA: &str = r#"
local last = redis.call('ZRANGE', KEYS[1], -1, -1, 'WITHSCORES')
if #last >= 2 then
    local max_ts = tonumber(last[2])
    local max_min = math.floor(max_ts / 60000)
    if max_min >= tonumber(ARGV[2]) then
        return 0
    end
end
redis.call('ZADD', KEYS[1], tonumber(ARGV[3]), ARGV[4])
redis.call('EXPIRE', KEYS[1], tonumber(ARGV[5]))
redis.call('SADD', KEYS[2], ARGV[1])
redis.call('EXPIRE', KEYS[2], tonumber(ARGV[5]))
return 1
"#;

/// Atomic metric sample import (`ON CONFLICT (server_id, minute) DO NOTHING`).
///
/// KEYS: `[metrics_server_key, metric_servers_set]`
/// ARGV: `[server_id, min_start_ms, min_end_ms, ts_ms, member, ttl_secs]`
const METRIC_IMPORT_LUA: &str = r#"
local existing = redis.call('ZCOUNT', KEYS[1], tonumber(ARGV[2]), tonumber(ARGV[3]))
if existing > 0 then
    return 0
end
redis.call('ZADD', KEYS[1], tonumber(ARGV[4]), ARGV[5])
redis.call('EXPIRE', KEYS[1], tonumber(ARGV[6]))
redis.call('SADD', KEYS[2], ARGV[1])
redis.call('EXPIRE', KEYS[2], tonumber(ARGV[6]))
return 1
"#;

/// Atomic page row upsert enforcing `MAX_ACCOUNT_BYTES`.
///
/// KEYS: `[page_rows_account_key]`
/// ARGV: `[field, value, max_bytes]`
const PAGE_ROW_PUT_LUA: &str = r#"
local field = ARGV[1]
local val = ARGV[2]
local max_bytes = tonumber(ARGV[3])
local all = redis.call('HGETALL', KEYS[1])
local total = 0
local replaced = false
for i = 1, #all, 2 do
    local f = all[i]
    local v = all[i + 1]
    local sep = string.find(f, '\0', 1, true)
    local klen = sep and (#f - sep) or #f
    if f == field then
        total = total + klen + #val
        replaced = true
    else
        total = total + klen + #v
    end
end
if not replaced then
    local sep = string.find(field, '\0', 1, true)
    local klen = sep and (#field - sep) or #field
    total = total + klen + #val
end
if total > max_bytes then
    return 0
end
redis.call('HSET', KEYS[1], field, val)
return 1
"#;

/// Atomic prune of audit entries older than `before_ms`.
///
/// KEYS: `[audit_zset]`
/// ARGV: `[before_ms]`
const AUDIT_PRUNE_LUA: &str = r#"
local cutoff = tonumber(ARGV[1])
local members = redis.call('ZRANGE', KEYS[1], 0, -1)
local removed = 0
for _, m in ipairs(members) do
    local ok, parsed = pcall(cjson.decode, m)
    if ok and type(parsed) == 'table' and tonumber(parsed.at_ms or 0) < cutoff then
        redis.call('ZREM', KEYS[1], m)
        removed = removed + 1
    end
end
return removed
"#;

/// Whether `url` names a Redis or Valkey store (`redis://`, `rediss://`,
/// `valkey://`, `valkeys://`).
pub fn is_redis_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("redis://")
        || lower.starts_with("rediss://")
        || lower.starts_with("valkey://")
        || lower.starts_with("valkeys://")
}

/// Extract an optional `?prefix=…` query parameter from `url` and normalize
/// `valkey://` / `valkeys://` schemes for `redis::Client::open`.
pub(crate) fn parse_redis_url(url: &str) -> io::Result<(String, String)> {
    let trimmed = url.trim();
    let normalized = if let Some(rest) = trimmed.strip_prefix("valkey://") {
        format!("redis://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("valkeys://") {
        format!("rediss://{rest}")
    } else if trimmed.len() >= 9 && trimmed[..9].eq_ignore_ascii_case("valkey://") {
        format!("redis://{}", &trimmed[9..])
    } else if trimmed.len() >= 10 && trimmed[..10].eq_ignore_ascii_case("valkeys://") {
        format!("rediss://{}", &trimmed[10..])
    } else {
        trimmed.to_string()
    };

    let (base, query_and_frag) = match normalized.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (normalized.as_str(), None),
    };
    let mut prefix = DEFAULT_PREFIX.to_string();
    let mut kept_params = Vec::new();
    let mut fragment = None;

    if let Some(qf) = query_and_frag {
        let (query, frag) = match qf.split_once('#') {
            Some((q, f)) => (q, Some(f)),
            None => (qf, None),
        };
        fragment = frag;
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            if let Some(value) = pair.strip_prefix("prefix=") {
                let clean = value.trim_matches(':').trim();
                if clean.is_empty() {
                    return Err(io::Error::other("Redis store prefix cannot be empty"));
                }
                prefix = clean.to_string();
            } else {
                kept_params.push(pair);
            }
        }
    }

    let mut clean_url = base.to_string();
    if !kept_params.is_empty() {
        clean_url.push('?');
        clean_url.push_str(&kept_params.join("&"));
    }
    if let Some(frag) = fragment {
        clean_url.push('#');
        clean_url.push_str(frag);
    }
    Ok((clean_url, prefix))
}

/// The bridge's stores in one Redis / Valkey instance.
#[derive(Clone)]
pub struct RedisStore {
    client: redis::Client,
    conn: Arc<RwLock<MultiplexedConnection>>,
    prefix: Arc<str>,
    server_edit: Arc<Mutex<()>>,
    trash_edit: Arc<Mutex<()>>,
}

impl RedisStore {
    /// Connect to `url`, verify connectivity, and check `master.key` against
    /// `{prefix}:meta`.
    pub async fn connect(url: &str) -> io::Result<Self> {
        let (clean_url, prefix) = parse_redis_url(url)?;
        let client =
            redis::Client::open(clean_url.as_str()).map_err(|e| io::Error::other(format!("{}: {e}", redacted(url))))?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| io::Error::other(format!("{}: {e}", redacted(url))))?;
        let store = Self {
            client,
            conn: Arc::new(RwLock::new(conn)),
            prefix: Arc::from(prefix),
            server_edit: Arc::default(),
            trash_edit: Arc::default(),
        };
        store.check_master_key().await?;
        Ok(store)
    }

    /// Key prefix configured for this store (e.g. `"zedis"`).
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    fn key(&self, suffix: &str) -> String {
        format!("{}:{suffix}", self.prefix)
    }

    fn scoped_key(&self, domain: &str, id: &str) -> String {
        format!("{}:{domain}:{id}", self.prefix)
    }

    /// Run an async Redis operation on the multiplexed connection, reconnecting
    /// once if the underlying connection was closed or dropped.
    async fn run<T, F, Fut>(&self, op: F) -> io::Result<T>
    where
        F: Fn(MultiplexedConnection) -> Fut,
        Fut: std::future::Future<Output = redis::RedisResult<T>>,
    {
        let conn = self.conn.read().await.clone();
        match op(conn).await {
            Ok(value) => Ok(value),
            Err(e) if e.is_io_error() || e.is_connection_dropped() => {
                let fresh = self
                    .client
                    .get_multiplexed_async_connection()
                    .await
                    .map_err(redis_err)?;
                *self.conn.write().await = fresh.clone();
                op(fresh).await.map_err(redis_err)
            }
            Err(e) => Err(redis_err(e)),
        }
    }

    /// Every store on this Redis instance.
    pub fn stores(self) -> Stores {
        let store = Arc::new(self);
        Stores {
            accounts: store.clone(),
            logins: store.clone(),
            servers: store.clone(),
            trash: store.clone(),
            metrics: store.clone(),
            page_rows: store.clone(),
            key_notes: store.clone(),
            scripts: store.clone(),
            setup: store.clone(),
            audit: Some(Arc::new(RedisAudit::start(store.as_ref().clone()))),
            throttle: Some(store.clone()),
            presence: Some(store.clone()),
            versions: Some(store),
        }
    }

    /// Verify that this bridge's `master.key` matches the one that initialized
    /// `{prefix}:meta`.
    async fn check_master_key(&self) -> io::Result<()> {
        let sealed = encrypt(MASTER_KEY_CHECK_TEXT).map_err(|e| io::Error::other(e.to_string()))?;
        let meta_key = self.key("meta");
        let stored: String = self
            .run(|mut conn| {
                let meta_key = meta_key.clone();
                let sealed = sealed.clone();
                async move {
                    let _: i64 = redis::cmd("HSETNX")
                        .arg(&meta_key)
                        .arg(MASTER_KEY_CHECK)
                        .arg(&sealed)
                        .query_async(&mut conn)
                        .await?;
                    redis::cmd("HGET")
                        .arg(&meta_key)
                        .arg(MASTER_KEY_CHECK)
                        .query_async(&mut conn)
                        .await
                }
            })
            .await?;
        match decrypt(&stored) {
            Ok(text) if text == MASTER_KEY_CHECK_TEXT => Ok(()),
            _ => Err(io::Error::other(
                "this bridge's master.key is not the one the store's other bridges use: \
                 give every bridge on one store the same master.key",
            )),
        }
    }

    /// Whether the Redis store holds no account, server entry or script — what
    /// `import-local` checks before copying a local directory in.
    pub async fn is_empty(&self) -> io::Result<bool> {
        let accounts_key = self.key("accounts");
        let servers_key = self.key("servers");
        let scripts_key = self.key("scripts");
        let (a, s, sc): (u64, u64, u64) = self
            .run(|mut conn| {
                let accounts_key = accounts_key.clone();
                let servers_key = servers_key.clone();
                let scripts_key = scripts_key.clone();
                async move {
                    redis::pipe()
                        .cmd("HLEN")
                        .arg(&accounts_key)
                        .cmd("HLEN")
                        .arg(&servers_key)
                        .cmd("HLEN")
                        .arg(&scripts_key)
                        .query_async(&mut conn)
                        .await
                }
            })
            .await?;
        Ok(a + s + sc == 0)
    }

    /// Copy a recycle-bin entry from `import-local` preserving its `id` and
    /// `deleted_at_ms`.
    pub(crate) async fn import_trash(&self, server_id: &str, meta: &Meta, payload: Vec<u8>) -> io::Result<()> {
        let meta_doc = to_json(meta)?;
        let trash_key = self.scoped_key("trash", server_id);
        let payload_key = self.scoped_key("trash_payload", server_id);
        let servers_key = self.key("trash_servers");
        let server_id = server_id.to_string();
        let id = meta.id.clone();
        self.run(|mut conn| {
            let trash_key = trash_key.clone();
            let payload_key = payload_key.clone();
            let servers_key = servers_key.clone();
            let server_id = server_id.clone();
            let id = id.clone();
            let meta_doc = meta_doc.clone();
            let payload = payload.clone();
            async move {
                redis::pipe()
                    .cmd("SADD")
                    .arg(&servers_key)
                    .arg(&server_id)
                    .ignore()
                    .cmd("EXPIRE")
                    .arg(&servers_key)
                    .arg(TRASH_TTL_SECS)
                    .ignore()
                    .cmd("HSET")
                    .arg(&trash_key)
                    .arg(&id)
                    .arg(&meta_doc)
                    .ignore()
                    .cmd("EXPIRE")
                    .arg(&trash_key)
                    .arg(TRASH_TTL_SECS)
                    .ignore()
                    .cmd("HSET")
                    .arg(&payload_key)
                    .arg(&id)
                    .arg(&payload)
                    .ignore()
                    .cmd("EXPIRE")
                    .arg(&payload_key)
                    .arg(TRASH_TTL_SECS)
                    .ignore()
                    .query_async::<()>(&mut conn)
                    .await
            }
        })
        .await
    }

    /// Copy a metrics sample from `import-local` preserving the minute it was
    /// taken.
    pub(crate) async fn import_metric(&self, server_id: &str, ts_ms: i64, sample: &RawValue) -> io::Result<()> {
        let minute = ts_ms.div_euclid(metrics::INTERVAL_MS);
        let min_start = minute * metrics::INTERVAL_MS;
        let min_end = (minute + 1) * metrics::INTERVAL_MS - 1;
        let member = format!("{minute}:{ts_ms}\n{}", sample.get());
        let metrics_key = self.scoped_key("metrics", server_id);
        let servers_key = self.key("metric_servers");
        let server_id = server_id.to_string();
        let script = redis::Script::new(METRIC_IMPORT_LUA);
        let _: i64 = self
            .run(|mut conn| {
                let script = script.clone();
                let metrics_key = metrics_key.clone();
                let servers_key = servers_key.clone();
                let server_id = server_id.clone();
                let member = member.clone();
                async move {
                    script
                        .key(&metrics_key)
                        .key(&servers_key)
                        .arg(&server_id)
                        .arg(min_start)
                        .arg(min_end)
                        .arg(ts_ms)
                        .arg(&member)
                        .arg(METRICS_TTL_SECS)
                        .invoke_async(&mut conn)
                        .await
                }
            })
            .await?;
        Ok(())
    }
}

// ---- helpers -------------------------------------------------------------------

fn sha256_hex(text: &str) -> String {
    hex(&Sha256::digest(text.as_bytes()))
}

fn redis_err(e: redis::RedisError) -> io::Error {
    io::Error::other(e)
}

fn to_json<T: Serialize>(value: &T) -> io::Result<String> {
    serde_json::to_string(value).map_err(io::Error::other)
}

fn from_json<T: for<'de> Deserialize<'de>>(doc: &str) -> io::Result<T> {
    serde_json::from_str(doc).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{AccountOp, AccountRecord};
    use crate::auth::{Login, LoginChange};
    use crate::page_data::{KeyNote, ScriptRecord};
    use crate::store::{
        AccountStore, AuditEntry, AuditQuery, KeyNoteStore, LoginStore, PageRowStore, ScriptStore, SetupStore,
    };
    use crate::trash::NewEntry;
    use std::sync::Once;
    use std::time::Duration;
    use uuid::Uuid;
    use zedis_connection::RedisServer;
    use zedis_connection::{disable_keychain, get_server, install_crypto_provider};

    const TEST_URL_ENV: &str = "ZEDIS_BRIDGE_TEST_REDIS";

    fn unique(prefix: &str) -> String {
        format!("{prefix}-{}", Uuid::new_v4().simple())
    }

    async fn test_store() -> RedisStore {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            let dir = std::env::temp_dir().join(format!("zedis-bridge-redis-test-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            zedis_core::fs::override_config_dir(dir);
            install_crypto_provider();
            disable_keychain();
        });
        let base_url = std::env::var(TEST_URL_ENV).unwrap_or_else(|_| "redis://127.0.0.1:46379".to_string());
        let sep = if base_url.contains('?') { '&' } else { '?' };
        let url = format!("{base_url}{sep}prefix={}", unique("zedis-test"));
        RedisStore::connect(&url).await.expect("connect to test Redis")
    }

    async fn key_ttl(store: &RedisStore, key: &str) -> i64 {
        let key = key.to_string();
        store
            .run(|mut conn| {
                let key = key.clone();
                async move { redis::cmd("TTL").arg(&key).query_async(&mut conn).await }
            })
            .await
            .expect("TTL")
    }

    #[test]
    fn recognizes_redis_and_valkey_store_urls() {
        assert!(is_redis_url("redis://127.0.0.1:6379"));
        assert!(is_redis_url("rediss://:secret@redis.example.com:6380/0"));
        assert!(is_redis_url("valkey://127.0.0.1:6379/1?prefix=zedis:prod"));
        assert!(is_redis_url("valkeys://redis.example.com:6380"));
        assert!(!is_redis_url("postgres://localhost/zedis"));
        assert!(!is_redis_url("mysql://localhost/zedis"));
    }

    #[test]
    fn extracts_prefix_and_normalizes_scheme() {
        let (url, prefix) = parse_redis_url("redis://127.0.0.1:6379/0").expect("default prefix");
        assert_eq!(url, "redis://127.0.0.1:6379/0");
        assert_eq!(prefix, "zedis");

        let (url, prefix) =
            parse_redis_url("valkeys://user:pass@127.0.0.1:6380/2?prefix=zedis:staging:&protocol=resp3#main")
                .expect("custom prefix");
        assert_eq!(url, "rediss://user:pass@127.0.0.1:6380/2?protocol=resp3#main");
        assert_eq!(prefix, "zedis:staging");

        assert!(parse_redis_url("redis://127.0.0.1:6379?prefix=:::").is_err());
    }

    #[tokio::test]
    #[ignore]
    async fn every_store_keeps_the_contract_and_hybrid_ttl_rules() {
        let store = test_store().await;
        crate::store::conformance::keeps_the_contract(&store.clone().stores()).await;

        // Verify hybrid TTL strategy:
        // 1. Core config keys (`meta`, `versions`, `accounts`) are permanent (TTL == -1).
        let stores = store.clone().stores();
        let ver = stores.accounts.version().await.expect("version");
        stores
            .accounts
            .apply(
                ver,
                vec![AccountOp::PutAccount(AccountRecord {
                    name: unique("perm"),
                    ..AccountRecord::default()
                })],
            )
            .await
            .expect("apply")
            .expect("version moved");
        assert_eq!(key_ttl(&store, &store.key("meta")).await, -1);
        assert_eq!(key_ttl(&store, &store.key("versions")).await, -1);
        assert_eq!(key_ttl(&store, &store.key("accounts")).await, -1);

        // 2. Session leases & temporary tokens (`logins`, `setup_token`, `throttle`) have sliding TTL > 0.
        stores.setup.put("tok-ttl".to_string()).await.expect("put setup");
        stores
            .logins
            .save(vec![(
                unique("login"),
                LoginChange::Opened(Login {
                    account: "perm".to_string(),
                    version: 1,
                    last_used: 1_800_000_000,
                    remember: false,
                }),
            )])
            .await
            .expect("open login");
        assert!(key_ttl(&store, &store.key("setup_token")).await > 0);
        assert!(key_ttl(&store, &store.key("logins")).await > 0);
        let throttle = stores.throttle.as_ref().expect("throttle");
        throttle
            .failed("proc-a".to_string(), "ip:1.2.3.4".to_string(), 2_000)
            .await
            .expect("throttle failed");
        assert!(key_ttl(&store, &store.key("throttle")).await > 0);
    }

    #[tokio::test]
    #[ignore]
    async fn server_list_audit_and_import_local_work_in_redis() {
        use crate::accounts::{InviteKind, InviteRecord, LocalAccounts, LocalSetup, Role, SignupRecord};
        use crate::auth::LocalLogins;
        use crate::import::{copy, import_local};
        use crate::metrics::LocalMetrics;
        use crate::page_data::{LocalKeyNotes, LocalPageRows, LocalScripts};
        use crate::store::local_paths as at;
        use crate::trash::LocalTrash;

        let store = test_store().await;
        let stores = store.clone().stores();
        let id = unique("server");
        let first = RedisServer {
            id: id.clone(),
            name: "first".to_string(),
            host: "127.0.0.1".to_string(),
            port: 6379,
            password: Some("hunter2".to_string()),
            ..Default::default()
        };
        stores.servers.put(first.clone()).await.expect("put");
        assert_eq!(get_server(&id).expect("cached"), first);
        assert_eq!(
            key_ttl(&store, &store.key("servers")).await,
            -1,
            "servers key is permanent"
        );

        let raw: String = store
            .run(|mut conn| {
                let servers_key = store.key("servers");
                let id = id.clone();
                async move {
                    redis::cmd("HGET")
                        .arg(&servers_key)
                        .arg(&id)
                        .query_async(&mut conn)
                        .await
                }
            })
            .await
            .expect("raw server doc");
        assert!(!raw.contains("hunter2"), "secret is sealed in Redis");

        let edited = RedisServer {
            name: "edited".to_string(),
            ..first.clone()
        };
        stores.servers.put(edited.clone()).await.expect("edit");
        assert!(!stores.servers.swap(first, None).await.expect("stale swap refused"));
        assert!(stores.servers.swap(edited, None).await.expect("swap delete"));

        // Audit log append, query, prune, and sliding TTL
        let log = stores.audit.as_ref().expect("audit");
        let auditor = unique("auditor");
        for (n, event) in ["login", "logout", "server_deleted"].into_iter().enumerate() {
            log.append(AuditEntry {
                at_ms: 1_000 + n as i64,
                account: auditor.clone(),
                event: event.to_string(),
                server: (event == "server_deleted").then(|| "srv-a".to_string()),
                line: format!(r#"{{"event":"{event}","account":"{auditor}","n":{n}}}"#).into_bytes(),
            });
        }
        let mut kept = 0;
        for _ in 0..100 {
            kept = log
                .query(AuditQuery {
                    account: Some(auditor.clone()),
                    ..AuditQuery::default()
                })
                .await
                .expect("query")
                .lines
                .len();
            if kept == 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(kept, 3);
        assert!(
            key_ttl(&store, &store.key("audit")).await > 0,
            "audit key has sliding TTL"
        );
        assert!(log.prune(1_001).await.expect("prune") >= 1);

        // import-local into a fresh RedisStore prefix
        let import_store = test_store().await;
        let dir = std::env::temp_dir().join(unique("zedis-redis-import"));
        std::fs::create_dir_all(&dir).expect("dir");
        let (account, server, script, login_key) = (unique("erin"), unique("srv"), unique("script"), unique("login"));
        let record = AccountRecord {
            name: account.clone(),
            role: Role::Full,
            version: 2,
            ..AccountRecord::default()
        };
        let invite = InviteRecord {
            id: unique("invite"),
            code_sha256: "11".repeat(32),
            kind: InviteKind::Reset {
                account: account.clone(),
            },
            expires_at: 4_000_000_000,
            created_by: "root".to_string(),
            created_at: 1,
        };
        let request = SignupRecord {
            id: unique("signup"),
            name: unique("frank"),
            password: "hash".to_string(),
            note: "waiting".to_string(),
            peer: None,
            requested_at: 1,
            expires_at: 4_000_000_000,
        };
        LocalAccounts::new(dir.join(at::ACCOUNTS))
            .apply(
                0,
                vec![
                    AccountOp::PutAccount(record.clone()),
                    AccountOp::PutInvite(invite.clone()),
                    AccountOp::PutSignup(request.clone()),
                ],
            )
            .await
            .expect("accounts")
            .expect("applied");
        let login = Login {
            account: account.clone(),
            version: 2,
            last_used: 1_800_000_000,
            remember: true,
        };
        LocalLogins::new(dir.join(at::LOGINS))
            .save(vec![(login_key.clone(), LoginChange::Opened(login.clone()))])
            .await
            .expect("logins");
        std::fs::write(
            dir.join(at::SERVERS),
            format!("[[servers]]\nid = \"{server}\"\nname = \"imported\"\nhost = \"127.0.0.1\"\nport = 6379\n"),
        )
        .expect("servers");
        let saved = ScriptRecord {
            id: script.clone(),
            owner: Some(account.clone()),
            script: RawValue::from_string(r#"{"name":"moved"}"#.to_string()).expect("json"),
            updated_by: account.clone(),
            updated_at: 3,
        };
        ScriptStore::put(&LocalScripts::new(dir.join(at::SCRIPTS)), saved)
            .await
            .expect("script");
        PageRowStore::put(
            &LocalPageRows::new(dir.join(at::PAGE_ROWS)),
            account.clone(),
            "favorites".to_string(),
            server.clone(),
            Some("[\"k\"]".to_string()),
        )
        .await
        .expect("row");
        let note = KeyNote {
            tag: None,
            note: "moved".to_string(),
            by: account.clone(),
            at: 4,
        };
        KeyNoteStore::put(
            &LocalKeyNotes::new(dir.join(at::KEY_NOTES)),
            server.clone(),
            "user:1".to_string(),
            Some(note.clone()),
        )
        .await
        .expect("note");
        let bin = LocalTrash::new(dir.join(at::TRASH));
        let _deleted = bin
            .put(
                &server,
                NewEntry {
                    key: "user:2".to_string(),
                    db: 0,
                    pttl_ms: -1,
                    deleted_by: account.clone(),
                    payload: b"dump".to_vec(),
                    size: 4,
                    sealed: false,
                },
            )
            .expect("bin");
        let sampled_at = crate::metrics::now_ms();
        assert!(
            LocalMetrics::new(dir.join(at::METRICS))
                .put(&server, sampled_at, r#"{"ops":7}"#)
                .expect("sample")
        );
        let token = unique("token");
        LocalSetup::new(dir.join(at::SETUP_TOKEN))
            .put(token.clone())
            .await
            .expect("token");

        let done = copy(&dir, &import_store).await.expect("copied");
        assert_eq!(
            (
                done.accounts,
                done.invites,
                done.signups,
                done.logins,
                done.servers,
                done.scripts,
            ),
            (1, 1, 1, 1, 1, 1)
        );
        assert_eq!((done.page_rows, done.key_notes, done.trash, done.metrics), (1, 1, 1, 1));
        assert!(done.setup_token);
        assert!(
            import_local(&dir, None, &import_store).await.is_err(),
            "non-empty RedisStore is refused by import_local"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
