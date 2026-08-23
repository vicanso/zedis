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

use redis::cmd;
use std::collections::HashSet;
use std::env;
use std::sync::Once;
use zedis_connection::error::ConnectionErrorKind;
use zedis_connection::{
    CommandStatus, ConflictMode, RedisAsyncConn, RedisServer, RestoreStatus, ServerCommand, ServerFlavor,
    dump_keys_chunk, get_connection_manager, get_servers, open_single_connection, probe_server_features,
    restore_keys_chunk, save_servers,
};

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

async fn version_at_least(id: &str, version: &str) -> bool {
    get_connection_manager()
        .get_client(id, 0)
        .await
        .expect("client")
        .is_at_least_version(version)
}

// ── standalone ───────────────────────────────────────────────────────────

#[test]
#[ignore]
fn standalone_connect_reports_metadata() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        client.ping().await.expect("ping");
        assert!(!client.version().is_empty(), "version must be read from INFO server");
        assert!(client.databases() >= 1);
        assert_eq!(client.nodes(), (1, 1), "standalone: one master, one node in total");
        assert_eq!(
            format!("{:?}", client.access_mode()),
            "ReadWrite",
            "the default user is not read-only"
        );
        client.dbsize().await.expect("dbsize");
    });
}

#[test]
#[ignore]
fn standalone_scan_sees_every_type_it_wrote() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        let mut c = conn(&id, 0).await;
        let prefix = unique("types");
        let keys = [
            (format!("{prefix}:s"), "string"),
            (format!("{prefix}:h"), "hash"),
            (format!("{prefix}:l"), "list"),
            (format!("{prefix}:z"), "zset"),
            (format!("{prefix}:set"), "set"),
        ];
        cmd("SET")
            .arg(&keys[0].0)
            .arg("v")
            .exec_async(&mut c)
            .await
            .expect("set");
        cmd("HSET")
            .arg(&keys[1].0)
            .arg("f")
            .arg("v")
            .exec_async(&mut c)
            .await
            .expect("hset");
        cmd("RPUSH")
            .arg(&keys[2].0)
            .arg("a")
            .exec_async(&mut c)
            .await
            .expect("rpush");
        cmd("ZADD")
            .arg(&keys[3].0)
            .arg(1)
            .arg("m")
            .exec_async(&mut c)
            .await
            .expect("zadd");
        cmd("SADD")
            .arg(&keys[4].0)
            .arg("m")
            .exec_async(&mut c)
            .await
            .expect("sadd");
        cmd("EXPIRE")
            .arg(&keys[0].0)
            .arg(600)
            .exec_async(&mut c)
            .await
            .expect("expire");

        // Page until every cursor is 0 — the tree's own loop.
        let mut found: Vec<(String, String, i64)> = Vec::new();
        let mut cursors = None;
        loop {
            let (next, page) = client
                .scan(cursors, &format!("{prefix}:*"), 100, true, None)
                .await
                .expect("scan");
            found.extend(page);
            if next.iter().sum::<u64>() == 0 {
                break;
            }
            cursors = Some(next);
        }
        let by_name: HashSet<(String, String)> = found.iter().map(|(k, t, _)| (k.clone(), t.clone())).collect();
        for (key, kind) in &keys {
            assert!(
                by_name.contains(&(key.clone(), kind.to_string())),
                "missing {key} as {kind}: {found:?}"
            );
        }
        let ttl = found
            .iter()
            .find(|(k, _, _)| k == &keys[0].0)
            .map(|(_, _, ttl)| *ttl)
            .expect("ttl row");
        assert!((1..=600).contains(&ttl), "SCAN with_ttl must carry the TTL, got {ttl}");

        // The server-side TYPE filter (6.0+) and the client-side fallback agree.
        let (_, only_hashes) = client
            .first_scan(&format!("{prefix}:*"), 100, false, Some("hash"))
            .await
            .expect("scan hash");
        assert_eq!(only_hashes.len(), 1);
        assert_eq!(only_hashes[0].0, keys[1].0);

        assert_eq!(client.key_type(&keys[2].0).await.expect("type"), "list");
        assert_eq!(client.get_key_bytes(&keys[0].0).await.expect("get"), b"v");
        assert!(client.memory_usage(&keys[1].0, "hash").await.expect("memory usage") > 0);

        for (key, _) in &keys {
            cmd("DEL").arg(key).exec_async(&mut c).await.expect("del");
        }
    });
}

