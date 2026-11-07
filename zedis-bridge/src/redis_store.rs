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

use crate::accounts::{AccountOp, AccountRecord, InviteRecord, SignupRecord};
use crate::auth::{Login, LoginChange, hex};
use crate::metrics::{self, DAY_MS, RETENTION_DAYS};
use crate::page_data::{AccountRows, KeyNote, MAX_ACCOUNT_BYTES, RowError, ScriptRecord};
use crate::sql::redacted;
use crate::store::{
    AccountStore, AccountsAt, AuditEntry, AuditLog, AuditPage, AuditQuery, KeyNoteStore, LoginStore, MetricsStore,
    PageRowStore, ScriptStore, ServerStore, SetupStore, StoreFuture, StoreVersions, Stores, ThrottleStore, TrashStore,
    Versions,
};
use crate::trash::{self, MAX_BYTES, MAX_ENTRIES, MAX_STORED, Meta, NewEntry, PutError, RETENTION};
use futures::future::BoxFuture;
use redis::aio::MultiplexedConnection;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock, mpsc};
use uuid::Uuid;
use zedis_connection::string::{decrypt, encrypt};
use zedis_connection::{RedisServer, get_servers, open_server_secrets, seal_server_secrets, set_servers_cache};

const DEFAULT_PREFIX: &str = "zedis";
const MASTER_KEY_CHECK: &str = "master_key_check";
const MASTER_KEY_CHECK_TEXT: &str = "zedis-bridge master key check";

/// Sliding TTLs (in seconds) refreshed whenever ephemeral or historical stores
/// are written.
const LOGINS_TTL_SECS: i64 = 30 * 86_400;
const INVITES_SIGNUPS_TTL_SECS: i64 = 30 * 86_400;
const SETUP_TTL_SECS: i64 = 7 * 86_400;
const THROTTLE_TTL_SECS: i64 = 3_600;
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

    /// Append a batch of audit entries directly into `{prefix}:audit`.
    pub(crate) async fn write_audit_batch(&self, batch: Vec<AuditEntry>) -> io::Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let seq_key = self.key("audit_seq");
        let audit_key = self.key("audit");
        let count = batch.len() as i64;
        let end_id: u64 = self
            .run(|mut conn| {
                let seq_key = seq_key.clone();
                async move {
                    redis::cmd("INCRBY")
                        .arg(&seq_key)
                        .arg(count)
                        .query_async(&mut conn)
                        .await
                }
            })
            .await?;
        let start_id = end_id.saturating_sub(batch.len() as u64) + 1;
        let mut docs = Vec::with_capacity(batch.len());
        for (idx, entry) in batch.into_iter().enumerate() {
            let id = start_id + idx as u64;
            let stored = StoredAudit {
                id,
                at_ms: entry.at_ms,
                account: entry.account,
                event: entry.event,
                server: entry.server,
                line: String::from_utf8_lossy(&entry.line).into_owned(),
            };
            docs.push((id, to_json(&stored)?));
        }
        if docs.is_empty() {
            return Ok(());
        }
        self.run(|mut conn| {
            let audit_key = audit_key.clone();
            let seq_key = seq_key.clone();
            let docs = docs.clone();
            async move {
                let mut pipe = redis::pipe();
                for (id, doc) in &docs {
                    pipe.cmd("ZADD").arg(&audit_key).arg(*id as f64).arg(doc).ignore();
                }
                pipe.cmd("ZREMRANGEBYRANK")
                    .arg(&audit_key)
                    .arg(0isize)
                    .arg(-(AUDIT_MAX_ENTRIES + 1))
                    .ignore()
                    .cmd("EXPIRE")
                    .arg(&audit_key)
                    .arg(AUDIT_TTL_SECS)
                    .ignore()
                    .cmd("EXPIRE")
                    .arg(&seq_key)
                    .arg(AUDIT_TTL_SECS)
                    .ignore();
                pipe.query_async::<()>(&mut conn).await
            }
        })
        .await
    }
}

impl StoreVersions for RedisStore {
    fn read(&self) -> StoreFuture<Versions> {
        let store = self.clone();
        Box::pin(async move {
            let key = store.key("versions");
            let pairs: Vec<(String, u64)> = store
                .run(|mut conn| {
                    let key = key.clone();
                    async move { redis::cmd("HGETALL").arg(&key).query_async(&mut conn).await }
                })
                .await?;
            let mut versions = Versions::default();
            for (name, ver) in pairs {
                match name.as_str() {
                    "servers" => versions.servers = ver,
                    "accounts" => versions.accounts = ver,
                    "logins_revoked" => versions.logins_revoked = ver,
                    _ => {}
                }
            }
            Ok(versions)
        })
    }
}

// ---- accounts, invitations and signup requests ---------------------------------

impl AccountStore for RedisStore {
    fn load(&self) -> StoreFuture<AccountsAt> {
        let store = self.clone();
        Box::pin(async move {
            let versions_key = store.key("versions");
            let accounts_key = store.key("accounts");
            let invites_key = store.key("invites");
            let signups_key = store.key("signups");
            type RawLoaded = (
                Option<u64>,
                Vec<(String, String)>,
                Vec<(String, String)>,
                Vec<(String, String)>,
            );
            let (version, raw_accounts, raw_invites, raw_signups): RawLoaded = store
                .run(|mut conn| {
                    let versions_key = versions_key.clone();
                    let accounts_key = accounts_key.clone();
                    let invites_key = invites_key.clone();
                    let signups_key = signups_key.clone();
                    async move {
                        redis::pipe()
                            .cmd("HGET")
                            .arg(&versions_key)
                            .arg("accounts")
                            .cmd("HGETALL")
                            .arg(&accounts_key)
                            .cmd("HGETALL")
                            .arg(&invites_key)
                            .cmd("HGETALL")
                            .arg(&signups_key)
                            .query_async(&mut conn)
                            .await
                    }
                })
                .await?;

            let mut accounts = Vec::with_capacity(raw_accounts.len());
            for (_, doc) in raw_accounts {
                accounts.push(from_json::<AccountRecord>(&doc)?);
            }
            accounts.sort_by(|a, b| a.name.cmp(&b.name));

            let mut invites = Vec::with_capacity(raw_invites.len());
            for (_, doc) in raw_invites {
                invites.push(from_json::<InviteRecord>(&doc)?);
            }
            invites.sort_by(|a, b| a.id.cmp(&b.id));

            let mut signups = Vec::with_capacity(raw_signups.len());
            for (_, doc) in raw_signups {
                signups.push(from_json::<SignupRecord>(&doc)?);
            }
            signups.sort_by(|a, b| a.id.cmp(&b.id));

            Ok(AccountsAt {
                version: version.unwrap_or(0),
                accounts,
                invites,
                signups,
            })
        })
    }

