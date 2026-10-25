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

//! Live integration tests against real Redis-compatible servers.
//!
//! Every test is `#[ignore]`: the unit suite (`make test`) never needs a
//! server. Start the topology with `make it-up` (local `redis-server`, or
//! `REDIS_IMAGE=redis:7.2 make it-up` for docker), then `make it`. Each
//! scenario reads its own `ZEDIS_IT_*` variable and skips — loudly — when
//! it is unset, so a partial topology (standalone only) still runs.
//!
//! CI runs the full matrix in `.github/workflows/integration.yml`.

use redis::{FromRedisValue, cmd};
use regex::Regex;
use semver::Version;
use std::collections::{HashMap, HashSet};
use std::env;
use std::sync::Once;
use std::sync::atomic::AtomicBool;
use zedis_connection::error::ConnectionErrorKind;
use zedis_connection::floors::{self, Floor};
use zedis_connection::{
    AclDryRun, BitOpKind, ChannelSubscription, ClusterNode, CommandLogKind, CommandStatus, CompareOptions, CompareSide,
    ConflictMode, ExpireCondition, FAILOVER_TIMEOUT_MS, FieldTtl, FromEnd, GeoShape, HeatMetric, HeatProbe,
    HllEncoding, ImportFormat, KeyDifference, KeyOp, KeyOpOutcome, KillFilter, KillOutcome, KillTarget, PauseMode,
    ProbKind, ProbeOutcome, PubsubChannel, REPORT_CONFIG, ReadLimits, ReadableValue, ReadableWriteStatus,
    RedisAsyncConn, RedisServer, ReplicationInfo, ReplicationRole, ReplyFormat, RestoreStatus, SERVER_TYPE_SENTINEL,
    SearchOptions, ServerCommand, ServerDb, ServerFlavor, SlotStatMetric, StreamGroup, StreamTail, StreamTrim,
    StringWrite, SubscribeKind, TerminalSession, TsAlter, TsMRange, VectorSimOptions, acl_del_user, acl_dryrun,
    acl_file, acl_genpass, acl_get_user, acl_log, acl_log_reset, acl_save, acl_set_user, acl_whoami, bgrewriteaof,
    bgsave, bit_field, bit_op, bitmap_info, client_kill_by, client_kill_id, client_list, client_pause, client_unpause,
    cluster_get_slot_migrations, cluster_migrate_slots, command_log_reset, command_logs, compare_prefix,
    config_get_all, config_get_named, config_get_one, config_load, config_resetstat, config_rewrite, config_set,
    consumer_create, consumer_delete, count_keys_matching, create_key, csv_header, dbsize, delete_key, delete_keys,
    delete_keys_matching, dump_key, dump_keys_chunk, entry_to_csv, entry_to_json, expire_key, expire_key_at,
    forget_client, ft_explain, ft_info, ft_search, ft_spellcheck, ft_tagvals, geo_add, geo_dist, geo_sample,
    geo_search, get_connection_manager, get_server, get_server_heat_probe, get_servers, group_create, group_destroy,
    group_set_id, hash_delete_fields, hash_field_ttls, hash_len, hash_scan, heartbeat_probe, hll_info, info_everything,
    key_bytes, key_memory_usage, key_object_meta, key_type_and_ttl, key_types, kill_filter_commands, kill_running,
    latency_history, latency_latest, latency_monitor_threshold, latency_reset, list_len, list_push, list_range,
    list_set_if_unchanged, master_addrs, master_infos, maxmemory_policy, node_add_slots, node_cancel_slot_migrations,
    node_failover, node_load, node_replicate, node_slot_migrations, node_stabilize_slot, open_monitor_feeds,
    open_single_connection, parse_readable_entries, pending_page, persist_key, pf_add, pf_merge,
    plan_cluster_rebalance, preview_key_conflicts, prob_info, prob_probe, probe_server_features, read_readable_chunk,
    remove_list_indexes, rename_hash_field, rename_key, restore_key, restore_keys_chunk, run_key_op, run_script,
    save_servers, scan_page, script_exists, script_load, script_sha1, sentinel_ckquorum, sentinel_flushconfig,
    sentinel_master_names, sentinel_masters, sentinel_monitor, sentinel_remove, sentinel_set, server_summary,
    server_supports, set_add, set_bit, set_card, set_hash_field_ttl, set_keys_ttl, set_remove, set_replace_member,
    set_scan, set_ttl_matching, slow_logs, snapshot_key, sniff_import_format, split_acl_rules, stream_ack, stream_add,
    stream_autoclaim, stream_claim, stream_delete, stream_info, stream_len, stream_page, stream_set_id, stream_trim,
    string_get, string_prefix, string_set, test_connection, troubleshooting_snapshot, ts_add, ts_alter, ts_create_rule,
    ts_delete_rule, ts_mrange, ts_window, unassigned_slot_ranges, value_preview, vset_info, vset_remove, vset_set_attr,
    vset_sim, write_hash_field, write_readable_chunk, zset_card, zset_count_by_score, zset_looks_geo, zset_put,
    zset_range, zset_range_by_score, zset_remove, zset_scan,
};
use zedis_connection::{
    AggregateOptions, CreateFieldSpec, CreateIndexOptions, FunctionRestorePolicy, ReducerFn, ReducerSpec, acl_list,
    acl_load, cluster_forget, cluster_meet, failover_abort, flush_all, flush_db, ft_aggregate, ft_alter_add, ft_create,
    ft_dropindex, ft_profile, function_dump, function_fcall, function_flush, function_list, function_restore, json_get,
    json_merge, json_set, key_size_distributions, memory_stats, node_migrate_slots, publish, sample_memory_usage,
    script_flush, stream_ack_delete, stream_nack,
};
use zedis_connection::{
    CopyLoss, SearchableType, ValueSearchSpec, copy_preflight, multi_search_exact, multi_search_scan, scan_values_round,
};
use zedis_connection::{
    MonitorFeed, SlotMigrationDialect, SlotMove, bgsave_cancel, client_name, command_stats, copy_key,
    copy_key_logically, function_delete, function_load, function_stats, is_foreign_payload, is_zedis_client_name,
    migrate_slot, restore_or_recreate_chunk, slowlog_settings, zedis_client_addrs,
};
use zedis_core::json::JsonPathOp;
use zedis_core::keysizes::KeysizesUnit;
use zedis_core::search_params::{ParamKind, encode_param};

