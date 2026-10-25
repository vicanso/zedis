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

//! The server as a whole: its configuration, the administration panels,
//! clients, persistence and the observability reads.

use super::*;

/// The TimeSeries writes the chart panel gained. RedisTimeSeries only, so
/// this runs on the stack lane and skips loudly everywhere else.
/// `CONFIG` through the operations the config editor calls. It changes one
/// parameter nothing else depends on, and puts it back.
#[test]
#[ignore]
fn standalone_config_is_read_set_and_compared() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let at = ServerDb::new(&id, 0);

        let loaded = config_load(&at).await.expect("config load");
        assert!(
            loaded.params.len() > 50,
            "CONFIG GET * returned {} parameters",
            loaded.params.len()
        );
        assert!(loaded.params.windows(2).all(|w| w[0].0 <= w[1].0), "sorted by name");

        // Each alias once, under the modern spelling, the others kept beside
        // it — and nothing the server answered is lost (#202).
        let raw: HashMap<String, String> = cmd("CONFIG")
            .arg("GET")
            .arg("*")
            .query_async(&mut conn(&id, 0).await)
            .await
            .expect("raw CONFIG GET *");
        let listed: HashSet<&str> = loaded
            .params
            .iter()
            .map(|(name, _)| name.as_str())
            .chain(loaded.aliases.values().flatten().map(String::as_str))
            .collect();
        for name in raw.keys() {
            assert!(listed.contains(name.as_str()), "{name} is neither listed nor an alias");
        }
        let names: HashSet<&str> = loaded.params.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names.len(), loaded.params.len(), "a name listed twice");
        // Redis 7+ and Valkey answer with both names; older servers with one.
        if raw.contains_key("slave-read-only") && raw.contains_key("replica-read-only") {
            assert!(names.contains("replica-read-only") && !names.contains("slave-read-only"));
            assert_eq!(loaded.aliases["replica-read-only"], ["slave-read-only"]);
            // The name it is listed under is one the server takes.
            let value = loaded
                .params
                .iter()
                .find(|(name, _)| name == "replica-read-only")
                .map(|(_, value)| value.clone())
                .expect("listed");
            config_set(&at, "replica-read-only", &value)
                .await
                .expect("set by its listed name");
        }

        let map = config_get_all(&at).await.expect("config get all");
        assert_eq!(
            map.len(),
            loaded.params.len(),
            "the list and the map are the same reply"
        );

        // `slowlog-max-len` is a plain integer no other test reads — listed
        // under its own name, or on Valkey 9 as an alias of
        // `commandlog-slow-execution-max-len`.
        let name = loaded
            .params
            .iter()
            .map(|(name, _)| name.as_str())
            .find(|name| {
                *name == "slowlog-max-len"
                    || loaded
                        .aliases
                        .get(*name)
                        .is_some_and(|aliases| aliases.iter().any(|alias| alias == "slowlog-max-len"))
            })
            .expect("slowlog-max-len is listed");
        let before = map.get(name).cloned().expect("listed in the map too");
        let changed = if before == "137" { "138" } else { "137" };
        config_set(&at, name, changed).await.expect("config set");
        assert_eq!(
            config_get_all(&at).await.expect("reread").get(name).map(String::as_str),
            Some(changed)
        );
        config_set(&at, name, &before).await.expect("restore");
        // A value the server refuses is an error, not a silent no-op.
        assert!(config_set(&at, name, "not-a-number").await.is_err());
        assert!(config_set(&at, "zedis-no-such-parameter", "1").await.is_err());

        // REWRITE needs a config file: with one it succeeds, without one the
        // server says so — and `config_file` is what told the editor which.
        let rewritten = config_rewrite(&at).await;
        assert_eq!(
            rewritten.is_ok(),
            !loaded.config_file.is_empty(),
            "config_file = {:?}, rewrite = {rewritten:?}",
            loaded.config_file
        );
    });
}

