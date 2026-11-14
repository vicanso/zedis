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

//! Connecting and staying connected: what a connect learns, the link
//! dropping and coming back, the heartbeat's probe.

use super::*;

/// The same connection code works under a tokio runtime, which is what the
/// HTTP bridge will run on (ADR 9).
///
/// redis is built with both runtime adapters and picks per connection: tokio
/// when `Handle::try_current()` finds an ambient runtime, smol otherwise. Every
/// other test in this file runs outside one and therefore only ever proves the
/// smol half. This one proves the other, so the bridge's premise cannot rot
/// silently — a regression here surfaces as "there is no reactor running",
/// not as a subtly different code path.
#[test]
#[ignore]
fn standalone_connects_from_inside_a_tokio_runtime() {
    let (host, port) = skip_unless!("ZEDIS_IT_STANDALONE");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async {
        let id = register(server("it-tokio-runtime", (host, port))).await;
        let client = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("client built under tokio");
        client.ping().await.expect("ping under tokio");

        // A write and a read back, so this is a real round trip through the
        // tokio socket and not just a handshake.
        let key = unique("tokio-rt");
        let mut c = conn(&id, 0).await;
        cmd("SET")
            .arg(&key)
            .arg("through-tokio")
            .exec_async(&mut c)
            .await
            .expect("set under tokio");
        let value: String = cmd("GET").arg(&key).query_async(&mut c).await.expect("get under tokio");
        assert_eq!(value, "through-tokio");
        cmd("DEL").arg(&key).exec_async(&mut c).await.expect("cleanup");
    });
}

/// The troubleshooting report's snapshot (ADR 19): every part answers on a
/// plain server, read-only, and the settings come back in the report's
/// order, named.
#[test]
#[ignore]
fn standalone_troubleshooting_snapshot_reads_every_part() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let snapshot = troubleshooting_snapshot(&ServerDb::new(&id, 0)).await;
        let summary = snapshot.summary.as_ref().expect("who the server is");
        assert!(!summary.version.is_empty());
        let info = snapshot.info.as_ref().expect("INFO");
        assert_eq!(info.len(), 1, "one master");
        assert!(info[0].1.contains("used_memory:"), "the fullest INFO: {}", info[0].1);
        let doctor = snapshot.memory_doctor.as_ref().expect("MEMORY DOCTOR");
        assert!(doctor.first().is_some_and(|reply| !reply.value.trim().is_empty()));
        assert!(
            snapshot.latency_doctor.is_ok(),
            "{:?}",
            snapshot.latency_doctor.as_ref().err()
        );
        assert!(snapshot.latency.is_ok() && snapshot.slow_log.is_ok());
        let config = snapshot.config.as_ref().expect("CONFIG GET");
        let names: Vec<&str> = config.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, REPORT_CONFIG);
        assert!(
            config
                .iter()
                .any(|(name, value)| name == "maxmemory-policy" && value.is_some()),
            "{config:?}"
        );
    });
}