#[test]
#[ignore]
fn standalone_dump_restore_round_trips_a_key() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let mut c = conn(&id, 0).await;
        let key = unique("dump");
        cmd("HSET")
            .arg(&key)
            .arg("a")
            .arg("1")
            .arg("b")
            .arg("2")
            .exec_async(&mut c)
            .await
            .expect("hset");
        cmd("EXPIRE")
            .arg(&key)
            .arg(300)
            .exec_async(&mut c)
            .await
            .expect("expire");
        let entries = dump_keys_chunk(&mut c, std::slice::from_ref(&key)).await.expect("dump");
        assert_eq!(entries.len(), 1);
        cmd("DEL").arg(&key).exec_async(&mut c).await.expect("del");

        let statuses = restore_keys_chunk(&mut c, &entries, ConflictMode::Skip)
            .await
            .expect("restore");
        assert!(matches!(statuses[0], RestoreStatus::Written), "{statuses:?}");
        let b: String = cmd("HGET").arg(&key).arg("b").query_async(&mut c).await.expect("hget");
        assert_eq!(b, "2");
        let ttl: i64 = cmd("TTL").arg(&key).query_async(&mut c).await.expect("ttl");
        assert!((1..=300).contains(&ttl), "RESTORE must carry the TTL over, got {ttl}");

        // Skip leaves an existing key alone; Overwrite replaces it.
        cmd("HSET")
            .arg(&key)
            .arg("b")
            .arg("changed")
            .exec_async(&mut c)
            .await
            .expect("hset");
        let statuses = restore_keys_chunk(&mut c, &entries, ConflictMode::Skip)
            .await
            .expect("restore");
        assert!(matches!(statuses[0], RestoreStatus::Skipped), "{statuses:?}");
        let statuses = restore_keys_chunk(&mut c, &entries, ConflictMode::Overwrite)
            .await
            .expect("restore");
        assert!(matches!(statuses[0], RestoreStatus::Written), "{statuses:?}");
        let b: String = cmd("HGET").arg(&key).arg("b").query_async(&mut c).await.expect("hget");
        assert_eq!(b, "2");
        cmd("DEL").arg(&key).exec_async(&mut c).await.expect("del");
    });
}

#[test]
#[ignore]
fn standalone_feature_probe_matches_the_server() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let features = probe_server_features(&id, 0).await.expect("probe");
        assert!(features.probed);
        for c in [
            ServerCommand::Info,
            ServerCommand::Scan,
            ServerCommand::Dbsize,
            ServerCommand::ConfigGet,
            ServerCommand::SlowlogGet,
            ServerCommand::ClientList,
            ServerCommand::Dump,
            // Exists on a standalone server too — "cluster support disabled"
            // is a server type, not a limitation.
            ServerCommand::ClusterInfo,
        ] {
            assert_eq!(features.status(c), CommandStatus::Available, "{c:?}");
        }
        let has_functions = version_at_least(&id, "7.0.0").await;
        let expect_functions = if has_functions {
            CommandStatus::Available
        } else {
            CommandStatus::Missing
        };
        assert_eq!(features.status(ServerCommand::FunctionList), expect_functions);
        // Mutating commands are never executed: on 7+ ACL DRYRUN says
        // Available, on 6.x COMMAND INFO proves existence and the status
        // stays optimistic — either way they must not read as unusable.
        for c in [ServerCommand::Monitor, ServerCommand::Bgsave, ServerCommand::FlushDb] {
            assert!(features.is_usable(c), "{c:?} → {:?}", features.status(c));
            if has_functions {
                assert_eq!(features.status(c), CommandStatus::Available, "{c:?}");
            }
        }
        if let Ok(expected) = env::var("ZEDIS_IT_FLAVOR") {
            assert_eq!(
                features.flavor.label().to_ascii_lowercase(),
                expected.to_ascii_lowercase()
            );
        } else {
            assert!(matches!(features.flavor, ServerFlavor::Redis | ServerFlavor::Valkey));
        }
    });
}

