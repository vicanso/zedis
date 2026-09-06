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

//! Feature probe: finds out which commands the connected server offers this
//! user (see `zedis_core::features` for the pure side).
//!
//! Runs once per server, in the background, right after the first connect —
//! never on the connect critical path, and never again on a db switch. Every
//! probe is read-only, idempotent and cheap (`INFO server`, `SLOWLOG GET 0`,
//! `MEMORY USAGE <key that cannot exist>`, …); mutating commands (`MONITOR`,
//! `BGSAVE`, `CONFIG SET`, `FLUSHDB`, …) are never executed — their
//! existence comes from `COMMAND INFO` and their permission from
//! `ACL DRYRUN` (Redis 7+). All probes go out concurrently on one dedicated
//! connection (a single round trip), and the connection is discarded
//! afterwards so a proxy that drops the link on an unknown command can't
//! poison the shared pool.

use crate::async_connection::open_single_connection;
use crate::config::get_server;
use crate::error::Error;
use futures::future::join_all;
use redis::{Cmd, RedisError, Value, aio::MultiplexedConnection, cmd};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use tracing::{debug, info, warn};
use zedis_core::features::{CommandStatus, ReplyClass, ServerCommand, ServerFeatures, ServerFlavor, classify_reply};

type Result<T, E = Error> = std::result::Result<T, E>;

/// A key that no real dataset contains — lets `MEMORY USAGE` / `DUMP` /
/// `OBJECT ENCODING` run without touching user data (they answer nil).
const PROBE_KEY: &str = "__zedis:probe:never-exists__";
/// 40 zero hex digits: a SHA1 no script will ever have.
const PROBE_SHA: &str = "0000000000000000000000000000000000000000";

/// Per-server results, keyed by server id. `None` means "not probed yet",
/// which the UI treats as everything-available.
static FEATURES: LazyLock<Mutex<HashMap<String, Arc<ServerFeatures>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// The cached matrix for `server_id`, or the optimistic default.
pub fn get_server_features(server_id: &str) -> Arc<ServerFeatures> {
    FEATURES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(server_id)
        .cloned()
        .unwrap_or_default()
}

/// Forgets the cached matrix; the next connect re-probes. Call when the
/// server's credentials change or the user asks for a re-probe.
pub fn invalidate_server_features(server_id: &str) {
    FEATURES.lock().unwrap_or_else(|e| e.into_inner()).remove(server_id);
}

fn store(server_id: &str, features: ServerFeatures) -> Arc<ServerFeatures> {
    let features = Arc::new(features);
    FEATURES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(server_id.to_string(), features.clone());
    features
}

/// Feeds a runtime failure back into the matrix: a `NOPERM` / `unknown
/// command` reply for a command the probe thought usable flips it, so the
/// panel that hit it degrades from then on. Returns the commands that
/// changed (empty for unrelated errors) and the updated matrix.
pub fn note_server_command_error(
    server_id: &str,
    error: &Error,
) -> (Vec<(ServerCommand, CommandStatus)>, Arc<ServerFeatures>) {
    let Error::Redis { source } = error else {
        return (Vec::new(), get_server_features(server_id));
    };
    let mut features = (*get_server_features(server_id)).clone();
    let changed = features.note_reply_error(source.code(), source.detail().unwrap_or_default());
    if changed.is_empty() {
        return (changed, get_server_features(server_id));
    }
    info!(
        server_id,
        ?changed,
        "server feature matrix updated from a command error"
    );
    (changed, store(server_id, features))
}

/// Probes `server_id` (through database `db`) and caches the result.
pub async fn probe_server_features(server_id: &str, db: usize) -> Result<Arc<ServerFeatures>> {
    let server = get_server(server_id)?;
    let conn = open_single_connection(&server, db, false).await?;
    let features = run_probe(conn, || {
        let server = server.clone();
        async move { open_single_connection(&server, db, false).await }
    })
    .await;
    let unusable = features.unusable();
    info!(
        server_id,
        flavor = features.flavor.label(),
        ?unusable,
        "server feature probe finished"
    );
    Ok(store(server_id, features))
}