    fn version(&self) -> StoreFuture<u64> {
        let store = self.clone();
        Box::pin(async move {
            let versions_key = store.key("versions");
            let ver: Option<u64> = store
                .run(|mut conn| {
                    let versions_key = versions_key.clone();
                    async move {
                        redis::cmd("HGET")
                            .arg(&versions_key)
                            .arg("accounts")
                            .query_async(&mut conn)
                            .await
                    }
                })
                .await?;
            Ok(ver.unwrap_or(0))
        })
    }

    fn apply(&self, seen: u64, ops: Vec<AccountOp>) -> StoreFuture<Option<u64>> {
        let store = self.clone();
        Box::pin(async move {
            let mut encoded_ops: Vec<(String, String, String)> = Vec::with_capacity(ops.len());
            for op in ops {
                match op {
                    AccountOp::PutAccount(record) => {
                        let doc = to_json(&record)?;
                        encoded_ops.push(("put_account".to_string(), record.name, doc));
                    }
                    AccountOp::RemoveAccount(name) => {
                        encoded_ops.push(("rem_account".to_string(), name, String::new()));
                    }
                    AccountOp::PutInvite(invite) => {
                        let doc = to_json(&invite)?;
                        encoded_ops.push(("put_invite".to_string(), invite.id, doc));
                    }
                    AccountOp::RemoveInvite(id) => {
                        encoded_ops.push(("rem_invite".to_string(), id, String::new()));
                    }
                    AccountOp::PutSignup(signup) => {
                        let doc = to_json(&signup)?;
                        encoded_ops.push(("put_signup".to_string(), signup.id, doc));
                    }
                    AccountOp::RemoveSignup(id) => {
                        encoded_ops.push(("rem_signup".to_string(), id, String::new()));
                    }
                }
            }

            let versions_key = store.key("versions");
            let accounts_key = store.key("accounts");
            let invites_key = store.key("invites");
            let signups_key = store.key("signups");
            let script = redis::Script::new(ACCOUNT_APPLY_LUA);
            let res: i64 = store
                .run(|mut conn| {
                    let script = script.clone();
                    let versions_key = versions_key.clone();
                    let accounts_key = accounts_key.clone();
                    let invites_key = invites_key.clone();
                    let signups_key = signups_key.clone();
                    let encoded_ops = encoded_ops.clone();
                    async move {
                        let mut inv = script.key(&versions_key);
                        inv.key(&accounts_key)
                            .key(&invites_key)
                            .key(&signups_key)
                            .arg(seen)
                            .arg(INVITES_SIGNUPS_TTL_SECS);
                        for (kind, k, v) in &encoded_ops {
                            inv.arg(kind).arg(k).arg(v);
                        }
                        inv.invoke_async(&mut conn).await
                    }
                })
                .await?;
            Ok(u64::try_from(res).ok())
        })
    }
}

// ---- logins, setup token and throttle ------------------------------------------

impl LoginStore for RedisStore {
    fn load(&self) -> StoreFuture<HashMap<String, Login>> {
        let store = self.clone();
        Box::pin(async move {
            let logins_key = store.key("logins");
            let pairs: Vec<(String, String)> = store
                .run(|mut conn| {
                    let logins_key = logins_key.clone();
                    async move { redis::cmd("HGETALL").arg(&logins_key).query_async(&mut conn).await }
                })
                .await?;
            let mut logins = HashMap::with_capacity(pairs.len());
            for (key, doc) in pairs {
                let login = from_json::<Login>(&doc)?;
                logins.insert(key, login);
            }
            Ok(logins)
        })
    }

    fn get(&self, key: String) -> StoreFuture<Option<Login>> {
        let store = self.clone();
        Box::pin(async move {
            let logins_key = store.key("logins");
            let doc: Option<String> = store
                .run(|mut conn| {
                    let logins_key = logins_key.clone();
                    let key = key.clone();
                    async move {
                        redis::cmd("HGET")
                            .arg(&logins_key)
                            .arg(&key)
                            .query_async(&mut conn)
                            .await
                    }
                })
                .await?;
            doc.map(|d| from_json::<Login>(&d)).transpose()
        })
    }

    fn save(&self, changes: Vec<(String, LoginChange)>) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            if changes.is_empty() {
                return Ok(());
            }
            let mut args: Vec<(String, String, u64, String)> = Vec::with_capacity(changes.len());
            for (key, change) in changes {
                match change {
                    LoginChange::Opened(login) => {
                        let doc = to_json(&login)?;
                        args.push(("open".to_string(), key, login.last_used, doc));
                    }
                    LoginChange::Used(login) => {
                        args.push(("use".to_string(), key, login.last_used, String::new()));
                    }
                    LoginChange::Closed => {
                        args.push(("close".to_string(), key, 0, String::new()));
                    }
                }
            }
            let logins_key = store.key("logins");
            let versions_key = store.key("versions");
            let script = redis::Script::new(LOGIN_SAVE_LUA);
            let _: i64 = store
                .run(|mut conn| {
                    let script = script.clone();
                    let logins_key = logins_key.clone();
                    let versions_key = versions_key.clone();
                    let args = args.clone();
                    async move {
                        let mut inv = script.key(&logins_key);
                        inv.key(&versions_key).arg(LOGINS_TTL_SECS);
                        for (op, k, ts, doc) in &args {
                            inv.arg(op).arg(k).arg(*ts).arg(doc);
                        }
                        inv.invoke_async(&mut conn).await
                    }
                })
                .await?;
            Ok(())
        })
    }
}

