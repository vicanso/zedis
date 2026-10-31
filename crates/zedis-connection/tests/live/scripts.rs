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

//! Lua scripts: the cache, read-only evaluation and stopping a runaway one.

use super::*;

/// A runaway script makes the server answer BUSY to everything — the pooled
/// connection included — so the kill travels on a fresh connection that
/// sends only what a busy server still takes. With nothing running every
/// node says NOTBUSY, which is a result, not a failure. Runs on the `busy`
/// scenario's server, which nothing else uses.
#[test]
#[ignore]
fn busy_script_kill_stops_a_runaway_script() {
    smol::block_on(async {
        // Its own server: while the script runs, every command there gets
        // BUSY, which the other tests must never see.
        let addr = skip_unless!("ZEDIS_IT_BUSY");
        let _busy = BUSY_SERVER.lock().await;
        let id = register(server("it-busy", addr)).await;
        let entry = get_server(&id).expect("saved entry");
        let mut c = conn(&id, 0).await;

        let replies = kill_running(&entry, KillTarget::Script).await.expect("kill (idle)");
        assert!(
            replies.iter().all(|r| r.outcome == KillOutcome::NothingRunning),
            "{replies:?}"
        );

        // Answer BUSY after 100ms instead of 5s, then park a read-only
        // script on a dedicated connection.
        let previous: Vec<String> = cmd("CONFIG")
            .arg("GET")
            .arg("lua-time-limit")
            .query_async(&mut c)
            .await
            .expect("config get");
        cmd("CONFIG")
            .arg("SET")
            .arg("lua-time-limit")
            .arg("100")
            .exec_async(&mut c)
            .await
            .expect("config set");
        let mut runaway = open_single_connection(&entry, 0, false)
            .await
            .expect("dedicated connection");
        let script = smol::spawn(async move {
            cmd("EVAL")
                .arg("while true do end")
                .arg(0)
                .query_async::<redis::Value>(&mut runaway)
                .await
        });
        smol::Timer::after(std::time::Duration::from_millis(500)).await;

        let mut killed = false;
        for _ in 0..20 {
            let replies = kill_running(&entry, KillTarget::Script).await.expect("kill");
            if replies.iter().any(|r| r.outcome == KillOutcome::Killed) {
                killed = true;
                break;
            }
            smol::Timer::after(std::time::Duration::from_millis(200)).await;
        }
        assert!(killed, "SCRIPT KILL never reached the busy server");
        let outcome = script.await;
        assert!(outcome.is_err(), "the script must have been stopped: {outcome:?}");

        // Back to what the server had; a plain command works again.
        cmd("CONFIG")
            .arg("SET")
            .arg("lua-time-limit")
            .arg(previous.get(1).cloned().unwrap_or_else(|| "5000".to_string()))
            .exec_async(&mut c)
            .await
            .expect("config restore");
        let pong: String = cmd("PING").query_async(&mut c).await.expect("ping after kill");
        assert_eq!(pong, "PONG");
    });
}

/// The script library's cache operations: the digest computed locally is the
/// one the server computes, `SCRIPT EXISTS` answers per digest in order, and
/// a script never loaded is `false` rather than an error.
#[test]
#[ignore]
fn standalone_script_cache_agrees_with_the_local_digest() {
    smol::block_on(async {
        let id = register(server("it-script-cache", standalone())).await;
        let at = ServerDb::new(&id, 0);
        // Unique source, so another test's SCRIPT FLUSH or LOAD cannot be
        // what this one observes.
        let code = format!("return '{}'", unique("script"));
        let sha = script_sha1(&code);
        assert_eq!(sha.len(), 40);
        assert_eq!(
            script_sha1("return 1"),
            "e0e1f9fabfc9d4800c877a703b823ac0578ff8db",
            "SHA-1 of the source, as redis-cli prints it"
        );

        let never = script_sha1(&format!("{code} -- never loaded"));
        assert_eq!(
            script_exists(&at, &[sha.clone(), never.clone()])
                .await
                .expect("script exists"),
            [false, false]
        );
        assert_eq!(script_load(&at, &code).await.expect("script load"), sha);
        assert_eq!(
            script_exists(&at, &[never, sha]).await.expect("script exists"),
            [false, true],
            "one answer per digest, in the order asked"
        );
        assert!(script_exists(&at, &[]).await.expect("no digests").is_empty());
    });
}