/// The small reads and resets behind the persistence, load, INFO and latency
/// panels — each was one hand-built command inside its view.
#[test]
#[ignore]
fn standalone_admin_panels_read_and_reset_through_their_operations() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let at = ServerDb::new(&id, 0);

        // CONFIG GET by name: a value for what exists, `None` for what does
        // not, in the order asked.
        let values = config_get_named(&at, &["appendonly", "zedis-no-such-parameter", "dbfilename"])
            .await
            .expect("config get named");
        assert_eq!(values.len(), 3);
        assert!(
            matches!(values[0].as_deref(), Some("yes" | "no")),
            "appendonly = {:?}",
            values[0]
        );
        assert_eq!(values[1], None);
        assert!(
            values[2].as_deref().is_some_and(|name| !name.is_empty()),
            "dbfilename = {:?}",
            values[2]
        );

        // The fullest INFO the server gives, one entry per master.
        let info = info_everything(&at).await.expect("info everything");
        assert_eq!(info.len(), 1);
        assert!(info[0].0.contains(':'), "labelled host:port, got {:?}", info[0].0);
        assert!(info[0].1.contains("redis_version:"));
        // `everything` / `all` carry what the plain INFO leaves out.
        assert!(info[0].1.contains("# Commandstats"), "the full listing was not reached");

        config_resetstat(&at).await.expect("config resetstat");

        // The Server Load page's sample: a row per command, in name order.
        // It came out of a hash map, in a new order on every call, and rows
        // the page's sort tied reshuffled with each refresh (#199).
        let mut probe = conn(&id, 0).await;
        let _: Option<String> = cmd("GET")
            .arg("it:command-stats")
            .query_async(&mut probe)
            .await
            .expect("get");
        let stats = command_stats(&at).await.expect("command stats");
        let names: Vec<&str> = stats.iter().map(|stat| stat.name.as_str()).collect();
        assert!(
            names.windows(2).all(|pair| pair[0] < pair[1]),
            "in name order: {names:?}"
        );
        assert!(
            stats.iter().any(|stat| stat.name == "get" && stat.calls >= 1),
            "the GET just sent is counted: {names:?}"
        );

        // LATENCY: the listing parses whether or not monitoring is on, a
        // reset answers how many events it cleared, and the threshold is what
        // CONFIG says.
        let listing = latency_latest(&at).await.expect("latency latest");
        assert!(!listing.unsupported, "a real Redis has LATENCY");
        let threshold = latency_monitor_threshold(&at).await.expect("threshold");
        assert_eq!(
            config_get_named(&at, &["latency-monitor-threshold"])
                .await
                .expect("named")[0],
            Some(threshold.to_string())
        );
        latency_reset(&at, &[]).await.expect("latency reset");
        assert!(
            latency_latest(&at).await.expect("after reset").events.is_empty(),
            "reset left events behind"
        );
        assert!(latency_history(&at, "command").await.expect("history").is_empty());

        // The mean beside latest and max: Valkey 8.1+ keeps a sum and a
        // count in LATENCY LATEST, Redis does not and the mean of HISTORY
        // stands in. A Lua loop of a few milliseconds makes the event —
        // counted, not clocked: TIME inside a script reads the event loop's
        // cached clock, which stands still until the busy threshold. Short,
        // because the other tests share this server and a command that
        // lands mid-script waits on it.
        let threshold_before = threshold.to_string();
        config_set(&at, "latency-monitor-threshold", "1")
            .await
            .expect("threshold on");
        let mut slow = conn(&id, 0).await;
        let _: i64 = cmd("EVAL")
            .arg("local x = 0; for i = 1, 3000000 do x = x + i end; return 1")
            .arg(0)
            .query_async(&mut slow)
            .await
            .expect("a slow script");
        let listing = latency_latest(&at).await.expect("latency latest");
        let command = listing
            .events
            .iter()
            .find(|event| event.event == "command")
            .expect("the slow script made a command event");
        let mean = command.avg_ms.expect("a mean either way");
        assert!(mean >= 1.0 && command.avg_samples >= 1, "{command:?}");
        assert_eq!(
            command.avg_exact,
            supports(&id, floors::LATENCY_STATS).await,
            "the server's own sum on Valkey 8.1+, HISTORY's mean elsewhere: {command:?}"
        );
        config_set(&at, "latency-monitor-threshold", &threshold_before)
            .await
            .expect("threshold back");
        latency_reset(&at, &[]).await.expect("latency reset");

        // The slow log through the panel's operation, and its reset.
        command_log_reset(&at, CommandLogKind::Slow)
            .await
            .expect("slowlog reset");
        let logs = command_logs(&at, CommandLogKind::Slow).await.expect("slow log");
        assert!(logs.len() <= 1, "just reset — at most the reset itself: {}", logs.len());
    });
}