impl SetupStore for RedisStore {
    fn waiting(&self) -> StoreFuture<bool> {
        let store = self.clone();
        Box::pin(async move {
            let setup_key = store.key("setup_token");
            let exists: i64 = store
                .run(|mut conn| {
                    let setup_key = setup_key.clone();
                    async move { redis::cmd("EXISTS").arg(&setup_key).query_async(&mut conn).await }
                })
                .await?;
            Ok(exists > 0)
        })
    }

    fn accepts(&self, token: String) -> StoreFuture<bool> {
        let store = self.clone();
        Box::pin(async move {
            let setup_key = store.key("setup_token");
            let stored: Option<String> = store
                .run(|mut conn| {
                    let setup_key = setup_key.clone();
                    async move { redis::cmd("GET").arg(&setup_key).query_async(&mut conn).await }
                })
                .await?;
            let hash = sha256_hex(&token);
            Ok(stored.is_some_and(|s| crate::auth::same(s.as_bytes(), hash.as_bytes())))
        })
    }

    fn put(&self, token: String) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            let setup_key = store.key("setup_token");
            let hash = sha256_hex(&token);
            store
                .run(|mut conn| {
                    let setup_key = setup_key.clone();
                    let hash = hash.clone();
                    async move {
                        redis::cmd("SET")
                            .arg(&setup_key)
                            .arg(&hash)
                            .arg("EX")
                            .arg(SETUP_TTL_SECS)
                            .query_async::<()>(&mut conn)
                            .await
                    }
                })
                .await
        })
    }

    fn put_if_none(&self, token: String) -> StoreFuture<bool> {
        let store = self.clone();
        Box::pin(async move {
            let setup_key = store.key("setup_token");
            let hash = sha256_hex(&token);
            let res: Option<String> = store
                .run(|mut conn| {
                    let setup_key = setup_key.clone();
                    let hash = hash.clone();
                    async move {
                        redis::cmd("SET")
                            .arg(&setup_key)
                            .arg(&hash)
                            .arg("NX")
                            .arg("EX")
                            .arg(SETUP_TTL_SECS)
                            .query_async(&mut conn)
                            .await
                    }
                })
                .await?;
            Ok(res.is_some())
        })
    }

    fn shown(&self) -> StoreFuture<Option<String>> {
        Box::pin(async { Ok(None) })
    }

    fn take(&self) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            let setup_key = store.key("setup_token");
            store
                .run(|mut conn| {
                    let setup_key = setup_key.clone();
                    async move { redis::cmd("DEL").arg(&setup_key).query_async::<()>(&mut conn).await }
                })
                .await
        })
    }

    fn location(&self) -> String {
        "the database".to_string()
    }
}

#[derive(Serialize, Deserialize)]
struct ThrottleEntry {
    p: String,
    h: String,
    k: String,
    a: i64,
    n: String,
}

impl ThrottleStore for RedisStore {
    fn failed(&self, process: String, key: String, at_ms: i64) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            let entry = ThrottleEntry {
                p: process,
                h: sha256_hex(&key),
                k: key,
                a: at_ms,
                n: Uuid::new_v4().simple().to_string(),
            };
            let doc = to_json(&entry)?;
            let throttle_key = store.key("throttle");
            store
                .run(|mut conn| {
                    let throttle_key = throttle_key.clone();
                    let doc = doc.clone();
                    async move {
                        redis::pipe()
                            .cmd("ZADD")
                            .arg(&throttle_key)
                            .arg(at_ms as f64)
                            .arg(&doc)
                            .ignore()
                            .cmd("EXPIRE")
                            .arg(&throttle_key)
                            .arg(THROTTLE_TTL_SECS)
                            .ignore()
                            .query_async::<()>(&mut conn)
                            .await
                    }
                })
                .await
        })
    }

    fn succeeded(&self, key: String) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            let throttle_key = store.key("throttle");
            let hash = sha256_hex(&key);
            let script = redis::Script::new(THROTTLE_CLEAR_LUA);
            let _: i64 = store
                .run(|mut conn| {
                    let script = script.clone();
                    let throttle_key = throttle_key.clone();
                    let hash = hash.clone();
                    async move { script.key(&throttle_key).arg(&hash).invoke_async(&mut conn).await }
                })
                .await?;
            Ok(())
        })
    }

    fn others(&self, process: String, since_ms: i64) -> StoreFuture<Vec<(String, i64)>> {
        let store = self.clone();
        Box::pin(async move {
            let throttle_key = store.key("throttle");
            let raw: Vec<String> = store
                .run(|mut conn| {
                    let throttle_key = throttle_key.clone();
                    async move {
                        redis::cmd("ZRANGEBYSCORE")
                            .arg(&throttle_key)
                            .arg(since_ms as f64)
                            .arg("+inf")
                            .query_async(&mut conn)
                            .await
                    }
                })
                .await?;
            let mut out = Vec::new();
            for doc in raw {
                if let Ok(entry) = from_json::<ThrottleEntry>(&doc)
                    && entry.p != process
                {
                    out.push((entry.k, entry.a));
                }
            }
            Ok(out)
        })
    }

    fn prune(&self, before_ms: i64) -> StoreFuture<u64> {
        let store = self.clone();
        Box::pin(async move {
            let throttle_key = store.key("throttle");
            let max_score = format!("({before_ms}");
            let removed: u64 = store
                .run(|mut conn| {
                    let throttle_key = throttle_key.clone();
                    let max_score = max_score.clone();
                    async move {
                        redis::cmd("ZREMRANGEBYSCORE")
                            .arg(&throttle_key)
                            .arg("-inf")
                            .arg(&max_score)
                            .query_async(&mut conn)
                            .await
                    }
                })
                .await?;
            Ok(removed)
        })
    }
}

// ---- the server list -----------------------------------------------------------

impl ServerStore for RedisStore {
    fn list(&self) -> StoreFuture<Vec<RedisServer>> {
        let store = self.clone();
        Box::pin(async move { store.load_servers().await })
    }