#[test]
#[ignore]
fn standalone_acl_users_are_classified() {
    smol::block_on(async {
        let admin_id = register(server("it-standalone", standalone())).await;
        let mut admin = conn(&admin_id, 0).await;
        let acl_ok: Result<String, _> = cmd("ACL").arg("WHOAMI").query_async(&mut admin).await;
        if acl_ok.is_err() {
            eprintln!("skipped: server has no ACL (Redis < 6)");
            return;
        }
        let suffix = unique("acl").rsplit(':').take(3).collect::<Vec<_>>().join("_");
        let ro_user = format!("zedis_it_ro_{suffix}");
        let limited_user = format!("zedis_it_limited_{suffix}");
        // Read-only: every read, no writes. Container-level denials keep
        // the rule valid on Redis 6 (subcommand denials are 7.0+).
        cmd("ACL")
            .arg("SETUSER")
            .arg(&ro_user)
            .arg("on")
            .arg(">pw")
            .arg("~*")
            .arg("&*")
            .arg("+@read")
            .arg("+@connection")
            .arg("+acl")
            .arg("+info")
            .arg("+scan")
            .exec_async(&mut admin)
            .await
            .expect("setuser ro");
        cmd("ACL")
            .arg("SETUSER")
            .arg(&limited_user)
            .arg("on")
            .arg(">pw")
            .arg("~*")
            .arg("&*")
            .arg("+@all")
            .arg("-config")
            .arg("-slowlog")
            .arg("-latency")
            .arg("-flushdb")
            .arg("-flushall")
            .arg("-bgsave")
            .exec_async(&mut admin)
            .await
            .expect("setuser limited");

        let mut ro = server(&format!("it-ro-{suffix}"), standalone());
        ro.username = Some(ro_user.clone());
        ro.password = Some("pw".into());
        let ro_id = register(ro).await;
        let client = get_connection_manager().get_client(&ro_id, 0).await.expect("ro client");
        assert_eq!(
            format!("{:?}", client.access_mode()),
            "StrictReadOnly",
            "a read-only ACL user must be detected (DRYRUN on 7+, SET probe before)"
        );

        let mut limited = server(&format!("it-limited-{suffix}"), standalone());
        limited.username = Some(limited_user.clone());
        limited.password = Some("pw".into());
        let limited_id = register(limited).await;
        let features = probe_server_features(&limited_id, 0).await.expect("probe");
        for c in [
            ServerCommand::ConfigGet,
            ServerCommand::SlowlogGet,
            ServerCommand::LatencyLatest,
        ] {
            assert_eq!(features.status(c), CommandStatus::Denied, "{c:?}");
        }
        assert_eq!(features.status(ServerCommand::Scan), CommandStatus::Available);
        if version_at_least(&admin_id, "7.0.0").await {
            for c in [ServerCommand::ConfigSet, ServerCommand::Bgsave, ServerCommand::FlushDb] {
                assert_eq!(features.status(c), CommandStatus::Denied, "{c:?} (ACL DRYRUN)");
            }
        }

        for user in [&ro_user, &limited_user] {
            cmd("ACL")
                .arg("DELUSER")
                .arg(user)
                .exec_async(&mut admin)
                .await
                .expect("deluser");
        }
    });
}

#[test]
#[ignore]
fn standalone_recovers_after_the_server_drops_the_link() {
    smol::block_on(async {
        // Its own server id, so the pooled client it kills is not the one
        // the other (parallel) standalone tests share.
        let id = register(server("it-standalone-linkloss", standalone())).await;
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        client.ping().await.expect("ping before");
        let mut pooled = conn(&id, 0).await;
        let victim: i64 = cmd("CLIENT")
            .arg("ID")
            .query_async(&mut pooled)
            .await
            .expect("client id");

        // Kill exactly that link from a throwaway connection — the pooled
        // multiplexed connection dies the way it does on laptop wake / VPN
        // flip / server restart.
        let mut killer = open_single_connection(&server("it-killer", standalone()), 0, false)
            .await
            .expect("killer connection");
        let killed: i64 = cmd("CLIENT")
            .arg("KILL")
            .arg("ID")
            .arg(victim)
            .query_async(&mut killer)
            .await
            .expect("client kill");
        assert_eq!(killed, 1);

        let err = client.ping().await.expect_err("the cached link must be dead now");
        assert_eq!(err.connection_kind(), ConnectionErrorKind::Network, "{err}");

        // What the app does on that error (note_link_error / heartbeat):
        // drop the cached client and let the next call rebuild it.
        get_connection_manager().remove_client(&id, 0);
        let client = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("rebuilt client");
        client.ping().await.expect("ping after rebuild");
    });
}

// ── tls ──────────────────────────────────────────────────────────────────