/// `CONFIG SET` only changes the running configuration; `CONFIG REWRITE`
/// is what makes it survive a restart, and it needs the server to have been
/// started with a config file. The harness starts this one from command-line
/// arguments alone, which is exactly the case the config editor has to
/// detect — it reads `config_file` from `INFO server` and, when empty, says
/// the edits are runtime-only instead of offering a button that can only
/// fail.
#[test]
#[ignore]
fn standalone_without_a_config_file_cannot_rewrite_it() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let mut c = conn(&id, 0).await;
        let info: String = cmd("INFO")
            .arg("server")
            .query_async(&mut c)
            .await
            .expect("info server");
        let config_file = info
            .lines()
            .find_map(|line| line.trim().strip_prefix("config_file:"))
            .expect("INFO server reports config_file")
            .trim();
        assert!(
            config_file.is_empty(),
            "the harness starts this server without a config file: {config_file}"
        );
        let rewritten: Result<String, _> = cmd("CONFIG").arg("REWRITE").query_async(&mut c).await;
        assert!(
            rewritten.is_err(),
            "CONFIG REWRITE needs a config file — that is why the button is hidden"
        );
    });
}

/// The server-wide operations behind the status bar and the admin panels:
/// what a connect learns, what a heartbeat asks, and the forks.
#[test]
#[ignore]
fn standalone_server_operations_describe_and_administer_the_server() {
    smol::block_on(async {
        let id = register(server("it-server-ops", standalone())).await;
        let at = ServerDb::new(&id, 0);

        let summary = server_summary(&at).await.expect("summary");
        assert!(!summary.version.is_empty());
        // Replica count is not asserted: `replication_pair_…` attaches one to
        // a server of its own, and the suite runs in parallel.
        assert_eq!(summary.nodes.0, 1, "a standalone is one master");
        assert_eq!(summary.description.server_type, "Standalone");
        assert!(summary.databases >= 1);
        // Not compared against `summary.dbsize`: the suite runs in parallel
        // against this server, so two reads of a live counter differ.
        dbsize(&at).await.expect("dbsize");
        assert!(server_supports(&at, floors::MEMORY_USAGE).await.expect("floor"));

        // The heartbeat: with one master the probe *is* the INFO.
        let probed = heartbeat_probe(&at).await.expect("probe");
        let info = probed.clone().expect("a standalone answers the INFO itself");
        assert!(
            info.contains("redis_version:") || info.contains("valkey_version:"),
            "{info:.60}"
        );
        // Which is then not asked for a second time.
        let infos = master_infos(&at, probed).await.expect("infos");
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].1, info);
        // Without one, it is fetched per master.
        let fetched = master_infos(&at, None).await.expect("infos");
        assert_eq!(fetched.len(), 1);
        assert!(fetched[0].1.contains("# Server"));
        assert!(slow_logs(&at).await.is_ok(), "the slow log is sampled with the beat");

        // The forks. The reply is a status line that differs between forks
        // and is not read; what is checked is that the server took it. In
        // this order: an AOF rewrite asked for while a BGSAVE runs is
        // *scheduled*, while a BGSAVE asked for during a rewrite is refused.
        // A save another test started may still be running — the BGSAVE
        // CANCEL test's on Valkey 8.1+, whose child is reaped some time after
        // the cancel — and a second one is refused as "already in progress"
        // until it is gone; ours goes in then.
        let mut started = bgsave(&at).await;
        for _ in 0..100 {
            match &started {
                Err(e) if e.to_string().contains("already in progress") => {
                    smol::Timer::after(std::time::Duration::from_millis(100)).await;
                    started = bgsave(&at).await;
                }
                _ => break,
            }
        }
        started.expect("bgsave");
        bgrewriteaof(&at).await.expect("bgrewriteaof");
        let saved = master_infos(&at, None).await.expect("infos");
        assert!(
            saved[0].1.contains("rdb_last_bgsave_status"),
            "the persistence panel reads this back"
        );

        // Dropping the pooled client sends nothing and costs nothing: the
        // next call rebuilds it, which is how a failover is followed.
        forget_client(&at);
        assert_eq!(server_summary(&at).await.expect("reconnect").nodes.0, 1);
    });
}

/// What the Slow Log tab prints above its table: the server's own
/// `slowlog-log-slower-than` and `slowlog-max-len`, as `CONFIG GET` answers
/// them (on Valkey 8.1+ through the aliases of the `COMMANDLOG` names).
#[test]
#[ignore]
fn standalone_slow_log_settings_are_the_servers_own() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let at = ServerDb::new(&id, 0);
        let settings = slowlog_settings(&at).await.expect("slow log settings");
        let raw = |name: &'static str| {
            let id = id.clone();
            async move {
                let pair: Vec<String> = cmd("CONFIG")
                    .arg("GET")
                    .arg(name)
                    .query_async(&mut conn(&id, 0).await)
                    .await
                    .expect("raw CONFIG GET");
                pair.into_iter().nth(1).expect("a value")
            }
        };
        assert_eq!(
            settings.slower_than_us.map(|us| us.to_string()),
            Some(raw("slowlog-log-slower-than").await)
        );
        assert_eq!(
            settings.max_len.map(|n| n.to_string()),
            Some(raw("slowlog-max-len").await)
        );
        assert!(!settings.is_unknown());
    });
}