    fn refresh(&self) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            store.load_servers().await?;
            Ok(())
        })
    }

    fn put(&self, server: RedisServer) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            let _edit = store.server_edit.lock().await;
            let doc = sealed_doc(&server)?;
            let servers_key = store.key("servers");
            let versions_key = store.key("versions");
            let id = server.id.clone();
            store
                .run(|mut conn| {
                    let servers_key = servers_key.clone();
                    let versions_key = versions_key.clone();
                    let id = id.clone();
                    let doc = doc.clone();
                    async move {
                        redis::pipe()
                            .cmd("HSET")
                            .arg(&servers_key)
                            .arg(&id)
                            .arg(&doc)
                            .ignore()
                            .cmd("HINCRBY")
                            .arg(&versions_key)
                            .arg("servers")
                            .arg(1i64)
                            .ignore()
                            .query_async::<()>(&mut conn)
                            .await
                    }
                })
                .await?;
            cache_with(|servers| {
                if let Some(slot) = servers.iter_mut().find(|s| s.id == server.id) {
                    *slot = server;
                } else {
                    servers.push(server);
                }
            })
        })
    }

    fn swap(&self, expected: RedisServer, replacement: Option<RedisServer>) -> StoreFuture<bool> {
        let store = self.clone();
        Box::pin(async move {
            let _edit = store.server_edit.lock().await;
            let servers_key = store.key("servers");
            let versions_key = store.key("versions");
            let id = expected.id.clone();
            let current_doc: Option<String> = store
                .run(|mut conn| {
                    let servers_key = servers_key.clone();
                    let id = id.clone();
                    async move {
                        redis::cmd("HGET")
                            .arg(&servers_key)
                            .arg(&id)
                            .query_async(&mut conn)
                            .await
                    }
                })
                .await?;
            let current = current_doc.as_deref().map(opened).transpose()?;
            if current.as_ref() != Some(&expected) {
                return Ok(false);
            }
            match &replacement {
                Some(next) => {
                    debug_assert_eq!(next.id, expected.id);
                    let doc = sealed_doc(next)?;
                    store
                        .run(|mut conn| {
                            let servers_key = servers_key.clone();
                            let versions_key = versions_key.clone();
                            let id = id.clone();
                            let doc = doc.clone();
                            async move {
                                redis::pipe()
                                    .cmd("HSET")
                                    .arg(&servers_key)
                                    .arg(&id)
                                    .arg(&doc)
                                    .ignore()
                                    .cmd("HINCRBY")
                                    .arg(&versions_key)
                                    .arg("servers")
                                    .arg(1i64)
                                    .ignore()
                                    .query_async::<()>(&mut conn)
                                    .await
                            }
                        })
                        .await?;
                }
                None => {
                    store
                        .run(|mut conn| {
                            let servers_key = servers_key.clone();
                            let versions_key = versions_key.clone();
                            let id = id.clone();
                            async move {
                                redis::pipe()
                                    .cmd("HDEL")
                                    .arg(&servers_key)
                                    .arg(&id)
                                    .ignore()
                                    .cmd("HINCRBY")
                                    .arg(&versions_key)
                                    .arg("servers")
                                    .arg(1i64)
                                    .ignore()
                                    .query_async::<()>(&mut conn)
                                    .await
                            }
                        })
                        .await?;
                }
            }
            cache_with(|servers| match replacement {
                Some(next) => {
                    if let Some(slot) = servers.iter_mut().find(|s| s.id == expected.id) {
                        *slot = next;
                    } else {
                        servers.push(next);
                    }
                }
                None => servers.retain(|s| s.id != expected.id),
            })?;
            Ok(true)
        })
    }
}

impl RedisStore {
    async fn load_servers(&self) -> io::Result<Vec<RedisServer>> {
        let servers_key = self.key("servers");
        let pairs: Vec<(String, String)> = self
            .run(|mut conn| {
                let servers_key = servers_key.clone();
                async move { redis::cmd("HGETALL").arg(&servers_key).query_async(&mut conn).await }
            })
            .await?;
        let mut servers = Vec::with_capacity(pairs.len());
        for (_, doc) in pairs {
            servers.push(opened(&doc)?);
        }
        servers.sort_by(|a, b| a.id.cmp(&b.id));
        set_servers_cache(servers.clone());
        Ok(servers)
    }
}

fn sealed_doc(server: &RedisServer) -> io::Result<String> {
    let mut copy = server.clone();
    seal_server_secrets(&mut copy).map_err(|e| io::Error::other(e.to_string()))?;
    to_json(&copy)
}

fn opened(doc: &str) -> io::Result<RedisServer> {
    let mut server = from_json::<RedisServer>(doc)?;
    open_server_secrets(&mut server);
    Ok(server)
}

fn cache_with(edit: impl FnOnce(&mut Vec<RedisServer>)) -> io::Result<()> {
    let mut servers = get_servers().map_err(|e| io::Error::other(e.to_string()))?;
    edit(&mut servers);
    set_servers_cache(servers);
    Ok(())
}

// ---- the recycle bin -----------------------------------------------------------

impl TrashStore for RedisStore {
    fn put(&self, server_id: String, entry: NewEntry) -> BoxFuture<'static, Result<Meta, PutError>> {
        let store = self.clone();
        Box::pin(async move {
            if entry.payload.len() > MAX_STORED {
                return Err(PutError::TooLarge);
            }
            if !trash::plain(&server_id) {
                return Err(PutError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "not a server id",
                )));
            }
            let size = entry.size;
            let meta = Meta {
                id: Uuid::now_v7().simple().to_string(),
                key: entry.key,
                db: entry.db,
                pttl_ms: entry.pttl_ms,
                deleted_at_ms: trash::now_ms(),
                deleted_by: entry.deleted_by,
                size,
                sealed: entry.sealed,
            };
            let meta_doc = to_json(&meta).map_err(PutError::Io)?;
            let _guard = store.trash_edit.lock().await;
            let trash_key = store.scoped_key("trash", &server_id);
            let payload_key = store.scoped_key("trash_payload", &server_id);
            let servers_key = store.key("trash_servers");