/// The probe proper, parameterised over a connection factory so the proxy
/// fallback (fresh connection per command) is testable.
async fn run_probe<F, Fut>(conn: MultiplexedConnection, reconnect: F) -> ServerFeatures
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<MultiplexedConnection>>,
{
    let mut features = ServerFeatures::probed_empty();

    // Brand first: INFO server is tiny and also proves the link works.
    let info: Option<String> = cmd("INFO").arg("server").query_async(&mut conn.clone()).await.ok();
    if let Some(info) = &info {
        features.flavor = ServerFlavor::from_info(info_fields(info));
    }

    // Safe probes, all in flight at once on the multiplexed connection.
    let safe: Vec<ServerCommand> = ServerCommand::ALL
        .iter()
        .copied()
        .filter(|c| !c.is_mutating())
        .collect();
    let results = join_all(safe.iter().map(|c| {
        let mut conn = conn.clone();
        let command = *c;
        async move {
            let result: std::result::Result<Value, RedisError> = probe_cmd(command).query_async(&mut conn).await;
            (command, result)
        }
    }))
    .await;
    let mut dropped = false;
    for (command, result) in results {
        let status = status_from_result(&result);
        if let Err(e) = &result
            && transport_failed(e)
        {
            dropped = true;
        }
        features.set(command, status);
    }

    // A proxy such as Twemproxy closes the connection on the first command it
    // doesn't know, which fails every probe queued behind it. Re-run those
    // one at a time, each on a fresh connection, so each gets its own verdict.
    if dropped {
        warn!("probe connection was dropped mid-flight; retrying unknown probes one per connection");
        for command in safe.iter().copied() {
            if features.status(command) != CommandStatus::Unknown {
                continue;
            }
            let Ok(mut fresh) = reconnect().await else {
                break;
            };
            let result: std::result::Result<Value, RedisError> = probe_cmd(command).query_async(&mut fresh).await;
            features.set(command, status_from_result(&result));
        }
    }

    // Mutating commands: existence via COMMAND INFO (nil = unknown to the
    // server), permission via ACL DRYRUN (Redis 7+). Neither executes them.
    let mutating: Vec<ServerCommand> = ServerCommand::ALL.iter().copied().filter(|c| c.is_mutating()).collect();
    let mut conn = match reconnect_if(dropped, &conn, &reconnect).await {
        Some(c) => c,
        None => return features,
    };
    probe_existence(&mut conn, &mutating, &mut features).await;
    probe_permissions(&mut conn, &mutating, &mut features).await;
    features
}

async fn reconnect_if<F, Fut>(
    dropped: bool,
    conn: &MultiplexedConnection,
    reconnect: &F,
) -> Option<MultiplexedConnection>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<MultiplexedConnection>>,
{
    if !dropped {
        return Some(conn.clone());
    }
    reconnect().await.ok()
}

/// `COMMAND INFO w1 w2 …` — one call; a nil entry means the server has never
/// heard of that top-level word. Subcommands are judged by their container
/// (`COMMAND INFO config|set` only works on 7+, and a renamed-away container
/// takes its subcommands with it anyway).
async fn probe_existence(conn: &mut MultiplexedConnection, commands: &[ServerCommand], features: &mut ServerFeatures) {
    let mut request = cmd("COMMAND");
    request.arg("INFO");
    for c in commands {
        request.arg(c.word().to_ascii_lowercase());
    }
    let reply: std::result::Result<Vec<Value>, RedisError> = request.query_async(conn).await;
    let entries = match reply {
        Ok(entries) if entries.len() == commands.len() => entries,
        Ok(_) => return,
        Err(e) => {
            debug!(error = %e, "COMMAND INFO unavailable; mutating commands stay Unknown unless DRYRUN answers");
            return;
        }
    };
    for (command, entry) in commands.iter().zip(entries) {
        if matches!(entry, Value::Nil) {
            features.set(*command, CommandStatus::Missing);
        }
    }
}