/// The Live Monitor's own-commands filter: the connections Zedis holds are
/// found by the name `CLIENT SETNAME` gave them, the listing's own pooled
/// connection among them, and a client under another name is not.
#[test]
#[ignore]
fn standalone_lists_its_own_connections_by_name() {
    smol::block_on(async {
        let (host, port) = standalone();
        let entry = server("it-standalone", (host.clone(), port));
        let id = register(entry.clone()).await;
        let at = ServerDb::new(&id, 0);
        // A connection of ours under someone else's name: the application
        // under study, as far as the filter can tell.
        let mut foreign = open_single_connection(&entry, 0, false).await.expect("a connection");
        let _: () = cmd("CLIENT")
            .arg("SETNAME")
            .arg("someone-else")
            .query_async(&mut foreign)
            .await
            .expect("setname");
        let foreign_id: i64 = cmd("CLIENT")
            .arg("ID")
            .query_async(&mut foreign)
            .await
            .expect("client id");

        let own = zedis_client_addrs(&at).await.expect("own clients");
        let node = format!("{host}:{port}");
        assert!(!own.is_empty(), "the pooled connection that asked is named zedis");
        assert!(own.iter().all(|(n, _)| *n == node), "{own:?}");

        let listing = client_list(&at).await.expect("client list");
        let (_, clients) = &listing[0];
        let addr_of = |id: i64| {
            clients
                .lines()
                .find(|line| line.split_whitespace().any(|f| f == format!("id={id}")))
                .and_then(|line| line.split_whitespace().find_map(|f| f.strip_prefix("addr=")))
                .expect("listed with an address")
                .to_string()
        };
        assert!(
            !own.contains(&(node.clone(), addr_of(foreign_id))),
            "another name is another application"
        );
        for line in clients.lines() {
            let named_zedis = line
                .split_whitespace()
                .any(|f| f.strip_prefix("name=").is_some_and(is_zedis_client_name));
            let addr = line
                .split_whitespace()
                .find_map(|f| f.strip_prefix("addr="))
                .expect("addr")
                .to_string();
            assert_eq!(own.contains(&(node.clone(), addr)), named_zedis, "{line}");
        }
    });
}

/// `CLIENT PAUSE` spelled per version (a mode word from 6.2), lifted by
/// `UNPAUSE` where it exists; a filtered `CLIENT KILL` composed the way
/// the panel composes it takes exactly the connection it names, and a
/// `MAXAGE` no client reaches takes none.
#[test]
#[ignore]
fn standalone_client_pause_and_filtered_kill() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let entry = get_server(&id).expect("saved entry");
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        let mut c = conn(&id, 0).await;

        // Through the operations the Clients panel calls.
        let at = ServerDb::new(&id, 0);
        let mode_supported = client.supports(floors::CLIENT_PAUSE_WRITE);
        // Without UNPAUSE (pre-6.2) the pause has to run out on its own —
        // keep it short so the other tests on this server barely notice.
        let ms = if mode_supported { 5000 } else { 50 };
        client_pause(&at, ms, PauseMode::Write, mode_supported)
            .await
            .expect("pause");
        if mode_supported {
            client_unpause(&at).await.expect("unpause");
        } else {
            smol::Timer::after(std::time::Duration::from_millis(100)).await;
        }
        let key = unique("pause");
        cmd("SET")
            .arg(&key)
            .arg("after")
            .exec_async(&mut c)
            .await
            .expect("a write after the pause");
        cmd("DEL").arg(&key).exec_async(&mut c).await.expect("del");

        // A victim connection, found by its id in the listing, killed by ADDR.
        let mut victim = open_single_connection(&entry, 0, false)
            .await
            .expect("victim connection");
        let victim_id: i64 = cmd("CLIENT")
            .arg("ID")
            .query_async(&mut victim)
            .await
            .expect("client id");
        let listing = client_list(&at).await.expect("client list");
        assert_eq!(listing.len(), 1, "a standalone is one node");
        let (node, clients) = &listing[0];
        assert_eq!((node.host.as_str(), node.port), (entry.host.as_str(), entry.port));
        let addr = clients
            .lines()
            .find(|line| line.split_whitespace().any(|f| f == format!("id={victim_id}")))
            .and_then(|line| line.split_whitespace().find_map(|f| f.strip_prefix("addr=")))
            .expect("the victim is listed with an address")
            .to_string();
        let filter = KillFilter {
            addr: Some(addr),
            skipme: true,
            ..Default::default()
        };
        let commands = kill_filter_commands(&filter);
        assert_eq!(commands.len(), 1);
        assert_eq!(
            client_kill_by(&at, &commands).await.expect("kill by addr"),
            1,
            "exactly the victim"
        );
        let after: Result<String, redis::RedisError> = cmd("PING").query_async(&mut victim).await;
        assert!(after.is_err(), "the victim's connection is gone");

        // The row's own Kill button: by id, on the node that listed it.
        let mut second = open_single_connection(&entry, 0, false).await.expect("second victim");
        let second_id: i64 = cmd("CLIENT")
            .arg("ID")
            .query_async(&mut second)
            .await
            .expect("client id");
        let second_id = second_id.to_string();
        assert!(client_kill_id(&at, node, &second_id).await.expect("kill by id"));
        let after: Result<String, redis::RedisError> = cmd("PING").query_async(&mut second).await;
        assert!(after.is_err(), "the second victim's connection is gone");
        // Already gone: the filter form of CLIENT KILL answers 0, not an error.
        assert!(
            !client_kill_id(&at, node, &second_id)
                .await
                .expect("kill a client that left")
        );

        if client.supports(floors::CLIENT_KILL_MAXAGE) {
            let filter = KillFilter {
                maxage_secs: Some(100_000_000),
                skipme: true,
                ..Default::default()
            };
            let killed = client_kill_by(&at, &kill_filter_commands(&filter))
                .await
                .expect("kill by maxage");
            assert_eq!(killed, 0, "no client is that old");
        }
    });
}