            let existing_raw: Vec<(String, String)> = store
                .run(|mut conn| {
                    let trash_key = trash_key.clone();
                    async move { redis::cmd("HGETALL").arg(&trash_key).query_async(&mut conn).await }
                })
                .await
                .map_err(PutError::Io)?;

            let cutoff = bin_cutoff();
            let mut live = Vec::new();
            let mut to_remove = Vec::new();
            for (id, doc) in existing_raw {
                match from_json::<Meta>(&doc) {
                    Ok(m) if m.deleted_at_ms > cutoff => live.push(m),
                    _ => to_remove.push(id),
                }
            }
            live.sort_by(|a, b| b.deleted_at_ms.cmp(&a.deleted_at_ms).then_with(|| b.id.cmp(&a.id)));

            let mut kept: Vec<(String, u64)> = live.into_iter().map(|m| (m.id, m.size)).collect();
            let mut bytes: u64 = kept.iter().map(|(_, s)| *s).sum();
            while !kept.is_empty() && (kept.len() + 1 > MAX_ENTRIES || bytes + size > MAX_BYTES) {
                if let Some((oldest_id, oldest_size)) = kept.pop() {
                    bytes = bytes.saturating_sub(oldest_size);
                    to_remove.push(oldest_id);
                }
            }

            let id = meta.id.clone();
            let payload = entry.payload;
            store
                .run(|mut conn| {
                    let trash_key = trash_key.clone();
                    let payload_key = payload_key.clone();
                    let servers_key = servers_key.clone();
                    let server_id = server_id.clone();
                    let to_remove = to_remove.clone();
                    let id = id.clone();
                    let meta_doc = meta_doc.clone();
                    let payload = payload.clone();
                    async move {
                        let mut pipe = redis::pipe();
                        pipe.cmd("SADD")
                            .arg(&servers_key)
                            .arg(&server_id)
                            .ignore()
                            .cmd("EXPIRE")
                            .arg(&servers_key)
                            .arg(TRASH_TTL_SECS)
                            .ignore();
                        for old_id in &to_remove {
                            pipe.cmd("HDEL")
                                .arg(&trash_key)
                                .arg(old_id)
                                .ignore()
                                .cmd("HDEL")
                                .arg(&payload_key)
                                .arg(old_id)
                                .ignore();
                        }
                        pipe.cmd("HSET")
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
                            .ignore();
                        pipe.query_async::<()>(&mut conn).await
                    }
                })
                .await
                .map_err(PutError::Io)?;
            Ok(meta)
        })
    }

    fn list(&self, server_id: String) -> StoreFuture<Vec<Meta>> {
        let store = self.clone();
        Box::pin(async move {
            let trash_key = store.scoped_key("trash", &server_id);
            let pairs: Vec<(String, String)> = store
                .run(|mut conn| {
                    let trash_key = trash_key.clone();
                    async move { redis::cmd("HGETALL").arg(&trash_key).query_async(&mut conn).await }
                })
                .await?;
            let cutoff = bin_cutoff();
            let mut entries = Vec::new();
            for (_, doc) in pairs {
                let meta = from_json::<Meta>(&doc)?;
                if meta.deleted_at_ms > cutoff {
                    entries.push(meta);
                }
            }
            entries.sort_by(|a, b| b.deleted_at_ms.cmp(&a.deleted_at_ms).then_with(|| b.id.cmp(&a.id)));
            Ok(entries)
        })
    }

    fn get(&self, server_id: String, id: String) -> StoreFuture<Option<(Meta, Vec<u8>)>> {
        let store = self.clone();
        Box::pin(async move {
            if !trash::plain(&id) {
                return Ok(None);
            }
            let trash_key = store.scoped_key("trash", &server_id);
            let payload_key = store.scoped_key("trash_payload", &server_id);
            let (meta_doc, payload): (Option<String>, Option<Vec<u8>>) = store
                .run(|mut conn| {
                    let trash_key = trash_key.clone();
                    let payload_key = payload_key.clone();
                    let id = id.clone();
                    async move {
                        redis::pipe()
                            .cmd("HGET")
                            .arg(&trash_key)
                            .arg(&id)
                            .cmd("HGET")
                            .arg(&payload_key)
                            .arg(&id)
                            .query_async(&mut conn)
                            .await
                    }
                })
                .await?;
            let (Some(doc), Some(bytes)) = (meta_doc, payload) else {
                return Ok(None);
            };
            let meta = from_json::<Meta>(&doc)?;
            if meta.deleted_at_ms <= bin_cutoff() {
                return Ok(None);
            }
            Ok(Some((meta, bytes)))
        })
    }

    fn remove(&self, server_id: String, id: String) -> StoreFuture<Option<Meta>> {
        let store = self.clone();
        Box::pin(async move {
            if !trash::plain(&id) {
                return Ok(None);
            }
            let trash_key = store.scoped_key("trash", &server_id);
            let payload_key = store.scoped_key("trash_payload", &server_id);
            let doc: Option<String> = store
                .run(|mut conn| {
                    let trash_key = trash_key.clone();
                    let payload_key = payload_key.clone();
                    let id = id.clone();
                    async move {
                        let (doc, _, _): (Option<String>, i64, i64) = redis::pipe()
                            .cmd("HGET")
                            .arg(&trash_key)
                            .arg(&id)
                            .cmd("HDEL")
                            .arg(&trash_key)
                            .arg(&id)
                            .cmd("HDEL")
                            .arg(&payload_key)
                            .arg(&id)
                            .query_async(&mut conn)
                            .await?;
                        Ok(doc)
                    }
                })
                .await?;
            doc.map(|d| from_json::<Meta>(&d)).transpose()
        })
    }

    fn remove_server(&self, server_id: String) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            let trash_key = store.scoped_key("trash", &server_id);
            let payload_key = store.scoped_key("trash_payload", &server_id);
            let servers_key = store.key("trash_servers");
            store
                .run(|mut conn| {
                    let trash_key = trash_key.clone();
                    let payload_key = payload_key.clone();
                    let servers_key = servers_key.clone();
                    let server_id = server_id.clone();
                    async move {
                        redis::pipe()
                            .cmd("DEL")
                            .arg(&trash_key)
                            .ignore()
                            .cmd("DEL")
                            .arg(&payload_key)
                            .ignore()
                            .cmd("SREM")
                            .arg(&servers_key)
                            .arg(&server_id)
                            .ignore()
                            .query_async::<()>(&mut conn)
                            .await
                    }
                })
                .await
        })
    }

    fn purge_expired(&self) -> StoreFuture<usize> {
        let store = self.clone();
        Box::pin(async move {
            let servers_key = store.key("trash_servers");
            let servers: Vec<String> = store
                .run(|mut conn| {
                    let servers_key = servers_key.clone();
                    async move { redis::cmd("SMEMBERS").arg(&servers_key).query_async(&mut conn).await }
                })
                .await?;
            let cutoff = bin_cutoff();
            let mut purged = 0usize;
            for server_id in servers {
                let trash_key = store.scoped_key("trash", &server_id);
                let payload_key = store.scoped_key("trash_payload", &server_id);
                let pairs: Vec<(String, String)> = store
                    .run(|mut conn| {
                        let trash_key = trash_key.clone();
                        async move { redis::cmd("HGETALL").arg(&trash_key).query_async(&mut conn).await }
                    })
                    .await?;
                let total = pairs.len();
                let mut expired_ids = Vec::new();
                for (id, doc) in pairs {
                    if !from_json::<Meta>(&doc).is_ok_and(|m| m.deleted_at_ms > cutoff) {
                        expired_ids.push(id);
                    }
                }
                if !expired_ids.is_empty() {
                    let removed_all = expired_ids.len() == total;
                    purged += expired_ids.len();
                    store
                        .run(|mut conn| {
                            let trash_key = trash_key.clone();
                            let payload_key = payload_key.clone();
                            let servers_key = servers_key.clone();
                            let server_id = server_id.clone();
                            let expired_ids = expired_ids.clone();
                            async move {
                                let mut pipe = redis::pipe();
                                for id in &expired_ids {
                                    pipe.cmd("HDEL")
                                        .arg(&trash_key)
                                        .arg(id)
                                        .ignore()
                                        .cmd("HDEL")
                                        .arg(&payload_key)
                                        .arg(id)
                                        .ignore();
                                }
                                if removed_all {
                                    pipe.cmd("SREM").arg(&servers_key).arg(&server_id).ignore();
                                }
                                pipe.query_async::<()>(&mut conn).await
                            }
                        })
                        .await?;
                } else if total == 0 {
                    store
                        .run(|mut conn| {
                            let servers_key = servers_key.clone();
                            let server_id = server_id.clone();
                            async move {
                                redis::cmd("SREM")
                                    .arg(&servers_key)
                                    .arg(&server_id)
                                    .query_async::<()>(&mut conn)
                                    .await
                            }
                        })
                        .await?;
                }
            }
            Ok(purged)
        })
    }
}