/// `EVALSHA_RO` (7.0): the read-only spelling makes the server refuse a
/// write inside the script; the same script runs under `EVALSHA`, and a
/// reading script runs under the read-only one.
#[test]
#[ignore]
fn standalone_evalsha_ro_rejects_writes() {
    smol::block_on(async {
        let id = register(server("it-eval-ro", standalone())).await;
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        if !client.supports(floors::EVAL_RO) {
            eprintln!("skipped: server predates EVALSHA_RO");
            return;
        }
        let mut c = conn(&id, 0).await;
        let at = ServerDb::new(&id, 0);
        let key = unique("evalro");
        let writer = "return redis.call('SET', KEYS[1], 'written')";
        let reader = "return redis.call('GET', KEYS[1])";
        let sha_of = |code: &str| redis::Script::new(code).get_hash().to_string();
        let keys = vec![key.clone()];

        let refused = run_script(&at, writer, &sha_of(writer), &keys, &[], true).await;
        let message = refused.expect_err("EVALSHA_RO must refuse a write").to_string();
        assert!(
            message.to_lowercase().contains("read-only"),
            "the server names the reason: {message}"
        );
        let exists: i64 = cmd("EXISTS").arg(&key).query_async(&mut c).await.expect("exists");
        assert_eq!(exists, 0, "the refused write left nothing behind");

        run_script(&at, writer, &sha_of(writer), &keys, &[], false)
            .await
            .expect("EVALSHA writes");
        let read = run_script(&at, reader, &sha_of(reader), &keys, &[], true)
            .await
            .expect("EVALSHA_RO reads");
        assert!(read.formatted.contains("written"), "read back: {}", read.formatted);

        cmd("DEL").arg(&key).exec_async(&mut c).await.expect("del");
    });
}

/// `SCRIPT SHOW` reads a cached script back — Valkey 8.0 and later; Redis's
/// cache is write-only and the floor keeps the command off it.
#[test]
#[ignore]
fn standalone_script_show_reads_the_cache_back_on_valkey() {
    smol::block_on(async {
        let id = register(server("it-script-show", standalone())).await;
        if !supports(&id, floors::SCRIPT_SHOW).await {
            eprintln!("skipped: no SCRIPT SHOW on this server");
            return;
        }
        let at = ServerDb::new(&id, 0);
        let source = format!("return {}", unique("script-show").len());
        let sha = script_load(&at, &source).await.expect("script load");
        assert_eq!(
            zedis_connection::script_show(&at, &sha)
                .await
                .expect("script show")
                .as_deref(),
            Some(source.as_str())
        );
        assert_eq!(
            zedis_connection::script_show(&at, "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef")
                .await
                .expect("an unknown digest is not an error"),
            None
        );
    });
}