/// `CONFIG GET name` → the value.
async fn config_value(c: &mut RedisAsyncConn, name: &str) -> String {
    let pair: Vec<String> = cmd("CONFIG")
        .arg("GET")
        .arg(name)
        .query_async(c)
        .await
        .expect("CONFIG GET");
    pair.get(1).cloned().expect("CONFIG GET answers name, value")
}

async fn set_config_raw(c: &mut RedisAsyncConn, name: &str, value: &str) {
    let _: String = cmd("CONFIG")
        .arg("SET")
        .arg(name)
        .arg(value)
        .query_async(c)
        .await
        .expect("CONFIG SET");
}

/// The slow log through the COMMANDLOG door on every server, and — on
/// Valkey 8.1+ — the two size logs: a request and a reply over lowered
/// thresholds land in their logs with their sizes, and a reset clears one
/// log without touching the other.
#[test]
#[ignore]
fn commandlog_lists_slow_and_oversized_commands() {
    smol::block_on(async {
        let id = register(server("it-commandlog", standalone())).await;
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        let mut c = conn(&id, 0).await;
        let key = unique("commandlog");
        let is_set_of_key = |entry: &zedis_connection::SlowLogEntry, command: &str| {
            entry.args.first().is_some_and(|a| a.eq_ignore_ascii_case(command)) && entry.args.get(1) == Some(&key)
        };

        // The slow log, through the same door, on every server: log every
        // command for a moment.
        let slower_than = config_value(&mut c, "slowlog-log-slower-than").await;
        set_config_raw(&mut c, "slowlog-log-slower-than", "0").await;
        let _: String = cmd("SET").arg(&key).arg("v").query_async(&mut c).await.expect("SET");
        let slow = client.get_command_logs(CommandLogKind::Slow).await.expect("slow log");
        set_config_raw(&mut c, "slowlog-log-slower-than", &slower_than).await;
        assert!(
            slow.iter().any(|e| is_set_of_key(e, "SET")),
            "the SET was logged: {slow:?}"
        );

        if !supports(&id, floors::COMMANDLOG).await {
            eprintln!("skipped the size logs: COMMANDLOG is Valkey 8.1+");
            return;
        }
        // Thresholds down to 1 KB, then one 2 KB request and one 2 KB reply.
        let request_threshold = config_value(&mut c, "commandlog-request-larger-than").await;
        let reply_threshold = config_value(&mut c, "commandlog-reply-larger-than").await;
        set_config_raw(&mut c, "commandlog-request-larger-than", "1024").await;
        set_config_raw(&mut c, "commandlog-reply-larger-than", "1024").await;
        let payload = "x".repeat(2048);
        let _: String = cmd("SET")
            .arg(&key)
            .arg(&payload)
            .query_async(&mut c)
            .await
            .expect("SET");
        let _: String = cmd("GET").arg(&key).query_async(&mut c).await.expect("GET");

        let requests = client
            .get_command_logs(CommandLogKind::LargeRequest)
            .await
            .expect("large-request log");
        let hit = requests
            .iter()
            .find(|e| is_set_of_key(e, "SET"))
            .unwrap_or_else(|| panic!("the 2 KB SET is a large request: {requests:?}"));
        assert!(hit.amount >= 2048, "the entry carries the request size: {hit:?}");
        let replies = client
            .get_command_logs(CommandLogKind::LargeReply)
            .await
            .expect("large-reply log");
        let hit = replies
            .iter()
            .find(|e| is_set_of_key(e, "GET"))
            .unwrap_or_else(|| panic!("the 2 KB GET is a large reply: {replies:?}"));
        assert!(hit.amount >= 2048, "the entry carries the reply size: {hit:?}");

        client
            .commandlog_reset(CommandLogKind::LargeRequest)
            .await
            .expect("COMMANDLOG RESET");
        let requests = client
            .get_command_logs(CommandLogKind::LargeRequest)
            .await
            .expect("large-request log");
        assert!(requests.is_empty(), "reset cleared the large-request log: {requests:?}");
        let replies = client
            .get_command_logs(CommandLogKind::LargeReply)
            .await
            .expect("large-reply log");
        assert!(
            replies.iter().any(|e| is_set_of_key(e, "GET")),
            "the other log is untouched: {replies:?}"
        );

        set_config_raw(&mut c, "commandlog-request-larger-than", &request_threshold).await;
        set_config_raw(&mut c, "commandlog-reply-larger-than", &reply_threshold).await;
    });
}