fn bin_cutoff() -> i64 {
    trash::now_ms() - i64::try_from(RETENTION.as_millis()).unwrap_or(i64::MAX)
}

// ---- metrics history -----------------------------------------------------------

impl MetricsStore for RedisStore {
    fn put(
        &self,
        server_id: String,
        ts_ms: i64,
        sample_json: String,
    ) -> BoxFuture<'static, Result<bool, metrics::PutError>> {
        let store = self.clone();
        Box::pin(async move {
            let raw = metrics::checked(&server_id, ts_ms, &sample_json, metrics::now_ms())?;
            let minute = ts_ms.div_euclid(metrics::INTERVAL_MS);
            let member = format!("{minute}:{ts_ms}\n{}", raw.get());
            let metrics_key = store.scoped_key("metrics", &server_id);
            let servers_key = store.key("metric_servers");
            let script = redis::Script::new(METRIC_ADD_LUA);
            let added: i64 = store
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
                            .arg(minute)
                            .arg(ts_ms)
                            .arg(&member)
                            .arg(METRICS_TTL_SECS)
                            .invoke_async(&mut conn)
                            .await
                    }
                })
                .await
                .map_err(|e| metrics::PutError::Io(e.to_string()))?;
            Ok(added > 0)
        })
    }

    fn since(&self, server_id: String, from_ms: i64, max_points: usize) -> StoreFuture<Vec<Box<RawValue>>> {
        let store = self.clone();
        Box::pin(async move {
            if !metrics::plain(&server_id) {
                return Ok(Vec::new());
            }
            let from_ms = from_ms.max(metrics::now_ms() - RETENTION_DAYS * DAY_MS);
            let metrics_key = store.scoped_key("metrics", &server_id);
            let members: Vec<String> = store
                .run(|mut conn| {
                    let metrics_key = metrics_key.clone();
                    async move {
                        redis::cmd("ZRANGEBYSCORE")
                            .arg(&metrics_key)
                            .arg(from_ms as f64)
                            .arg("+inf")
                            .query_async(&mut conn)
                            .await
                    }
                })
                .await?;
            let mut samples = Vec::with_capacity(members.len());
            for member in members {
                let raw_json = member.split_once('\n').map_or(member.as_str(), |(_, json)| json);
                samples.push(RawValue::from_string(raw_json.to_string()).map_err(io::Error::other)?);
            }
            Ok(metrics::thin(samples, max_points.clamp(2, metrics::MAX_POINTS)))
        })
    }

    fn remove_server(&self, server_id: String) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            let metrics_key = store.scoped_key("metrics", &server_id);
            let servers_key = store.key("metric_servers");
            store
                .run(|mut conn| {
                    let metrics_key = metrics_key.clone();
                    let servers_key = servers_key.clone();
                    let server_id = server_id.clone();
                    async move {
                        redis::pipe()
                            .cmd("DEL")
                            .arg(&metrics_key)
                            .ignore()
                            .cmd("SREM")
                            .arg(&servers_key)
                            .arg(&server_id)
                            .ignore()
                            .query_async::<()>(&mut conn)
                            .await
                    }
                })
                .await
        })
    }

    fn prune(&self) -> StoreFuture<usize> {
        let store = self.clone();
        Box::pin(async move {
            let servers_key = store.key("metric_servers");
            let servers: Vec<String> = store
                .run(|mut conn| {
                    let servers_key = servers_key.clone();
                    async move { redis::cmd("SMEMBERS").arg(&servers_key).query_async(&mut conn).await }
                })
                .await?;
            let oldest_day = metrics::now_ms().div_euclid(DAY_MS) - RETENTION_DAYS;
            let max_score = format!("({}", oldest_day * DAY_MS);
            let mut pruned = 0usize;
            for server_id in servers {
                let metrics_key = store.scoped_key("metrics", &server_id);
                let removed: usize = store
                    .run(|mut conn| {
                        let metrics_key = metrics_key.clone();
                        let max_score = max_score.clone();
                        async move {
                            redis::cmd("ZREMRANGEBYSCORE")
                                .arg(&metrics_key)
                                .arg("-inf")
                                .arg(&max_score)
                                .query_async(&mut conn)
                                .await
                        }
                    })
                    .await?;
                pruned += removed;
            }
            Ok(pruned)
        })
    }
}