#[test]
#[ignore]
fn tls_connects_with_the_root_cert_and_in_insecure_mode() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_TLS");
        let ca = std::fs::read_to_string(env::var("ZEDIS_IT_TLS_CA").expect("ZEDIS_IT_TLS_CA")).expect("read ca");

        let mut trusted = server("it-tls", addr.clone());
        trusted.tls = Some(true);
        trusted.root_cert = Some(ca);
        let id = register(trusted).await;
        let client = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("tls client (root cert)");
        client.ping().await.expect("ping over tls");

        let mut insecure = server("it-tls-insecure", addr.clone());
        insecure.tls = Some(true);
        insecure.insecure = Some(true);
        let id = register(insecure).await;
        let client = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("tls client (insecure)");
        client.ping().await.expect("ping over tls (insecure)");

        // Plaintext against a TLS-only port must fail, not hang.
        let id = register(server("it-tls-plain", addr)).await;
        assert!(get_connection_manager().get_client(&id, 0).await.is_err());
    });
}

// ── sentinel ─────────────────────────────────────────────────────────────

/// `SENTINEL GET-MASTER-ADDR-BY-NAME` straight from the sentinel.
async fn sentinel_master_port(sentinel: &mut redis::aio::MultiplexedConnection, master_name: &str) -> u16 {
    let (_, port): (String, String) = cmd("SENTINEL")
        .arg("GET-MASTER-ADDR-BY-NAME")
        .arg(master_name)
        .query_async(sentinel)
        .await
        .expect("get-master-addr-by-name");
    port.parse().expect("master port")
}

#[test]
#[ignore]
fn sentinel_resolves_the_master_and_follows_a_failover() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_SENTINEL");
        let master_name = env::var("ZEDIS_IT_MASTER_NAME").unwrap_or_else(|_| "mymaster".into());
        let mut sentinel = open_single_connection(&server("it-sentinel-raw", addr.clone()), 0, false)
            .await
            .expect("sentinel connection");
        let before = sentinel_master_port(&mut sentinel, &master_name).await;

        let mut s = server("it-sentinel", addr);
        s.master_name = Some(master_name.clone());
        let id = register(s).await;
        get_connection_manager().remove_client(&id, 0);
        let client = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("sentinel client");
        let masters = client.master_servers();
        assert_eq!(masters.len(), 1, "one monitored master");
        assert_eq!(masters[0].port, before, "the master the sentinel announces");
        assert_eq!(client.nodes_description().server_type, "Sentinel");

        let key = unique("sentinel");
        let mut c = conn(&id, 0).await;
        cmd("SET")
            .arg(&key)
            .arg("via-sentinel")
            .exec_async(&mut c)
            .await
            .expect("set on master");
        let value: String = cmd("GET").arg(&key).query_async(&mut c).await.expect("get");
        assert_eq!(value, "via-sentinel");

        // Force a failover and wait for the sentinel to promote the replica.
        // Right after the topology starts the sentinel may not have rated
        // the replica yet (`NOGOODSLAVE`): retry for a while.
        let mut started = false;
        for _ in 0..60 {
            let reply: Result<String, redis::RedisError> = cmd("SENTINEL")
                .arg("FAILOVER")
                .arg(&master_name)
                .query_async(&mut sentinel)
                .await;
            match reply {
                Ok(_) => {
                    started = true;
                    break;
                }
                Err(e) if e.to_string().contains("NOGOODSLAVE") => {
                    smol::Timer::after(std::time::Duration::from_millis(500)).await;
                }
                Err(e) => panic!("sentinel failover: {e}"),
            }
        }
        assert!(started, "sentinel never accepted the failover");
        let mut after = before;
        for _ in 0..60 {
            smol::Timer::after(std::time::Duration::from_millis(500)).await;
            after = sentinel_master_port(&mut sentinel, &master_name).await;
            if after != before {
                break;
            }
        }
        assert_ne!(after, before, "sentinel never promoted the replica");
        // The sentinel announces the new master before it has reconfigured
        // the old one; wait until the demoted node itself reports `slave`
        // (replica-read-only is the default, so writes bounce from then on).
        // Sentinel kills the demoted node's clients as part of the
        // reconfiguration, so poll on a fresh connection each time.
        let demoted_server = server("it-demoted", ("127.0.0.1".into(), before));
        let mut is_replica = false;
        for _ in 0..60 {
            if let Ok(mut demoted) = open_single_connection(&demoted_server, 0, false).await
                && let Ok(info) = cmd("INFO").arg("replication").query_async::<String>(&mut demoted).await
                && info.contains("role:slave")
            {
                is_replica = true;
                break;
            }
            smol::Timer::after(std::time::Duration::from_millis(500)).await;
        }
        assert!(is_replica, "the old master was never reconfigured as a replica");

        // The cached client still talks to the demoted node: a write now
        // bounces with READONLY — the signal `note_link_error` acts on.
        let write: Result<(), redis::RedisError> = cmd("SET").arg(&key).arg("after-failover").exec_async(&mut c).await;
        match write {
            Ok(()) => panic!("write on the demoted master should have been refused"),
            Err(e) => {
                let kind = zedis_connection::error::Error::from(e).connection_kind();
                assert!(
                    matches!(kind, ConnectionErrorKind::ReadOnly | ConnectionErrorKind::Network),
                    "{kind:?}"
                );
            }
        }
        // …which is to drop the client; discovery then lands on the new master.
        get_connection_manager().remove_client(&id, 0);
        let client = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("re-resolved client");
        assert_eq!(client.master_servers()[0].port, after, "resolved the promoted master");
        let mut c = conn(&id, 0).await;
        cmd("SET")
            .arg(&key)
            .arg("after-failover")
            .exec_async(&mut c)
            .await
            .expect("write on the new master");
        cmd("DEL").arg(&key).exec_async(&mut c).await.expect("del");
    });
}