#[test]
#[ignore]
fn standalone_connect_reports_metadata() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        client.ping().await.expect("ping");
        assert!(!client.version().is_empty(), "version must be read from INFO server");
        // Which product answered, and in its own version. On the two this
        // matrix runs that is the version the floors are asked as well: Redis
        // has no second one, and Valkey's own is its column in `floors`.
        let description = client.nodes_description();
        if client.is_valkey() {
            assert_eq!(description.flavor, ServerFlavor::Valkey);
            assert_eq!(description.flavor_version, client.version());
        } else {
            assert_eq!(description.flavor, ServerFlavor::Redis);
            assert_eq!(description.flavor_version, "", "Redis names one version");
        }
        // The setup goes out as one pipeline of best-effort commands: one a
        // server refuses (SETINFO before 7.2) must not take the rest with it.
        let mut c = conn(&id, 0).await;
        let info: String = cmd("CLIENT")
            .arg("INFO")
            .query_async(&mut c)
            .await
            .expect("client info");
        assert!(info.contains("name=zedis:v"), "CLIENT SETNAME must apply: {info}");
        if supports(&id, floors::CLIENT_SETINFO).await {
            assert!(
                info.contains("lib-name=zedis") && info.contains("lib-ver="),
                "CLIENT SETINFO must name the client: {info}"
            );
        }
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
        let has_functions = supports(&id, floors::FUNCTIONS).await;
        let expect_functions = if has_functions {
            CommandStatus::Available
        } else {
            CommandStatus::Missing
        };
        assert_eq!(features.status(ServerCommand::FunctionList), expect_functions);
        // Mutating commands are never executed: on 7+ ACL DRYRUN says
        // Available, on 6.x COMMAND INFO proves existence and the status
        // stays optimistic — either way they must not read as unusable.
        for c in [
            ServerCommand::Monitor,
            ServerCommand::Bgsave,
            ServerCommand::FlushDb,
            ServerCommand::FlushAll,
        ] {
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

/// An ACL user who may read but not run `INFO` is a working connection, not
/// an offline one. `INFO` is `@slow @dangerous`, outside `+@read`, and the
/// app asked it to connect (for the version) and on every beat (as the
/// probe): the connect failed, the heartbeat failed, and a user who could
/// read every key saw an empty tree under "Offline", retried for ever.
#[test]
#[ignore]
fn standalone_user_without_info_connects_and_beats() {
    smol::block_on(async {
        let admin_id = register(server("it-noinfo-admin", standalone())).await;
        let mut admin = conn(&admin_id, 0).await;
        let user = unique("zedis_it_noinfo");
        let key = unique("it:noinfo");
        cmd("ACL")
            .arg("SETUSER")
            .arg(&user)
            .arg("on")
            .arg(">pw")
            .arg("~*")
            .arg("+@read")
            .arg("+ping")
            .arg("+select")
            .arg("+hello")
            .arg("+auth")
            .arg("+client|setname")
            .arg("+client|setinfo")
            .arg("+scan")
            .arg("+dbsize")
            .exec_async(&mut admin)
            .await
            .expect("setuser");
        cmd("SET").arg(&key).arg("v").exec_async(&mut admin).await.expect("set");

        let mut limited = server(&unique("it-noinfo"), standalone());
        limited.username = Some(user.clone());
        limited.password = Some("pw".into());
        let id = register(limited).await;
        let at = ServerDb::new(&id, 0);

        // It connects, with what it could not ask left unknown.
        let summary = server_summary(&at).await.expect("connects without INFO");
        assert!(summary.dbsize >= 1, "{summary:?}");
        assert_eq!(summary.version, "0.0.0", "the version is INFO's to tell");
        // The beat judges the link by PING, and says there is no INFO text.
        assert_eq!(heartbeat_probe(&at).await.expect("the link answers"), None);
        // And it reads: the type and TTL of a key, the key itself.
        let (key_type, ttl) = key_type_and_ttl(&at, &key).await.expect("type + ttl");
        assert_eq!((key_type.as_str(), ttl), ("string", -1));

        cmd("DEL").arg(&key).exec_async(&mut admin).await.expect("del");
        cmd("ACL")
            .arg("DELUSER")
            .arg(&user)
            .exec_async(&mut admin)
            .await
            .expect("deluser");
    });
}

/// A port nobody listens on is an unreachable host, and stays one when the
/// classifier is asked about TLS: the hint is for a link that was accepted
/// and then dropped, and the app showed "TLS may be required" for a server
/// that was simply not running.
#[test]
#[ignore]
fn a_refused_connect_is_unreachable_not_a_tls_hint() {
    smol::block_on(async {
        // A port that was free a moment ago: bound, read, released.
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
            listener.local_addr().expect("its address").port()
        };
        let id = register(server("it-refused", ("127.0.0.1".to_string(), port))).await;
        let err = match get_connection_manager().get_client(&id, 0).await {
            Ok(_) => panic!("nothing listens on {port}"),
            Err(err) => err,
        };
        assert_eq!(err.connection_kind(), ConnectionErrorKind::Network, "{err}");
        assert_eq!(
            err.connection_kind_tls_aware(false),
            ConnectionErrorKind::Network,
            "{err}"
        );
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

/// The terminal's connection (`open_dedicated_connection`) shares nothing
/// with the pooled one: a `SELECT` typed there must not move the db the key
/// tree scans on. Before this existed the terminal ran on the pooled
/// connection, and `SELECT 3` silently redirected every later `SCAN`.
#[test]
#[ignore]
fn standalone_dedicated_connection_keeps_select_to_itself() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let mut pooled = conn(&id, 0).await;
        let mut dedicated = get_connection_manager()
            .open_dedicated_connection(&id, 0)
            .await
            .expect("dedicated connection");

        // Two sockets, not two handles onto one.
        let pooled_id: i64 = cmd("CLIENT")
            .arg("ID")
            .query_async(&mut pooled)
            .await
            .expect("client id");
        let dedicated_id: i64 = cmd("CLIENT")
            .arg("ID")
            .query_async(&mut dedicated)
            .await
            .expect("client id");
        assert_ne!(
            pooled_id, dedicated_id,
            "the dedicated connection must be its own client"
        );

        let _: () = cmd("SELECT")
            .arg(1)
            .query_async(&mut dedicated)
            .await
            .expect("select 1");
        let key = unique("dedicated");
        let _: () = cmd("SET")
            .arg(&key)
            .arg("1")
            .arg("EX")
            .arg(60)
            .query_async(&mut dedicated)
            .await
            .expect("set on db 1");

        // The dedicated connection stayed on db 1 …
        let on_dedicated: bool = cmd("EXISTS")
            .arg(&key)
            .query_async(&mut dedicated)
            .await
            .expect("exists");
        assert!(
            on_dedicated,
            "the SELECT must hold for later commands on the same connection"
        );
        // … and the pooled one never left db 0.
        let on_pooled: bool = cmd("EXISTS").arg(&key).query_async(&mut pooled).await.expect("exists");
        assert!(
            !on_pooled,
            "a SELECT on the dedicated connection leaked into the pooled one"
        );
        let mut pooled_again = conn(&id, 0).await;
        let on_pooled_again: bool = cmd("EXISTS")
            .arg(&key)
            .query_async(&mut pooled_again)
            .await
            .expect("exists");
        assert!(
            !on_pooled_again,
            "the cached client must still hand out a db-0 connection"
        );

        let _: () = cmd("DEL").arg(&key).query_async(&mut dedicated).await.expect("cleanup");
    });
}