// ---- page rows, key notes and scripts ------------------------------------------

impl PageRowStore for RedisStore {
    fn rows(&self, account: String) -> StoreFuture<AccountRows> {
        let store = self.clone();
        Box::pin(async move {
            let key = store.scoped_key("page_rows", &account);
            let pairs: Vec<(String, String)> = store
                .run(|mut conn| {
                    let key = key.clone();
                    async move { redis::cmd("HGETALL").arg(&key).query_async(&mut conn).await }
                })
                .await?;
            let mut out = AccountRows::new();
            for (field, value) in pairs {
                if let Some((table, row_key)) = field.split_once('\0') {
                    out.entry(table.to_string())
                        .or_default()
                        .insert(row_key.to_string(), value);
                }
            }
            Ok(out)
        })
    }

    fn put(
        &self,
        account: String,
        table: String,
        key: String,
        value: Option<String>,
    ) -> BoxFuture<'static, Result<(), RowError>> {
        let store = self.clone();
        Box::pin(async move {
            let rows_key = store.scoped_key("page_rows", &account);
            let field = format!("{table}\0{key}");
            match value {
                Some(val) => {
                    if key.len().saturating_add(val.len()) > MAX_ACCOUNT_BYTES {
                        return Err(RowError::TooLarge);
                    }
                    let script = redis::Script::new(PAGE_ROW_PUT_LUA);
                    let ok: i64 = store
                        .run(|mut conn| {
                            let script = script.clone();
                            let rows_key = rows_key.clone();
                            let field = field.clone();
                            let val = val.clone();
                            async move {
                                script
                                    .key(&rows_key)
                                    .arg(&field)
                                    .arg(&val)
                                    .arg(MAX_ACCOUNT_BYTES)
                                    .invoke_async(&mut conn)
                                    .await
                            }
                        })
                        .await
                        .map_err(RowError::Store)?;
                    if ok == 0 {
                        return Err(RowError::TooLarge);
                    }
                    Ok(())
                }
                None => store
                    .run(|mut conn| {
                        let rows_key = rows_key.clone();
                        let field = field.clone();
                        async move {
                            redis::cmd("HDEL")
                                .arg(&rows_key)
                                .arg(&field)
                                .query_async::<()>(&mut conn)
                                .await
                        }
                    })
                    .await
                    .map_err(RowError::Store),
            }
        })
    }

    fn forget(&self, account: String) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            let rows_key = store.scoped_key("page_rows", &account);
            store
                .run(|mut conn| {
                    let rows_key = rows_key.clone();
                    async move { redis::cmd("DEL").arg(&rows_key).query_async::<()>(&mut conn).await }
                })
                .await
        })
    }
}

impl KeyNoteStore for RedisStore {
    fn list(&self, server_id: String) -> StoreFuture<BTreeMap<String, KeyNote>> {
        let store = self.clone();
        Box::pin(async move {
            let notes_key = store.scoped_key("key_notes", &server_id);
            let pairs: Vec<(String, String)> = store
                .run(|mut conn| {
                    let notes_key = notes_key.clone();
                    async move { redis::cmd("HGETALL").arg(&notes_key).query_async(&mut conn).await }
                })
                .await?;
            let mut notes = BTreeMap::new();
            for (k, doc) in pairs {
                notes.insert(k, from_json::<KeyNote>(&doc)?);
            }
            Ok(notes)
        })
    }

    fn put(&self, server_id: String, key: String, note: Option<KeyNote>) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            let notes_key = store.scoped_key("key_notes", &server_id);
            match note {
                Some(note) => {
                    let doc = to_json(&note)?;
                    store
                        .run(|mut conn| {
                            let notes_key = notes_key.clone();
                            let key = key.clone();
                            let doc = doc.clone();
                            async move {
                                redis::cmd("HSET")
                                    .arg(&notes_key)
                                    .arg(&key)
                                    .arg(&doc)
                                    .query_async::<()>(&mut conn)
                                    .await
                            }
                        })
                        .await
                }
                None => {
                    store
                        .run(|mut conn| {
                            let notes_key = notes_key.clone();
                            let key = key.clone();
                            async move {
                                redis::cmd("HDEL")
                                    .arg(&notes_key)
                                    .arg(&key)
                                    .query_async::<()>(&mut conn)
                                    .await
                            }
                        })
                        .await
                }
            }
        })
    }

    fn remove_server(&self, server_id: String) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            let notes_key = store.scoped_key("key_notes", &server_id);
            store
                .run(|mut conn| {
                    let notes_key = notes_key.clone();
                    async move { redis::cmd("DEL").arg(&notes_key).query_async::<()>(&mut conn).await }
                })
                .await
        })
    }
}

impl ScriptStore for RedisStore {
    fn list(&self) -> StoreFuture<Vec<ScriptRecord>> {
        let store = self.clone();
        Box::pin(async move {
            let scripts_key = store.key("scripts");
            let pairs: Vec<(String, String)> = store
                .run(|mut conn| {
                    let scripts_key = scripts_key.clone();
                    async move { redis::cmd("HGETALL").arg(&scripts_key).query_async(&mut conn).await }
                })
                .await?;
            let mut scripts = Vec::with_capacity(pairs.len());
            for (_, doc) in pairs {
                scripts.push(from_json::<ScriptRecord>(&doc)?);
            }
            scripts.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(scripts)
        })
    }