/// `host:port` from an env var, or `None` when that scenario wasn't started.
fn scenario(var: &str) -> Option<(String, u16)> {
    let value = env::var(var).ok()?;
    let (host, port) = value.rsplit_once(':')?;
    Some((host.to_string(), port.parse().ok()?))
}

/// Standalone is the one scenario every run has; a missing variable is a
/// harness mistake, not a skip.
fn standalone() -> (String, u16) {
    scenario("ZEDIS_IT_STANDALONE").expect("ZEDIS_IT_STANDALONE=host:port — run scripts/it/up.sh and `make it`")
}

macro_rules! skip_unless {
    ($var:literal) => {
        match scenario($var) {
            Some(addr) => addr,
            None => {
                eprintln!("skipped: {} not set", $var);
                return;
            }
        }
    };
}

/// One isolated config dir per test process: `save_servers` writes the
/// server list (and the encryption key file) there, never to the real one.
fn isolate() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let dir = env::temp_dir().join(format!("zedis-it-{}", std::process::id()));
        zedis_core::fs::override_config_dir(dir);
    });
}

/// `save_servers` replaces the whole list and tests run in parallel —
/// serialise the read-append-write (an async lock: it is held across the
/// save's await).
static REGISTER: smol::lock::Mutex<()> = smol::lock::Mutex::new(());

/// Held by every test that changes cluster slot state. They share one
/// cluster, and the states are mutually exclusive on the server: a node
/// that is importing a slot atomically refuses `SETSLOT … IMPORTING`
/// with "Slot import in progress".
static CLUSTER_SLOTS: smol::lock::Mutex<()> = smol::lock::Mutex::new(());

/// Held by every test on the `busy` scenario's server: the runaway-script
/// test parks it in BUSY and the server-wide flushes empty it, and either
/// would fail the other.
static BUSY_SERVER: smol::lock::Mutex<()> = smol::lock::Mutex::new(());

async fn register(server: RedisServer) -> String {
    isolate();
    let _guard = REGISTER.lock().await;
    let id = server.id.clone();
    let mut servers = get_servers().unwrap_or_default();
    servers.retain(|s| s.id != id);
    servers.push(server);
    save_servers(servers).await.expect("save servers");
    id
}

fn server(id: &str, (host, port): (String, u16)) -> RedisServer {
    RedisServer {
        id: id.to_string(),
        name: id.to_string(),
        host,
        port,
        ..Default::default()
    }
}

