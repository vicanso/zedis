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

//! Sentinel: resolving the master, following a failover, the sentinels'
//! own commands and credentials.

use super::*;

/// `SENTINEL GET-MASTER-ADDR-BY-NAME` straight from the sentinel.
async fn sentinel_master_port(sentinel: &mut RedisAsyncConn, master_name: &str) -> u16 {
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
        // A direct dial of the sentinel: `sentinel_login()` swaps in the
        // credentials the sentinel itself wants, which are not the data
        // nodes'.
        let raw = sentinel_server("it-sentinel-raw", addr.clone()).sentinel_login();
        let mut sentinel = open_single_connection(&raw, 0, false)
            .await
            .expect("sentinel connection");
        let before = sentinel_master_port(&mut sentinel, &master_name).await;

        // Seeds are walked in order: a dead first address must not stop
        // discovery — the real sentinel is the second entry.
        let (sentinel_host, sentinel_port) = addr.clone();
        let mut s = sentinel_server(
            "it-sentinel",
            (format!("127.0.0.1:1, {sentinel_host}:{sentinel_port}"), sentinel_port),
        );
        s.master_name = Some(master_name.clone());
        assert_eq!(s.seed_endpoints().len(), 2);
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
        // A data node, so the data password — not the sentinel's.
        let demoted_server = protected_server("it-demoted", ("127.0.0.1".into(), before));
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

/// The admin commands go to the sentinels, never to the pooled data master
/// (where `SENTINEL` is unknown): MASTERS lists both monitored masters,
/// CKQUORUM answers per sentinel, SET changes what MASTERS reports next,
/// MONITOR / REMOVE add and drop a master, FLUSHCONFIG rewrites the file.
#[test]
#[ignore]
fn sentinel_admin_commands_reach_the_sentinels() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_SENTINEL");
        let master_name = env::var("ZEDIS_IT_MASTER_NAME").unwrap_or_else(|_| "mymaster".into());
        let mut s = sentinel_server("it-sentinel-admin", addr);
        s.master_name = Some(master_name.clone());
        let id = register(s).await;
        let server = get_server(&id).expect("saved entry");

        let masters = sentinel_masters(&server).await.expect("masters");
        assert!(masters.len() >= 2, "two monitored masters: {masters:?}");
        let replicas = sentinel_replicas(&server, &master_name).await.expect("replicas");
        assert!(!replicas.is_empty(), "sentinel reports replicas: {replicas:?}");
        let before = masters
            .iter()
            .find(|m| m.name == master_name)
            .cloned()
            .expect("the named master is listed");
        assert_eq!(before.quorum, 1);

        // One sentinel with quorum 1: from where it stands, a failover is possible.
        let replies = sentinel_ckquorum(&server, &master_name).await.expect("ckquorum");
        assert_eq!(replies.len(), 1, "one sentinel in the topology: {replies:?}");
        let reply = replies[0].result.as_ref().expect("ckquorum answers");
        assert!(reply.starts_with("OK"), "{reply}");

        // SET is visible in the next MASTERS; put the value back afterwards.
        let option = "down-after-milliseconds".to_string();
        let replies = sentinel_set(&server, &master_name, &[(option.clone(), "6000".to_string())])
            .await
            .expect("set");
        assert!(replies.iter().all(|r| r.result.is_ok()), "{replies:?}");
        let after = sentinel_masters(&server)
            .await
            .expect("masters")
            .into_iter()
            .find(|m| m.name == master_name)
            .expect("still listed");
        assert_eq!(after.down_after_ms, 6000);
        sentinel_set(&server, &master_name, &[(option, before.down_after_ms.to_string())])
            .await
            .expect("set back");

        // MONITOR a throwaway master (an address nothing answers on is fine:
        // the command only records it), then REMOVE it again.
        let tmp = unique("snt").replace(':', "-");
        let replies = sentinel_monitor(&server, &tmp, "127.0.0.1", 1, 1)
            .await
            .expect("monitor");
        assert!(replies.iter().all(|r| r.result.is_ok()), "{replies:?}");
        assert!(
            sentinel_masters(&server)
                .await
                .expect("masters")
                .iter()
                .any(|m| m.name == tmp)
        );
        let replies = sentinel_remove(&server, &tmp).await.expect("remove");
        assert!(replies.iter().all(|r| r.result.is_ok()), "{replies:?}");
        assert!(
            !sentinel_masters(&server)
                .await
                .expect("masters")
                .iter()
                .any(|m| m.name == tmp)
        );

        let replies = sentinel_flushconfig(&server).await.expect("flushconfig");
        assert!(replies.iter().all(|r| r.result.is_ok()), "{replies:?}");
    });
}