    fn put(&self, script: ScriptRecord) -> StoreFuture<()> {
        let store = self.clone();
        Box::pin(async move {
            let doc = to_json(&script)?;
            let scripts_key = store.key("scripts");
            let id = script.id;
            store
                .run(|mut conn| {
                    let scripts_key = scripts_key.clone();
                    let id = id.clone();
                    let doc = doc.clone();
                    async move {
                        redis::cmd("HSET")
                            .arg(&scripts_key)
                            .arg(&id)
                            .arg(&doc)
                            .query_async::<()>(&mut conn)
                            .await
                    }
                })
                .await
        })
    }

    fn remove(&self, id: String) -> StoreFuture<Option<ScriptRecord>> {
        let store = self.clone();
        Box::pin(async move {
            let scripts_key = store.key("scripts");
            let doc: Option<String> = store
                .run(|mut conn| {
                    let scripts_key = scripts_key.clone();
                    let id = id.clone();
                    async move {
                        let (doc, _): (Option<String>, i64) = redis::pipe()
                            .cmd("HGET")
                            .arg(&scripts_key)
                            .arg(&id)
                            .cmd("HDEL")
                            .arg(&scripts_key)
                            .arg(&id)
                            .query_async(&mut conn)
                            .await?;
                        Ok(doc)
                    }
                })
                .await?;
            doc.map(|d| from_json::<ScriptRecord>(&d)).transpose()
        })
    }
}

// ---- audit log in Redis --------------------------------------------------------

const QUEUE_LIMIT: usize = 4_096;
const BATCH_LIMIT: usize = 64;
const FLUSH_WINDOW: Duration = Duration::from_millis(50);

#[derive(Serialize, Deserialize)]
struct StoredAudit {
    id: u64,
    at_ms: i64,
    account: String,
    event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    server: Option<String>,
    line: String,
}

pub(crate) struct RedisAudit {
    store: RedisStore,
    tx: mpsc::Sender<AuditEntry>,
}

impl RedisAudit {
    pub(crate) fn start(store: RedisStore) -> Self {
        let (tx, rx) = mpsc::channel(QUEUE_LIMIT);
        tokio::spawn(run_audit_writer(store.clone(), rx));
        Self { store, tx }
    }
}

impl AuditLog for RedisAudit {
    fn append(&self, entry: AuditEntry) {
        if self.tx.try_send(entry).is_err() {
            tracing::warn!("the Redis audit queue is full; dropping an audit line");
        }
    }

    fn query(&self, q: AuditQuery) -> StoreFuture<AuditPage> {
        let store = self.store.clone();
        Box::pin(async move {
            let limit = q.limit();
            let before_id: Option<u64> = q.before.as_deref().and_then(|s| s.parse().ok());
            let max_score = match before_id {
                Some(before) => format!("({before}"),
                None => "+inf".to_string(),
            };
            let audit_key = store.key("audit");
            let mut matched: Vec<(u64, Box<RawValue>)> = Vec::with_capacity(limit);
            let mut offset = 0isize;
            const CHUNK: isize = 250;

            loop {
                let raw: Vec<String> = store
                    .run(|mut conn| {
                        let audit_key = audit_key.clone();
                        let max_score = max_score.clone();
                        async move {
                            redis::cmd("ZREVRANGEBYSCORE")
                                .arg(&audit_key)
                                .arg(&max_score)
                                .arg("-inf")
                                .arg("LIMIT")
                                .arg(offset)
                                .arg(CHUNK)
                                .query_async(&mut conn)
                                .await
                        }
                    })
                    .await?;
                let fetched = raw.len();
                for doc in raw {
                    let entry = from_json::<StoredAudit>(&doc)?;
                    if q.since_ms.is_some_and(|since| entry.at_ms < since) {
                        continue;
                    }
                    if q.until_ms.is_some_and(|until| entry.at_ms > until) {
                        continue;
                    }
                    if q.account.as_ref().is_some_and(|acc| &entry.account != acc) {
                        continue;
                    }
                    if q.event.as_ref().is_some_and(|ev| &entry.event != ev) {
                        continue;
                    }
                    if q.server.as_ref().is_some_and(|srv| entry.server.as_ref() != Some(srv)) {
                        continue;
                    }
                    let raw_line = RawValue::from_string(entry.line).map_err(io::Error::other)?;
                    matched.push((entry.id, raw_line));
                    if matched.len() == limit {
                        break;
                    }
                }
                if matched.len() == limit || (fetched as isize) < CHUNK {
                    break;
                }
                offset += CHUNK;
            }

            let mut page = AuditPage::default();
            if matched.len() == limit {
                page.next = matched.last().map(|(id, _)| id.to_string());
            }
            page.lines = matched.into_iter().map(|(_, line)| line).collect();
            Ok(page)
        })
    }

    fn prune(&self, before_ms: i64) -> StoreFuture<u64> {
        let store = self.store.clone();
        Box::pin(async move {
            let audit_key = store.key("audit");
            let script = redis::Script::new(AUDIT_PRUNE_LUA);
            let removed: u64 = store
                .run(|mut conn| {
                    let script = script.clone();
                    let audit_key = audit_key.clone();
                    async move { script.key(&audit_key).arg(before_ms).invoke_async(&mut conn).await }
                })
                .await?;
            Ok(removed)
        })
    }
}

async fn run_audit_writer(store: RedisStore, mut rx: mpsc::Receiver<AuditEntry>) {
    let mut batch = Vec::with_capacity(BATCH_LIMIT);
    while let Some(first) = rx.recv().await {
        batch.push(first);
        let deadline = tokio::time::Instant::now() + FLUSH_WINDOW;
        while batch.len() < BATCH_LIMIT {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(next)) => batch.push(next),
                Ok(None) | Err(_) => break,
            }
        }
        if let Err(e) = store.write_audit_batch(std::mem::take(&mut batch)).await {
            tracing::warn!(error = %e, "the audit lines could not be written to Redis");
        }
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
    use std::sync::Once;
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