/// The function editor's round trip (7.0+): a loaded library is listed with
/// its functions — and its source, when asked — `FCALL` runs one and
/// `FCALL_RO` refuses the one that writes, and a dump puts back a library
/// deleted since, where `APPEND` refuses the libraries still there.
#[test]
#[ignore]
fn standalone_functions_list_call_dump_and_restore() {
    smol::block_on(async {
        let id = register(server("it-functions", standalone())).await;
        if !supports(&id, floors::FUNCTIONS).await {
            eprintln!("skipped: functions are Redis 7.0+");
            return;
        }
        let at = ServerDb::new(&id, 0);
        let mut c = conn(&id, 0).await;
        // Unique, so a library an earlier run left behind is not this one.
        let suffix = unique("fn").rsplit(':').take(3).collect::<Vec<_>>().join("_");
        let library = format!("zedis_it_fn_{suffix}");
        let (reader, writer) = (format!("{library}_get"), format!("{library}_set"));
        let code = format!(
            "#!lua name={library}\n\
             redis.register_function{{function_name='{reader}', \
             callback=function(keys) return redis.call('GET', keys[1]) end, flags={{'no-writes'}}}}\n\
             redis.register_function('{writer}', function(keys, args) return redis.call('SET', keys[1], args[1]) end)"
        );
        assert_eq!(function_load(&at, &code, false).await.expect("function load"), library);

        let listing = function_list(&at, true).await.expect("function list");
        assert!(!listing.unsupported);
        let listed = listing
            .libraries
            .iter()
            .find(|lib| lib.name == library)
            .expect("the library is listed");
        assert_eq!(listed.engine.to_ascii_uppercase(), "LUA");
        let mut functions: Vec<(&str, &[String])> = listed
            .functions
            .iter()
            .map(|f| (f.name.as_str(), f.flags.as_slice()))
            .collect();
        functions.sort();
        assert_eq!(
            functions,
            [
                (reader.as_str(), &["no-writes".to_string()][..]),
                (writer.as_str(), &[][..])
            ],
            "both functions, each with its flags"
        );
        assert_eq!(
            listed.code.as_deref(),
            Some(code.as_str()),
            "WITHCODE hands the source back"
        );
        assert!(
            function_list(&at, false)
                .await
                .expect("function list")
                .libraries
                .iter()
                .any(|lib| lib.name == library && lib.code.is_none()),
            "and only WITHCODE does"
        );

        let key = unique("fn-key");
        let keys = vec![key.clone()];
        assert_eq!(
            function_fcall(&at, &writer, &keys, &["written".to_string()], false)
                .await
                .expect("fcall"),
            "OK"
        );
        assert_eq!(
            function_fcall(&at, &reader, &keys, &[], true).await.expect("fcall_ro"),
            "written"
        );
        let refused = function_fcall(&at, &writer, &keys, &["again".to_string()], true)
            .await
            .expect_err("FCALL_RO refuses a function that may write");
        assert!(
            refused.to_string().to_lowercase().contains("write"),
            "the server names the reason: {refused}"
        );

        // A dump holds every library. A second one stays loaded while the
        // first is deleted, so APPEND collides with it and REPLACE puts the
        // first back beside it.
        let other = format!("{library}_other");
        function_load(
            &at,
            &format!("#!lua name={other}\nredis.register_function('{other}_f', function() return 1 end)"),
            false,
        )
        .await
        .expect("function load");
        let dump = function_dump(&at).await.expect("function dump");
        assert!(!dump.is_empty());
        function_delete(&at, &library).await.expect("function delete");
        assert!(
            function_restore(&at, &dump, FunctionRestorePolicy::Append)
                .await
                .is_err(),
            "APPEND refuses a library that is loaded"
        );
        function_restore(&at, &dump, FunctionRestorePolicy::Replace)
            .await
            .expect("function restore replace");
        assert_eq!(
            function_fcall(&at, &reader, &keys, &[], true)
                .await
                .expect("the restored library runs"),
            "written"
        );

        for name in [&library, &other] {
            function_delete(&at, name).await.expect("function delete");
        }
        cmd("DEL").arg(&key).exec_async(&mut c).await.expect("del");
    });
}

/// The script the debugger tests step through: a write on line 2, a
/// `redis.debug` on line 3, a table returned on line 4.
const DEBUGGED: &str = "local a = 1\nredis.call('SET', KEYS[1], ARGV[1])\nredis.debug('seen', a)\nreturn {a, ARGV[1]}";