/// An entry that names no master on a sentinel with several connects to
/// the first by name and carries the whole list for the Topology switcher;
/// naming one the sentinel does not monitor fails and says what it does.
/// The sentinel's own credentials, which the harness deliberately makes
/// different from the data nodes'. Three shapes, all of them real:
///
/// * type declared as Sentinel → `open_seed_endpoint` takes
///   `sentinel_login()` straight to the seed;
/// * type on auto (what the connection dialog writes by default) → the
///   data password is refused by the sentinel and the retry finds the
///   sentinel's;
/// * no sentinel credentials at all → nothing to retry with, and the
///   failure has to arrive as an auth error rather than a hang or a
///   "seed unreachable".
#[test]
#[ignore]
fn sentinel_uses_its_own_credentials_for_the_sentinels() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_SENTINEL");
        let master_name = env::var("ZEDIS_IT_MASTER_NAME").unwrap_or_else(|_| "mymaster".into());
        let Some(sentinel_password) = env::var("ZEDIS_IT_SENTINEL_PASSWORD").ok().filter(|p| !p.is_empty()) else {
            eprintln!("skipped: ZEDIS_IT_SENTINEL_PASSWORD not set");
            return;
        };
        assert_ne!(
            Some(&sentinel_password),
            data_password().as_ref(),
            "the harness must give the sentinel a different password, or this proves nothing"
        );

        for mut entry in [
            sentinel_declared_server("it-sentinel-auth-declared", addr.clone()),
            sentinel_server("it-sentinel-auth-auto", addr.clone()),
        ] {
            entry.master_name = Some(master_name.clone());
            assert!(entry.has_sentinel_credentials());
            let id = register(entry).await;
            get_connection_manager().remove_client(&id, 0);
            let client = get_connection_manager()
                .get_client(&id, 0)
                .await
                .unwrap_or_else(|e| panic!("{id}: {e}"));
            assert_eq!(client.nodes_description().server_type, "Sentinel");
            // The data password is the one that has to reach the master.
            client.ping().await.unwrap_or_else(|e| panic!("{id} ping: {e}"));
        }

        // Only the data credentials: the sentinel refuses them, and the
        // legacy "retry without a password" fallback cannot help against a
        // sentinel that wants one.
        let mut blind = server("it-sentinel-auth-missing", addr);
        blind.master_name = Some(master_name);
        blind.password = data_password();
        assert!(!blind.has_sentinel_credentials());
        let id = register(blind).await;
        get_connection_manager().remove_client(&id, 0);
        let err = get_connection_manager()
            .get_client(&id, 0)
            .await
            .err()
            .expect("a sentinel password is required here");
        assert_eq!(err.connection_kind(), ConnectionErrorKind::Auth, "{err}");
    });
}

#[test]
#[ignore]
fn sentinel_without_a_master_name_takes_the_first_and_lists_all() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_SENTINEL");
        let master_name = env::var("ZEDIS_IT_MASTER_NAME").unwrap_or_else(|_| "mymaster".into());
        let Ok(second) = env::var("ZEDIS_IT_MASTER_NAME2") else {
            eprintln!("skipped: ZEDIS_IT_MASTER_NAME2 not set");
            return;
        };
        let id = register(sentinel_server("it-sentinel-unnamed", addr.clone())).await;
        get_connection_manager().remove_client(&id, 0);
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        let desc = client.nodes_description();
        assert_eq!(desc.server_type, "Sentinel");
        let mut expected = vec![master_name, second];
        expected.sort();
        // `sentinel_admin_commands_reach_the_sentinels` MONITORs a throwaway
        // `zedis-it-…` master for a moment; the suite runs in parallel, so it
        // may be listed here too — it sorts after the real ones either way.
        let listed: Vec<String> = desc
            .sentinel_master_names
            .iter()
            .filter(|name| !name.starts_with("zedis-it-"))
            .cloned()
            .collect();
        assert_eq!(listed, expected);
        assert_eq!(client.master_servers().len(), 1, "connected to one master");
        assert_eq!(desc.topology[0].master.master_name, expected[0], "the first by name");

        let mut s = sentinel_server("it-sentinel-unknown", addr);
        s.master_name = Some("no-such-master".into());
        let id = register(s).await;
        get_connection_manager().remove_client(&id, 0);
        let err = get_connection_manager()
            .get_client(&id, 0)
            .await
            .err()
            .expect("an unmonitored master name must fail the connect");
        assert!(err.to_string().contains("no-such-master"), "{err}");
    });
}
