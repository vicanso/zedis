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

use super::*;
use crate::metrics::{DAY_MS, RETENTION_DAYS};
use crate::page_data::{AccountRows, KeyNote, MAX_ACCOUNT_BYTES, RowError, ScriptRecord};
use crate::store::{
    AuditEntry, AuditLog, AuditPage, AuditQuery, KeyNoteStore, MetricsStore, PageRowStore, ScriptStore, ServerStore,
    StoreFuture, TrashStore,
};
use crate::trash::{self, MAX_BYTES, MAX_ENTRIES, MAX_STORED, NewEntry, PutError, RETENTION};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::collections::BTreeMap;
use std::io;
use std::time::Duration;
use tokio::sync::mpsc;
use uuid::Uuid;
use zedis_connection::{RedisServer, get_servers, open_server_secrets, seal_server_secrets, set_servers_cache};

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

impl RedisStore {
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