/// `HOTKEYS` (8.6): the full lifecycle — start with both metrics, hammer a
/// key, read the live report, stop (report stays), reset (report gone).
#[test]
#[ignore]
fn standalone_hotkeys_collects_a_report() {
    smol::block_on(async {
        let id = register(server("it-hotkeys", standalone())).await;
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        // A Valkey 9.x version number clears the 8.6 floor, but HOTKEYS is
        // a Redis-only command — the floor table knows.
        if !client.supports(floors::HOTKEYS) {
            eprintln!("skipped: HOTKEYS is Redis 8.6+ only");
            return;
        }
        // A stale collection from an earlier run: stop (idempotent) + reset.
        client.hotkeys_stop().await.expect("stop stale");
        client.hotkeys_reset().await.expect("reset stale");
        let empty = client.hotkeys_report().await.expect("empty report");
        assert!(empty.is_empty() && !empty.tracking_active);

        client.hotkeys_start(true, true, 10).await.expect("start");
        let key = unique("hot");
        let mut c = conn(&id, 0).await;
        cmd("SET").arg(&key).arg("v").exec_async(&mut c).await.expect("set");
        for _ in 0..40 {
            let _: Option<String> = cmd("GET").arg(&key).query_async(&mut c).await.expect("get");
        }
        assert!(
            client.hotkeys_report().await.expect("live report").tracking_active,
            "tracking shows active while collecting"
        );

        client.hotkeys_stop().await.expect("stop");
        let report = client.hotkeys_report().await.expect("stopped report");
        assert!(!report.tracking_active);
        assert!(
            report.by_cpu.iter().any(|e| e.key == key),
            "the hammered key ranks by CPU time: {:?}",
            report.by_cpu
        );
        assert!(
            report.by_net.iter().any(|e| e.key == key),
            "…and by network bytes: {:?}",
            report.by_net
        );
        assert!(report.total_cpu_us > 0 && report.total_net_bytes > 0);
        assert!(
            report.by_cpu.windows(2).all(|w| w[0].value >= w[1].value),
            "merged list is descending"
        );

        client.hotkeys_reset().await.expect("reset");
        assert!(client.hotkeys_report().await.expect("cleared").is_empty());
        cmd("DEL").arg(&key).exec_async(&mut c).await.expect("del");
    });
}

/// `INFO keysizes` (8+): written keys land in per-type bucket histograms —
/// strings bucketed by value bytes, containers by element count.
#[test]
#[ignore]
fn standalone_info_keysizes_buckets_types() {
    smol::block_on(async {
        let id = register(server("it-keysizes", standalone())).await;
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        // Flavor-aware: Valkey 8.x passes a bare 8.0 floor but has no
        // `keysizes` section, so `INFO keysizes` comes back empty there.
        let at = ServerDb::new(&id, 0);
        if !client.supports_info_keysizes() {
            // The panel's operation answers no histogram rather than an
            // error, and sends nothing for it.
            assert!(key_size_distributions(&at).await.expect("keysizes").is_empty());
            eprintln!("skipped: server has no INFO keysizes (Redis 8+ only)");
            return;
        }
        let prefix = unique("ks");
        let mut c = conn(&id, 0).await;
        cmd("SET")
            .arg(format!("{prefix}:s"))
            .arg("x".repeat(100))
            .exec_async(&mut c)
            .await
            .expect("set");
        for i in 0..5 {
            cmd("RPUSH")
                .arg(format!("{prefix}:l"))
                .arg(i)
                .exec_async(&mut c)
                .await
                .expect("rpush");
        }

        let dists = key_size_distributions(&at).await.expect("keysizes");
        let strings = dists.iter().find(|d| d.type_name == "strings").expect("strings dist");
        assert_eq!(strings.unit, KeysizesUnit::Bytes);
        assert!(strings.total() >= 1);
        let lists = dists.iter().find(|d| d.type_name == "lists").expect("lists dist");
        assert_eq!(lists.unit, KeysizesUnit::Items);
        assert!(lists.total() >= 1);

        cmd("DEL")
            .arg(format!("{prefix}:s"))
            .arg(format!("{prefix}:l"))
            .exec_async(&mut c)
            .await
            .expect("del");
    });
}