/// `ACL DRYRUN <user> <command …>`: `OK` means allowed, any other *status
/// string* is the denial message. An error reply means DRYRUN itself is
/// unavailable (Redis < 7, proxies, or an ACL that hides it) — leave the
/// existence verdict alone.
async fn probe_permissions(
    conn: &mut MultiplexedConnection,
    commands: &[ServerCommand],
    features: &mut ServerFeatures,
) {
    let user: String = match cmd("ACL").arg("WHOAMI").query_async(conn).await {
        Ok(user) => user,
        Err(e) => {
            debug!(error = %e, "ACL WHOAMI unavailable; skipping DRYRUN");
            return;
        }
    };
    for command in commands {
        // A command the server doesn't know can't be dry-run.
        if features.status(*command) == CommandStatus::Missing {
            continue;
        }
        let mut request = cmd("ACL");
        request.arg("DRYRUN").arg(&user);
        for arg in dryrun_args(*command) {
            request.arg(*arg);
        }
        let reply: std::result::Result<Value, RedisError> = request.query_async(conn).await;
        let status = match reply {
            Ok(Value::Okay) => CommandStatus::Available,
            Ok(Value::SimpleString(s)) | Ok(Value::VerbatimString { text: s, .. }) if s == "OK" => {
                CommandStatus::Available
            }
            Ok(Value::BulkString(bytes)) if bytes == b"OK" => CommandStatus::Available,
            Ok(_) => CommandStatus::Denied,
            Err(e) => {
                debug!(error = %e, "ACL DRYRUN unavailable; mutating permissions stay as probed");
                return;
            }
        };
        features.set(*command, status);
    }
}

/// The read-only probe for a non-mutating command.
fn probe_cmd(command: ServerCommand) -> Cmd {
    let mut c = cmd(command.word());
    match command {
        ServerCommand::Info => {
            c.arg("server");
        }
        ServerCommand::Scan => {
            c.arg(0).arg("COUNT").arg(1);
        }
        ServerCommand::Dbsize | ServerCommand::Lastsave => {}
        ServerCommand::MemoryUsage => {
            c.arg("USAGE").arg(PROBE_KEY);
        }
        ServerCommand::ObjectEncoding => {
            c.arg("ENCODING").arg(PROBE_KEY);
        }
        ServerCommand::Dump => {
            c.arg(PROBE_KEY);
        }
        ServerCommand::ConfigGet => {
            c.arg("GET").arg("maxmemory");
        }
        // Exactly the subcommand the panel needs, with an empty page.
        ServerCommand::SlowlogGet => {
            c.arg("GET").arg(0);
        }
        ServerCommand::LatencyLatest => {
            c.arg("LATEST");
        }
        // `ID 0` filters to nothing (6.2+); older servers answer "syntax
        // error", which still proves the command exists.
        ServerCommand::ClientList => {
            c.arg("LIST").arg("ID").arg(0);
        }
        ServerCommand::AclList => {
            c.arg("LIST");
        }
        ServerCommand::FunctionList => {
            c.arg("LIST").arg("LIBRARYNAME").arg(PROBE_KEY);
        }
        ServerCommand::ScriptExists => {
            c.arg("EXISTS").arg(PROBE_SHA);
        }
        // The channel browser's listing, filtered down to nothing.
        ServerCommand::PubsubChannels => {
            c.arg("CHANNELS").arg(PROBE_KEY);
        }
        ServerCommand::ClusterInfo => {
            c.arg("INFO");
        }
        // One slot, zero traffic. Pre-8.2 answers "unknown subcommand"
        // (→ Missing); a standalone 8.2+ answers "cluster support
        // disabled", which proves the command exists (→ Available) — the
        // slot-stats section is additionally gated on the cluster server
        // type, so that verdict never shows a dead panel.
        ServerCommand::ClusterSlotStats => {
            c.arg("SLOT-STATS").arg("SLOTSRANGE").arg(0).arg(0);
        }
        // Read-only: with tracking never started it answers nil; with a
        // collection present it returns the report without touching it.
        ServerCommand::HotkeysGet => {
            c.arg("GET");
        }
        // Zero entries of the slow log — proves the command, reads nothing.
        ServerCommand::CommandlogGet => {
            c.arg("GET").arg(0).arg("SLOW");
        }
        // Mutating variants never reach here (filtered by `is_mutating`);
        // fall back to a harmless no-op so a future slip can't execute them.
        _ => {
            c = cmd("PING");
        }
    }
    c
}

