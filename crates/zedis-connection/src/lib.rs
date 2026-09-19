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

//! Redis connection layer for Zedis: pooled clients, server config,
//! SSH tunnels, and command helpers. GUI-free — the app re-exports this
//! crate through `crate::connection` / `crate::helpers` / `crate::error`.

use tracing::info;

pub mod clients;
pub mod error;
pub mod floors;
pub mod reply;
pub mod reply_format;
pub mod script_kill;
#[cfg(not(target_family = "wasm"))]
pub mod sentinel;
pub mod string;
pub mod time;

mod acl;
/// Native-only: dialing, the local key store and the local files.
///
/// Excluded from the browser build, where Redis is reached through the HTTP
/// bridge and there is no socket, keychain or filesystem to use (ADR 9).
macro_rules! native_only {
    ($($m:ident),* $(,)?) => { $( #[cfg(not(target_family = "wasm"))] mod $m; )* };
}
native_only!(
    async_connection,
    diagnostics,
    master_key,
    readable_import,
    ssh_cluster_connection,
    ssh_stream,
    ssh_tunnel,
);
mod bitmap;
mod bridge;
mod command;
#[cfg(not(target_family = "wasm"))]
mod compare;
mod config;
mod conn;
mod danger;
mod dump_restore;
mod entry_check;
mod functions;
mod geo;
mod hash_fields;
mod hotkeys;
mod hyperloglog;
mod import_clients;
mod key_ops;
mod latency;
mod list_ops;
mod lua_script;
mod manager;
mod module_ops;
#[cfg(not(target_family = "wasm"))]
mod multi_search;
mod panel_ops;
mod probabilistic;
mod probe;
mod readable_export;
mod search;
mod server_config;
mod server_db;
mod server_report;
mod slot_stats;
mod stream_tail;
#[cfg(not(target_family = "wasm"))]
mod subscription;
mod terminal;
mod vector_set;

pub use acl::{
    AclDryRun, AclLogEntry, AclSelector, AclUser, acl_del_user, acl_dryrun, acl_file, acl_genpass, acl_get_user,
    acl_list, acl_load, acl_log, acl_log_reset, acl_save, acl_set_user, acl_whoami, split_acl_rules,
};
#[cfg(not(target_family = "wasm"))]
pub use async_connection::{
    client_name, open_monitor_connection, open_node_connection, open_node_connection_cached, open_seed_connection,
    open_single_connection, set_redis_connection_timeout, set_redis_response_timeout,
};
pub use bitmap::{BitOpKind, BitmapInfo, bit_field, bit_op, bitmap_info, set_bit};
pub use bridge::{
    BridgeConn, BridgeError, BridgeErrorKind, BridgeReply, BridgeRequest, BridgeServerStore, BridgeTransport,
    PipelineSpec, bridge_server_store, bridge_transport, set_bridge_server_store, set_bridge_transport,
};
/// What stands in for redis-rs's `Cmd::query_async` / `Pipeline::query_async`
/// in the browser, where the `aio` feature that provides them cannot be built.
/// A file that sends commands imports these and changes nothing else — inherent
/// methods win over trait methods, so on the desktop they are never in scope
/// and never consulted (ADR 9).
#[cfg(target_family = "wasm")]
pub use bridge::{BridgePipeline, BridgeQuery};
pub use clients::{
    KillFilter, PauseMode, client_kill_by, client_kill_id, client_list, client_pause, client_unpause,
    kill_filter_commands, kill_filter_summary, pause_args,
};
#[cfg(not(target_family = "wasm"))]
pub use compare::{
    CompareOptions, CompareProgress, CompareReport, CompareSide, CompareStage, DifferingKey, KeyDifference,
    compare_prefix, prefix_pattern, values_equal,
};
#[cfg(not(target_family = "wasm"))]
pub use config::servers_toml_redacted;
#[cfg(target_family = "wasm")]
pub use config::set_servers_cache;
pub use config::{
    ImportError, RedisServer, SERVER_TYPE_AUTO, SERVER_TYPE_CLUSTER, SERVER_TYPE_SENTINEL, SERVER_TYPE_STANDALONE,
    TAG_ENV_LABELS, get_server_groups, tag_color_index,
};
pub use config::{get_server, get_servers, save_servers};
pub use conn::RedisAsyncConn;
pub use danger::{
    ConfirmStrictness, DangerKind, classify_dangerous, classify_dangerous_line, confirm_strictness, is_write_command,
    requires_write_confirm,
};
#[cfg(not(target_family = "wasm"))]
pub use diagnostics::{
    DiagHint, DiagOutcome, DiagStage, DiagStatus, diag_stages, diag_timeout, dial_endpoint, probe_dns, probe_redis,
    probe_ssh_auth, probe_ssh_tunnel, probe_tcp,
};
pub use dump_restore::{
    ConflictMode, ConflictPreview, DumpEntry, RestoreStatus, copy_key, dump_keys_chunk, preview_key_conflicts,
    restore_key, restore_keys_chunk,
};
/// The `.zdis` file itself: desktop only, there being no file in a tab.
#[cfg(not(target_family = "wasm"))]
pub use dump_restore::{DumpHeader, DumpReader, DumpWriter, preview_dump_conflicts};
pub use entry_check::sentinel_master_names;
#[cfg(not(target_family = "wasm"))]
pub use entry_check::test_connection;
pub use functions::{
    FunctionLibrary, FunctionMeta, FunctionRestorePolicy, FunctionStats, LibraryValidateError, LibraryValidation,
    function_delete, function_dump, function_fcall, function_flush, function_list, function_load, function_restore,
    function_stats, validate_library_source,
};
pub use geo::{GeoMember, GeoSample, GeoShape, geo_add, geo_dist, geo_sample, geo_search, zset_looks_geo};
pub use hash_fields::{FieldTtl, rename_hash_field, write_hash_field};
pub use hotkeys::{HotkeyEntry, HotkeysReport};
pub use hyperloglog::{HllEncoding, HllInfo, hll_info, pf_add, pf_merge};
pub use key_ops::{FromEnd, KeyOp, KeyOpOutcome, run_key_op};
pub use latency::{
    LatencyEvent, LatencySample, latency_history, latency_latest, latency_monitor_threshold, latency_reset,
};
pub use list_ops::remove_list_indexes;
pub use lua_script::{
    ScriptRunOutcome, max_keys_index, run_script, script_exists, script_flush, script_load, script_sha1,
};
#[cfg(not(target_family = "wasm"))]
pub use master_key::disable_keychain;
pub use module_ops::{
    TS_AGGREGATORS, TsAlter, TsInfo, TsMRange, TsRule, TsSeries, TsWindow, has_positive_matcher, ts_add, ts_alter,
    ts_create_rule, ts_delete_rule, ts_mrange, ts_window,
};
#[cfg(not(target_family = "wasm"))]
pub use multi_search::{MultiSearchHit, MultiSearchServerResult, multi_search_exact, multi_search_scan};
pub use panel_ops::{
    cluster_slot_stats, command_stats, hotkeys_report, hotkeys_reset, hotkeys_start, hotkeys_stop, key_bytes,
    key_size_distributions, maxmemory_policy, pubsub_channels, sample_memory_usage, scan_values_round, value_preview,
};
pub use probabilistic::{ProbInfo, ProbKind, ProbeOutcome, prob_info, prob_probe};
pub use probe::{
    get_server_features, get_server_heat_probe, invalidate_server_features, note_server_command_error,
    probe_server_features,
};
pub use readable_export::{
    ReadLimits, ReadableEntry, ReadableValue, csv_header, entry_to_csv, entry_to_json, next_stream_id,
    read_readable_chunk,
};
#[cfg(not(target_family = "wasm"))]
pub use readable_import::{
    ImportFormat, ReadableWriteStatus, detect_import_format, parse_readable_entries, preview_import_conflicts,
    sniff_import_format, write_readable_chunk,
};
pub use reply_format::{ReplyFormat, format_exec, format_reply, redis_value_to_json};
#[cfg(not(target_family = "wasm"))]
pub use script_kill::kill_running;
pub use script_kill::{KillOutcome, KillReply, KillTarget};
#[cfg(not(target_family = "wasm"))]
pub use sentinel::{
    SENTINEL_SET_OPTIONS, SentinelMaster, SentinelReply, sentinel_ckquorum, sentinel_failover, sentinel_flushconfig,
    sentinel_masters, sentinel_monitor, sentinel_remove, sentinel_reset, sentinel_set, summarize_replies,
};
pub use server_config::{
    ServerConfig, config_get_all, config_get_named, config_get_one, config_load, config_resetstat, config_rewrite,
    config_set, info_everything,
};
pub use server_db::ServerDb;
pub use server_report::{NodeReply, latency_doctor, memory_doctor, memory_stats};
pub use slot_stats::{SlotStatMetric, SlotStatRow};
#[cfg(not(target_family = "wasm"))]
pub use ssh_tunnel::{HostKeyApprover, HostKeyDecision, HostKeyPrompt, install_crypto_provider, set_host_key_approver};
pub use stream_tail::{StreamTail, StreamTailEntry};
/// A held socket the server pushes on — Pub/Sub, `MONITOR` — has no
/// equivalent over the HTTP bridge, and the panels that read one are left out
/// of the web build (ADR 9).
#[cfg(not(target_family = "wasm"))]
pub use subscription::{
    ChannelMessage, ChannelSubscription, MonitorFeed, MonitorFeeds, SubscribeKind, open_monitor_feeds,
};
pub use terminal::{ExecReplies, TerminalReply, TerminalSession};
pub use vector_set::{
    VectorNeighbour, VectorSetInfo, VectorSim, VectorSimOptions, vset_info, vset_remove, vset_set_attr, vset_sim,
};

/// A connection of the caller's own, through the bridge.
///
/// The desktop dials a second socket; the browser asks for a bridge session,
/// which is the same promise — one backend connection, this caller's alone —
/// so `SELECT`, `MULTI` and `CLIENT SETNAME` stay where they were typed
/// (ADR 4). Same signature as the native dialer, so the terminal, the live
/// tail and the client list keep their call sites.
#[cfg(target_family = "wasm")]
pub async fn open_single_connection(
    config: &config::RedisServer,
    db: usize,
    _use_cache: bool,
) -> Result<conn::RedisAsyncConn, error::Error> {
    manager::get_connection_manager()
        .open_dedicated_connection(&config.id, db)
        .await
}

/// Nothing to install: the browser build links no rustls, because every TLS
/// handshake Zedis makes on the web is made by the bridge (ADR 9). Kept as a
/// no-op so `run()` reads the same on both targets — the call is a promise
/// that no connection happens before a provider is chosen, and that promise
/// is trivially true here.
#[cfg(target_family = "wasm")]
pub fn install_crypto_provider() {}

pub use manager::get_connection_manager;
pub use manager::{
    AccessMode, CLUSTER_HASH_SLOTS, ClusterSlotMap, CommandLogKind, CommandStat, ExpireCondition, FAILOVER_TIMEOUT_MS,
    HeatMetric, HeatProbe, KeyMemoryUsage, MAX_PUBSUB_CHANNELS, MatchLocation, PubsubChannel, PubsubChannelsSnapshot,
    REBALANCE_THRESHOLD_PCT, RebalanceMove, RedisClientDescription, SlowLogEntry, ValueMatch, ValueSearchRound,
    command_log_reset, command_logs, group_slot_ranges, plan_cluster_rebalance, plan_reshard_slots, slots_in_ranges,
    unassigned_slot_ranges,
};
/// Slot migration and sharded Pub/Sub are cluster surgery and a held socket
/// — server-side work either way, and both are panels the web build drops
/// (ADR 9).
#[cfg(not(target_family = "wasm"))]
pub use manager::{
    AtomicSlotMigration, ShardedPubSub, cluster_cancel_slot_migrations, cluster_get_slot_migrations,
    cluster_migrate_slots,
};
pub use search::{
    AggregateOptions, AggregateResult, CreateFieldSpec, CreateIndexOptions, FieldKind, FieldSchema, IndexInfo,
    ReducerFn, ReducerSpec, SearchOptions, SearchResult, SpellingSuggestion, escape_tag_value, ft_aggregate,
    ft_alter_add, ft_create, ft_dropindex, ft_explain, ft_info, ft_list, ft_profile, ft_search, ft_spellcheck,
    ft_tagvals,
};
// The capability matrix is pure logic and lives in `zedis-core`; re-exported
// here so call sites keep using `crate::connection::Capability` unchanged.
pub use zedis_core::capability::Capability;
pub use zedis_core::features::{CommandStatus, ServerCommand, ServerFeatures, ServerFlavor};
pub use zedis_core::replication::{ReplicationInfo, ReplicationReplica, ReplicationRole};
/// Sweep what has gone stale. The client cache is swept on both targets; the
/// socket pool and the SSH session store exist only where there are sockets.
pub fn clear_expired_cache() {
    #[cfg(not(target_family = "wasm"))]
    {
        let (removed_count, total_count) = async_connection::clear_expired_connection_pool();
        if removed_count > 0 {
            info!(removed_count, total_count, "clear expired redis connection")
        }
    }

    let (removed_count, total_count) = manager::clear_expired_clients();
    if removed_count > 0 {
        info!(removed_count, total_count, "clear expired redis client")
    }

    #[cfg(not(target_family = "wasm"))]
    {
        let (removed_count, total_count) = ssh_tunnel::clear_expired_ssh_sessions();
        if removed_count > 0 {
            info!(removed_count, total_count, "clear expired ssh session")
        }
    }
}
pub use command::*;