// ── cluster ──────────────────────────────────────────────────────────────

#[test]
#[ignore]
fn cluster_discovers_nodes_and_scans_every_master() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_CLUSTER");
        let id = register(server("it-cluster", addr)).await;
        let client = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("cluster client");
        assert_eq!(client.nodes(), (3, 6), "3 masters out of 6 nodes");
        assert_eq!(client.nodes_description().server_type, "Cluster");

        // Keys without hash tags spread across slots, so a full SCAN has to
        // visit every master to find them all.
        let prefix = unique("cluster");
        let mut c = conn(&id, 0).await;
        let keys: Vec<String> = (0..30).map(|i| format!("{prefix}:{i}")).collect();
        for key in &keys {
            cmd("SET").arg(key).arg("x").exec_async(&mut c).await.expect("set");
        }
        let mut found = HashSet::new();
        let mut cursors = None;
        loop {
            let (next, page) = client
                .scan(cursors, &format!("{prefix}:*"), 10, false, None)
                .await
                .expect("scan");
            found.extend(page.into_iter().map(|(k, _, _)| k));
            if next.iter().sum::<u64>() == 0 {
                break;
            }
            cursors = Some(next);
        }
        assert_eq!(found.len(), keys.len(), "every key on every master");
        let total = client.dbsize().await.expect("dbsize");
        assert!(total >= keys.len() as u64, "dbsize sums the masters: {total}");

        let features = probe_server_features(&id, 0).await.expect("probe");
        assert_eq!(features.status(ServerCommand::ClusterInfo), CommandStatus::Available);
        assert_eq!(features.status(ServerCommand::Scan), CommandStatus::Available);

        for key in &keys {
            cmd("DEL").arg(key).exec_async(&mut c).await.expect("del");
        }
    });
}

// ── redis-stack ──────────────────────────────────────────────────────────

#[test]
#[ignore]
fn stack_modules_are_detected_and_usable() {
    smol::block_on(async {
        if env::var("ZEDIS_IT_STACK").is_err() {
            eprintln!("skipped: ZEDIS_IT_STACK not set");
            return;
        }
        let id = register(server("it-stack", standalone())).await;
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        assert!(client.supports_rejson(), "ReJSON must be listed by MODULE LIST");
        assert!(client.supports_search(), "RediSearch must be listed by MODULE LIST");

        let key = unique("json");
        let mut c = conn(&id, 0).await;
        cmd("JSON.SET")
            .arg(&key)
            .arg("$")
            .arg(r#"{"a":1}"#)
            .exec_async(&mut c)
            .await
            .expect("json.set");
        assert_eq!(client.key_type(&key).await.expect("type"), "ReJSON-RL");
        let listing = zedis_connection::ft_list(&mut c).await.expect("ft._list");
        assert!(!listing.unsupported);
        cmd("DEL").arg(&key).exec_async(&mut c).await.expect("del");
    });
}