/// Arguments for `ACL DRYRUN` — syntactically valid, never executed.
fn dryrun_args(command: ServerCommand) -> &'static [&'static str] {
    match command {
        ServerCommand::Restore => &["RESTORE", PROBE_KEY, "0", ""],
        ServerCommand::Migrate => &["MIGRATE", "127.0.0.1", "0", PROBE_KEY, "0", "1"],
        ServerCommand::Unlink => &["UNLINK", PROBE_KEY],
        ServerCommand::ConfigSet => &["CONFIG", "SET", "maxmemory", "0"],
        ServerCommand::ClientKill => &["CLIENT", "KILL", "ID", "0"],
        ServerCommand::AclSetUser => &["ACL", "SETUSER", PROBE_KEY],
        ServerCommand::FunctionLoad => &["FUNCTION", "LOAD", "x"],
        ServerCommand::Eval => &["EVAL", "return 1", "0"],
        ServerCommand::Monitor => &["MONITOR"],
        ServerCommand::Bgsave => &["BGSAVE"],
        ServerCommand::Publish => &["PUBLISH", PROBE_KEY, "x"],
        ServerCommand::Subscribe => &["SUBSCRIBE", PROBE_KEY],
        ServerCommand::FlushDb => &["FLUSHDB"],
        ServerCommand::HotkeysStart => &["HOTKEYS", "START", "METRICS", "1", "CPU"],
        ServerCommand::HSetEx => &["HSETEX", PROBE_KEY, "FIELDS", "1", "f", "v"],
        ServerCommand::Replicaof => &["REPLICAOF", "NO", "ONE"],
        ServerCommand::Failover => &["FAILOVER", "ABORT"],
        _ => &["PING"],
    }
}

fn transport_failed(e: &RedisError) -> bool {
    e.is_io_error() || e.is_connection_dropped() || e.is_timeout() || e.is_connection_refusal()
}

fn status_from_result(result: &std::result::Result<Value, RedisError>) -> CommandStatus {
    match result {
        Ok(_) => CommandStatus::Available,
        Err(e) if transport_failed(e) => CommandStatus::Unknown,
        Err(e) => status_from_reply(e.code(), e.detail().unwrap_or_default()),
    }
}

/// Pure verdict from an error reply; split out so it can be tested without
/// a server. Any error that is neither "unknown command" nor `NOPERM` proves
/// the command exists and this user may run it — `CLUSTER INFO` answering
/// "cluster support disabled" on a standalone server included: that's a
/// server type, not a limitation.
fn status_from_reply(code: Option<&str>, message: &str) -> CommandStatus {
    match classify_reply(code, message) {
        ReplyClass::Missing => CommandStatus::Missing,
        ReplyClass::Denied => CommandStatus::Denied,
        ReplyClass::Other => CommandStatus::Available,
    }
}