/// `BGSAVE CANCEL` (Valkey 8.1+): a snapshot just started is stopped — or
/// has already landed, a tiny dataset forking and finishing inside the
/// round trip, in which case the server says so and nothing is stopped.
/// With nothing running the refusal is the documented one.
#[test]
#[ignore]
fn standalone_bgsave_cancel_stops_a_snapshot_on_valkey_8_1() {
    smol::block_on(async {
        let id = register(server("it-standalone", standalone())).await;
        let at = ServerDb::new(&id, 0);
        if !supports(&id, floors::BGSAVE_CANCEL).await {
            eprintln!("skipped: BGSAVE CANCEL is Valkey 8.1+");
            return;
        }
        // The other tests share this server and may leave a save running
        // or scheduled, so what a cancel finds is not this test's to say;
        // what it proves is that the command is one the server takes —
        // "cancelled", or the documented refusal — never a syntax error.
        let taken = |result: Result<(), zedis_connection::error::Error>| match result {
            Ok(()) => true,
            Err(e) => {
                assert!(e.to_string().contains("not in progress"), "{e}");
                false
            }
        };
        let _started = bgsave(&at).await;
        taken(bgsave_cancel(&at).await);
        smol::Timer::after(std::time::Duration::from_millis(500)).await;
        taken(bgsave_cancel(&at).await);
    });
}

/// The memory analyzer's round: a scan of the whole keyspace in pages, each
/// sampled key with its type, size, TTL, encoding and idle time — and the
/// `MEMORY STATS` table beside it, a row per metric with the per-database
/// entries flattened under a dotted name.
#[test]
#[ignore]
fn standalone_memory_panels_sample_keys_and_read_memory_stats() {
    smol::block_on(async {
        let id = register(server("it-memory-panels", standalone())).await;
        let at = ServerDb::new(&id, 0);
        let mut c = conn(&id, 0).await;
        let prefix = unique("memory");
        let (hash, string) = (format!("{prefix}:h"), format!("{prefix}:s"));
        cmd("HSET")
            .arg(&hash)
            .arg("a")
            .arg("1")
            .arg("b")
            .arg("2")
            .exec_async(&mut c)
            .await
            .expect("hset");
        cmd("SET")
            .arg(&string)
            .arg("x".repeat(1000))
            .arg("EX")
            .arg(600)
            .exec_async(&mut c)
            .await
            .expect("set");

        // Every key, at a ratio of 1: the rounds end when every cursor is 0.
        // The heat column is whichever probe the policy makes answerable,
        // as the analyzer asks it.
        let heat = get_server_heat_probe(&id);
        let mut sampled = Vec::new();
        let mut cursors = None;
        for _ in 0..100 {
            let (scanned, next, rows) = sample_memory_usage(&at, 1.0, 500, cursors, heat, true)
                .await
                .expect("sample");
            assert!(scanned as usize >= rows.len(), "a ratio of 1 samples what it scans");
            sampled.extend(rows);
            if next.iter().all(|cursor| *cursor == 0) {
                break;
            }
            cursors = Some(next);
        }
        let row = |key: &str| {
            sampled
                .iter()
                .find(|row| row.key == key)
                .unwrap_or_else(|| panic!("{key} was sampled"))
        };
        let (hash_row, string_row) = (row(&hash), row(&string));
        assert_eq!((hash_row.key_type.as_str(), hash_row.ttl), ("hash", -1));
        assert_eq!(string_row.key_type, "string");
        assert!(string_row.ttl > 0 && string_row.ttl <= 600, "ttl {}", string_row.ttl);
        assert!(
            string_row.memory_usage >= 1000,
            "MEMORY USAGE counts the value: {}",
            string_row.memory_usage
        );
        assert!(hash_row.memory_usage > 0);
        assert!(
            hash_row.encoding == "listpack" || hash_row.encoding == "ziplist",
            "OBJECT ENCODING was asked: {:?}",
            hash_row.encoding
        );
        assert!(
            match heat {
                HeatProbe::Freq => matches!(hash_row.heat, HeatMetric::Freq(_)),
                HeatProbe::IdleTime => matches!(hash_row.heat, HeatMetric::IdleTime(_)),
                HeatProbe::None => hash_row.heat == HeatMetric::None,
            },
            "the heat the policy answers: {heat:?} → {:?}",
            hash_row.heat
        );

        let stats = memory_stats(&at).await.expect("memory stats");
        let [node]: [_; 1] = stats.try_into().expect("one node on a standalone");
        let metric = |name: &str| {
            node.value
                .iter()
                .find(|(metric, _)| metric == name)
                .map(|(_, value)| value)
        };
        assert!(
            metric("keys.count").is_some_and(|count| count.parse::<u64>().is_ok_and(|count| count >= 2)),
            "the table counts the keys: {:?}",
            node.value
        );
        assert!(metric("peak.allocated").is_some(), "{:?}", node.value);
        assert!(
            node.value.iter().any(|(metric, _)| metric.starts_with("db.0.")),
            "the database's own map is flattened under db.0: {:?}",
            node.value
        );

        cmd("DEL")
            .arg(&hash)
            .arg(&string)
            .exec_async(&mut c)
            .await
            .expect("del");
    });
}