/// A forked debugging session (`SCRIPT DEBUG YES`): stops before line 1,
/// runs to a breakpoint set at the start, shows the locals there, evaluates
/// an expression, and ends with the script's return value — while the write
/// it made is thrown away with the child.
#[test]
#[ignore]
fn standalone_lua_debugger_steps_a_forked_session() {
    smol::block_on(async {
        let id = register(server("it-lua-debug", standalone())).await;
        let at = ServerDb::new(&id, 0);
        let mut c = conn(&id, 0).await;
        let key = unique("lua-debug");
        let keys = vec![key.clone()];
        let args = vec!["v1".to_string()];

        let (session, first) = LuaDebugSession::start(&at, DEBUGGED, &keys, &args, &[3], DebugMode::Forked)
            .await
            .expect("start");
        assert_eq!(first.stopped_at, Some(1), "{first:?}");
        assert!(first.end.is_none());

        let at_break = session.send(&DebugCommand::Continue).await.expect("continue");
        assert_eq!(at_break.stopped_at, Some(3), "{at_break:?}");
        assert_eq!(at_break.stop_reason.as_deref(), Some("break point"));
        assert!(
            at_break.locals.iter().any(|l| l.name == "a" && l.value == "1"),
            "{:?}",
            at_break.locals
        );

        // `e` runs in a call frame of its own: the script's globals (KEYS,
        // ARGV) are there, its locals are not.
        let eval = session
            .send(&DebugCommand::Eval("#KEYS + 41".to_string()))
            .await
            .expect("eval");
        assert!(
            eval.log.contains(&DebugLine::RetVal("42".to_string())),
            "{:?}",
            eval.log
        );
        assert_eq!(eval.stopped_at, None, "an eval does not move the script");

        let end = session.send(&DebugCommand::Continue).await.expect("finish");
        assert!(
            end.log
                .iter()
                .any(|l| matches!(l, DebugLine::Debug(text) if text.contains("seen"))),
            "{:?}",
            end.log
        );
        let finished = end.end.expect("the script finished");
        assert!(!finished.error, "{}", finished.result);
        assert!(finished.result.contains("v1"), "{}", finished.result);
        assert!(!session.is_live());
        assert!(session.send(&DebugCommand::Step).await.is_err(), "the session is over");

        let exists: i64 = cmd("EXISTS").arg(&key).query_async(&mut c).await.expect("exists");
        assert_eq!(exists, 0, "a forked session rolls its writes back");
    });
}

/// `SCRIPT DEBUG SYNC` keeps what the script wrote; an abort ends the
/// session with the server's error; a script that does not compile ends
/// before its first line.
#[test]
#[ignore]
fn standalone_lua_debugger_sync_keeps_writes_and_reports_failures() {
    smol::block_on(async {
        let id = register(server("it-lua-debug-sync", standalone())).await;
        let at = ServerDb::new(&id, 0);
        let mut c = conn(&id, 0).await;
        let key = unique("lua-debug-sync");
        let keys = vec![key.clone()];
        let args = vec!["kept".to_string()];

        let (session, _) = LuaDebugSession::start(&at, DEBUGGED, &keys, &args, &[], DebugMode::Sync)
            .await
            .expect("start");
        let end = session.send(&DebugCommand::Continue).await.expect("finish");
        assert!(end.end.is_some_and(|e| !e.error));
        let value: Option<String> = cmd("GET").arg(&key).query_async(&mut c).await.expect("get");
        assert_eq!(value.as_deref(), Some("kept"), "a sync session keeps its writes");

        let (session, _) = LuaDebugSession::start(&at, DEBUGGED, &keys, &args, &[], DebugMode::Forked)
            .await
            .expect("start");
        let aborted = session.send(&DebugCommand::Abort).await.expect("abort");
        let aborted = aborted.end.expect("an abort ends the session");
        assert!(aborted.error && aborted.result.contains("abort"), "{}", aborted.result);

        let (_, broken) = LuaDebugSession::start(&at, "return (", &[], &[], &[], DebugMode::Forked)
            .await
            .expect("start");
        let broken = broken.end.expect("a script that does not compile never stops");
        assert!(broken.error, "{}", broken.result);

        cmd("DEL").arg(&key).exec_async(&mut c).await.expect("del");
    });
}