/// `key:value` lines of an `INFO` reply.
fn info_fields(info: &str) -> impl Iterator<Item = (&str, &str)> {
    info.lines().filter_map(|line| line.split_once(':'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_has_a_probe_strategy() {
        for &c in ServerCommand::ALL {
            if c.is_mutating() {
                let args = dryrun_args(c);
                assert_eq!(args[0], c.word(), "{c:?}: DRYRUN args must start with the command word");
            } else {
                let probe = probe_cmd(c);
                let wire = String::from_utf8_lossy(&probe.get_packed_command()).into_owned();
                assert!(wire.contains(c.word()), "{c:?}: probe must issue {}", c.word());
                if let Some(sub) = c.subcommand() {
                    assert!(wire.contains(sub), "{c:?}: probe must issue the {sub} subcommand");
                }
            }
        }
    }

    #[test]
    fn reply_verdicts() {
        assert_eq!(
            status_from_reply(Some("ERR"), "unknown command 'CONFIG', with args beginning with: 'GET'"),
            CommandStatus::Missing
        );
        assert_eq!(
            status_from_reply(
                Some("NOPERM"),
                "User x has no permissions to run the 'config|get' command"
            ),
            CommandStatus::Denied
        );
        // A semantic error proves the command exists — including CLUSTER INFO
        // on a standalone server, which must not read as "limited".
        assert_eq!(status_from_reply(Some("ERR"), "syntax error"), CommandStatus::Available);
        assert_eq!(
            status_from_reply(Some("ERR"), "This instance has cluster support disabled"),
            CommandStatus::Available
        );
    }

    #[test]
    fn info_fields_parse_key_value_lines() {
        let fields: Vec<_> = info_fields("# Server\r\nredis_version:7.2.4\r\nvalkey_version:8.0.1\r\n\r\n").collect();
        assert_eq!(fields, vec![("redis_version", "7.2.4"), ("valkey_version", "8.0.1")]);
        assert_eq!(ServerFlavor::from_info(fields), ServerFlavor::Valkey);
    }

    #[test]
    fn cache_round_trip_and_runtime_feedback() {
        let id = format!("probe-test-{}", std::process::id());
        assert!(!get_server_features(&id).probed);
        let mut features = ServerFeatures::probed_empty();
        features.set(ServerCommand::Scan, CommandStatus::Available);
        store(&id, features);
        assert!(get_server_features(&id).probed);

        // A NOPERM reply from a real command flips the matrix.
        let err = Error::Redis {
            source: RedisError::from((
                redis::ErrorKind::Server(redis::ServerErrorKind::ResponseError),
                "NOPERM",
                "NOPERM User u has no permissions to run the 'config|get' command".to_string(),
            )),
        };
        let (changed, features) = note_server_command_error(&id, &err);
        // Constructed errors carry no server code; the message alone is
        // enough for classification.
        assert_eq!(changed, vec![(ServerCommand::ConfigGet, CommandStatus::Denied)]);
        assert!(!features.is_usable(ServerCommand::ConfigGet));
        assert!(features.is_usable(ServerCommand::Scan));

        invalidate_server_features(&id);
        assert!(!get_server_features(&id).probed);
    }

    /// Live probe against a real server — run by hand:
    /// `ZEDIS_PROBE_URL=redis://127.0.0.1:6379 cargo test -p zedis-connection probe::tests::live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_probe() {
        let url = std::env::var("ZEDIS_PROBE_URL").expect("ZEDIS_PROBE_URL");
        let client = redis::Client::open(url).expect("client");
        let features = smol::block_on(async {
            let conn = client.get_multiplexed_async_connection().await.expect("connect");
            run_probe(conn, || {
                let client = client.clone();
                async move { Ok(client.get_multiplexed_async_connection().await?) }
            })
            .await
        });
        println!("flavor: {:?}", features.flavor);
        for &c in ServerCommand::ALL {
            println!("{:<18} {:?}", c.label(), features.status(c));
        }
        assert!(features.probed);
        assert_eq!(features.status(ServerCommand::Info), CommandStatus::Available);
    }
}