/// The password the harness put on the sentinel's data nodes and on every
/// cluster node. Absent only when those scenarios were not started, in
/// which case nothing that needs it runs either.
fn data_password() -> Option<String> {
    env::var("ZEDIS_IT_PASSWORD").ok().filter(|p| !p.is_empty())
}

/// An entry for a data node behind the harness's password: every cluster
/// node (including on a MOVED redirect) and the masters and replicas the
/// sentinel watches.
fn protected_server(id: &str, addr: (String, u16)) -> RedisServer {
    RedisServer {
        password: data_password(),
        ..server(id, addr)
    }
}

/// A Sentinel entry: the data nodes' password *and* the sentinel's own,
/// which the harness deliberately makes different. `password` reaches the
/// master Sentinel points at, `sentinel_password` the sentinels themselves
/// — the split `sentinel_login()` exists for.
///
/// `server_type` is left on auto, which is what the connection dialog
/// produces by default, so discovery reaches the sentinel through
/// `open_seed_endpoint`'s retry rather than its declared-Sentinel
/// shortcut. `sentinel_declared_server` covers the other branch.
fn sentinel_server(id: &str, addr: (String, u16)) -> RedisServer {
    RedisServer {
        password: data_password(),
        sentinel_password: env::var("ZEDIS_IT_SENTINEL_PASSWORD").ok().filter(|p| !p.is_empty()),
        ..server(id, addr)
    }
}

/// The same entry with the type declared, which takes the sentinel's
/// credentials straight to the seed instead of discovering them from a
/// refused AUTH.
fn sentinel_declared_server(id: &str, addr: (String, u16)) -> RedisServer {
    RedisServer {
        server_type: Some(SERVER_TYPE_SENTINEL),
        ..sentinel_server(id, addr)
    }
}

async fn conn(id: &str, db: usize) -> RedisAsyncConn {
    get_connection_manager()
        .get_connection(id, db)
        .await
        .expect("connection")
}

/// Unique per process and call, so parallel tests (and re-runs against a
/// server that wasn't flushed) never share a key or ACL user name.
fn unique(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!(
        "zedis:it:{prefix}:{}:{nanos}:{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// Version gate for a test: the same [`Floor`] the app uses, so a test
/// never carries its own (flavor-blind) version string.
/// Whether the server knows `name` — `COMMAND INFO` answers a nil entry for
/// a command it does not have. The module tests ask this per family, because
/// the stack image and valkey-bundle ship different sets: valkey-bloom is
/// `BF.*` alone, valkey-search has no `TAGVALS`, and neither has TimeSeries.
async fn has_command(c: &mut RedisAsyncConn, name: &str) -> bool {
    let reply: redis::RedisResult<Vec<redis::Value>> = cmd("COMMAND").arg("INFO").arg(name).query_async(c).await;
    reply.is_ok_and(|entries| entries.iter().any(|entry| !matches!(entry, redis::Value::Nil)))
}

/// Drop `index` and the documents under `prefix`. `FT.DROPINDEX … DD` would
/// do both on RediSearch; valkey-search has no `DD`, so the documents go by
/// hand — a test's keyspace is small enough for `KEYS`.
async fn drop_index_and_docs(c: &mut RedisAsyncConn, index: &str, prefix: &str) {
    cmd("FT.DROPINDEX")
        .arg(index)
        .exec_async(c)
        .await
        .expect("ft.dropindex");
    let keys: Vec<String> = cmd("KEYS")
        .arg(format!("{prefix}:*"))
        .query_async(c)
        .await
        .expect("keys");
    if !keys.is_empty() {
        cmd("DEL").arg(keys).exec_async(c).await.expect("del docs");
    }
}

/// Whether the lane is Valkey (`ZEDIS_IT_FLAVOR=valkey`), where the module
/// behind a familiar name is a different program with its own rules.
fn valkey_lane() -> bool {
    env::var("ZEDIS_IT_FLAVOR").is_ok_and(|flavor| flavor == "valkey")
}

async fn supports(id: &str, floor: Floor) -> bool {
    get_connection_manager()
        .get_client(id, 0)
        .await
        .expect("client")
        .supports(floor)
}

mod acl;
mod cluster;
mod cluster_admin;
mod compatible;
mod connect;
mod keys;
mod replication;
mod scripts;
mod sentinel;
mod server;
mod sessions;
mod stack;
mod transfer;
mod transport;
mod values;