/// The heartbeat's probe: with one master the `INFO` is the probe, on the
/// client's own connection, so a beat is one command; a cluster still probes
/// with `PING` and leaves the per-master `INFO` to the fan-out.
#[test]
#[ignore]
fn heartbeat_probe_is_the_info_itself_where_there_is_one_master() {
    smol::block_on(async {
        let id = register(server("it-heartbeat-standalone", standalone())).await;
        let client = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("standalone client");
        let info = client
            .heartbeat_probe()
            .await
            .expect("probe")
            .expect("one master: the probe carries the INFO");
        assert!(info.contains("redis_version:"), "not an INFO reply: {info:.60}");
        // The same text the fan-out would have fetched, so the parser behind
        // the status bar is fed what it always was.
        let (_, fanned): (_, Vec<String>) = client.query_async_masters(vec![cmd("INFO")]).await.expect("fan-out");
        assert_eq!(fanned.len(), 1);
        let section = |text: &str| {
            text.lines()
                .filter(|l| l.starts_with('#'))
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        assert_eq!(section(&info), section(&fanned[0]), "the two INFOs differ in shape");

        // A Sentinel entry's client is the data master: one master as well.
        if let Some(addr) = scenario("ZEDIS_IT_SENTINEL") {
            let id = register(sentinel_server("it-heartbeat-sentinel", addr)).await;
            let client = get_connection_manager()
                .get_client(&id, 0)
                .await
                .expect("sentinel client");
            let info = client
                .heartbeat_probe()
                .await
                .expect("probe")
                .expect("the master's INFO");
            assert!(info.contains("role:master"), "the probe did not reach the master");
        }

        let Some(addr) = scenario("ZEDIS_IT_CLUSTER") else {
            eprintln!("skipped the cluster half: ZEDIS_IT_CLUSTER not set");
            return;
        };
        let id = register(protected_server("it-heartbeat-cluster", addr)).await;
        let client = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("cluster client");
        assert!(
            client.heartbeat_probe().await.expect("probe").is_none(),
            "a cluster's bare INFO answers for one arbitrary node; the probe has to stay a PING"
        );
    });
}

#[test]
#[ignore]
fn standalone_resp3_negotiates_hello_3() {
    smol::block_on(async {
        let mut srv = server("it-resp3", standalone());
        srv.resp3 = Some(true);
        let id = register(srv).await;
        let client = get_connection_manager().get_client(&id, 0).await.expect("resp3 client");
        let mut conn = client.connection();
        let hello: redis::Value = cmd("HELLO").query_async(&mut conn).await.expect("hello");
        let fields = zedis_connection::reply::pairs(&hello).expect("resp3 hello map");
        let proto = fields
            .iter()
            .find(|(k, _)| k == "proto")
            .and_then(|(_, v)| zedis_connection::reply::int(v));
        assert_eq!(proto, Some(3), "expected RESP3 protocol negotiation: {fields:?}");
    });
}