/// What the server-wide confirms run: `FLUSHDB` empties the database it is
/// told and no other, `FLUSHALL` every one, `SCRIPT FLUSH` the script cache
/// and (7.0+) `FUNCTION FLUSH` the libraries — and `FUNCTION RESTORE …
/// FLUSH` puts back what a dump held in place of what is there. On the
/// `busy` scenario's server, which no shared test writes to.
#[test]
#[ignore]
fn busy_server_flushes_empty_what_they_name() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_BUSY");
        let _busy = BUSY_SERVER.lock().await;
        let id = register(server("it-busy", addr)).await;
        let (db0, db1) = (ServerDb::new(&id, 0), ServerDb::new(&id, 1));
        let key = unique("flush");
        let mut c0 = conn(&id, 0).await;
        let mut c1 = conn(&id, 1).await;
        for c in [&mut c0, &mut c1] {
            cmd("SET").arg(&key).arg("v").exec_async(c).await.expect("set");
        }
        let exists = async |c: &mut RedisAsyncConn| -> bool {
            cmd("EXISTS").arg(&key).query_async::<i64>(c).await.expect("exists") == 1
        };

        flush_db(&db1).await.expect("flushdb");
        assert!(!exists(&mut c1).await, "the database it was told");
        assert!(exists(&mut c0).await, "and no other");
        flush_all(&db1).await.expect("flushall");
        assert!(!exists(&mut c0).await, "every database");

        let script = format!("return '{}'", unique("flushed"));
        for async_mode in [false, true] {
            let sha = script_load(&db0, &script).await.expect("script load");
            script_flush(&db0, async_mode).await.expect("script flush");
            assert_eq!(
                script_exists(&db0, &[sha]).await.expect("script exists"),
                [false],
                "the cache is empty at once, ASYNC={async_mode} or not"
            );
        }

        if !supports(&id, floors::FUNCTIONS).await {
            eprintln!("skipped the functions half: they are Redis 7.0+");
            return;
        }
        let library =
            |name: &str| format!("#!lua name={name}\nredis.register_function('{name}_f', function() return 1 end)");
        let names = async || -> Vec<String> {
            let mut names: Vec<String> = function_list(&db0, false)
                .await
                .expect("function list")
                .libraries
                .into_iter()
                .map(|lib| lib.name)
                .collect();
            names.sort();
            names
        };
        function_flush(&db0, false).await.expect("start from none");
        function_load(&db0, &library("zedis_it_kept"), false)
            .await
            .expect("load");
        let dump = function_dump(&db0).await.expect("function dump");
        function_load(&db0, &library("zedis_it_dropped"), false)
            .await
            .expect("load");
        function_restore(&db0, &dump, FunctionRestorePolicy::Flush)
            .await
            .expect("function restore flush");
        assert_eq!(
            names().await,
            ["zedis_it_kept"],
            "what the dump held, in place of what was there"
        );
        for async_mode in [false, true] {
            function_load(&db0, &library("zedis_it_flushed"), true)
                .await
                .expect("load");
            function_flush(&db0, async_mode).await.expect("function flush");
            assert!(names().await.is_empty(), "every library, ASYNC={async_mode} or not");
        }
    });
}
