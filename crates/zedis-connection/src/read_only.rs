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

//! What a read-only caller may send.
//!
//! This is an **allowlist, and everything it does not name is refused** —
//! the opposite shape from [`crate::danger`], and deliberately so. That
//! module answers "should a human be asked first?", where missing a command
//! costs a confirmation nobody saw; this one answers "may this caller run
//! it at all?", where missing one costs the guarantee. A denylist of writes
//! cannot carry that: `EVAL` runs any script, `BITFIELD` writes under a
//! name that reads, `GETDEL` and `GETEX` are writes spelled like gets, and
//! every module command — `JSON.SET`, `TS.ADD`, `FT.CREATE` — is a write
//! the core command table has never heard of. So an unknown command is a
//! refusal, and the cost of forgetting a *read* is a panel that says it is
//! not available, which someone reports.
//!
//! [`KEYSPACE_READS`] is not hand-written: it is every command Redis itself
//! flags `READONLY` in `src/commands/*.json` (generated from 8.10.2).
//! Redis means something narrow by that flag — "does not modify the
//! keyspace" — so it covers `GET` and `SCAN` but says nothing about `INFO`
//! or `CONFIG GET`, which touch no keys at all. Those are the second and
//! third tables, and they are the judgement calls: a read-only account may
//! *look* at the server (`INFO`, `CONFIG GET`, `CLIENT LIST`, `SLOWLOG
//! GET`, `ACL LOG`) because that is most of what the panels are, and may not change it
//! (`CONFIG SET`, `CLIENT KILL`, `SLOWLOG RESET`, `REPLICAOF`, `DEBUG`).
//!
//! **This is defence in depth, not the only line.** A Redis ACL user with
//! `-@write` is enforced by the server on every connection, whatever talks
//! to it; this check is enforced by the bridge, which is the only thing the
//! browser can reach. Deploy both: the ACL is the guarantee, and this is
//! what makes the refusal land before the round trip and lets the page grey
//! the buttons out.

/// Every command Redis flags `READONLY`: reads of the keyspace.
fn is_keyspace_read(name: &str) -> bool {
    matches!(
        name,
        "ARCOUNT"
            | "ARGET"
            | "ARGETRANGE"
            | "ARGREP"
            | "ARINFO"
            | "ARLASTITEMS"
            | "ARLEN"
            | "ARMGET"
            | "ARNEXT"
            | "AROP"
            | "ARSCAN"
            | "BITCOUNT"
            | "BITFIELD_RO"
            | "BITPOS"
            | "DBSIZE"
            | "DIGEST"
            | "DUMP"
            | "EVALSHA_RO"
            | "EVAL_RO"
            | "EXISTS"
            | "EXPIRETIME"
            | "FCALL_RO"
            | "GEODIST"
            | "GEOHASH"
            | "GEOPOS"
            | "GEORADIUSBYMEMBER_RO"
            | "GEORADIUS_RO"
            | "GEOSEARCH"
            | "GET"
            | "GETBIT"
            | "GETRANGE"
            | "HEXISTS"
            | "HEXPIRETIME"
            | "HGET"
            | "HGETALL"
            | "HKEYS"
            | "HLEN"
            | "HMGET"
            | "HPEXPIRETIME"
            | "HPTTL"
            | "HRANDFIELD"
            | "HSCAN"
            | "HSTRLEN"
            | "HTTL"
            | "HVALS"
            | "KEYS"
            | "LCS"
            | "LINDEX"
            | "LLEN"
            | "LOLWUT"
            | "LPOS"
            | "LRANGE"
            | "MGET"
            | "PEXPIRETIME"
            | "PFCOUNT"
            | "PTTL"
            | "RANDOMKEY"
            | "SCAN"
            | "SCARD"
            | "SDIFF"
            | "SDIFFCARD"
            | "SINTER"
            | "SINTERCARD"
            | "SISMEMBER"
            | "SMEMBERS"
            | "SMISMEMBER"
            | "SORT_RO"
            | "SRANDMEMBER"
            | "SSCAN"
            | "STRLEN"
            | "SUBSTR"
            | "SUNION"
            | "SUNIONCARD"
            | "TOUCH"
            | "TTL"
            | "TYPE"
            | "XLEN"
            | "XPENDING"
            | "XRANGE"
            | "XREAD"
            | "XREVRANGE"
            | "ZCARD"
            | "ZCOUNT"
            | "ZDIFF"
            | "ZINTER"
            | "ZINTERCARD"
            | "ZLEXCOUNT"
            | "ZMSCORE"
            | "ZRANDMEMBER"
            | "ZRANGE"
            | "ZRANGEBYLEX"
            | "ZRANGEBYSCORE"
            | "ZRANK"
            | "ZREVRANGE"
            | "ZREVRANGEBYLEX"
            | "ZREVRANGEBYSCORE"
            | "ZREVRANK"
            | "ZSCAN"
            | "ZSCORE"
            | "ZUNION"
    )
}

/// Commands that touch no keys and only report: the panels are mostly these.
fn is_server_read(name: &str) -> bool {
    matches!(
        name,
        // Connection handshake and housekeeping. A read-only caller still
        // has to be able to open and name a connection.
        "AUTH" | "HELLO" | "PING" | "ECHO" | "SELECT" | "RESET" | "QUIT" | "TIME" | "LASTSAVE"
        // Reporting. ROLE is how the replication view learns which side it
        // is on, and MODULE LIST is asked once at connect — without it every
        // module panel would be hidden from a read-only account, which looks
        // exactly like a server with no modules.
        | "INFO" | "COMMAND" | "DBSIZE" | "WAIT" | "ASKING" | "READONLY" | "READWRITE" | "ROLE"
        // A transaction is allowed *because* every command inside it is
        // judged on its own: the bridge checks each frame of the batch, so
        // MULTI cannot be used to smuggle one past this.
        | "MULTI" | "EXEC" | "DISCARD" | "WATCH" | "UNWATCH"
    )
}

/// `CONTAINER SUBCOMMAND` pairs: the container alone says nothing, so these
/// are judged as a pair and an unlisted subcommand is refused.
fn is_container_read(name: &str, sub: &str) -> bool {
    matches!(
        (name, sub),
        // Redis's own READONLY-flagged subcommands.
        ("OBJECT", "ENCODING" | "FREQ" | "IDLETIME" | "REFCOUNT")
            | ("MEMORY", "USAGE")
            | ("XINFO", "CONSUMERS" | "GROUPS" | "STREAM")
            // Looking at the server, never changing it. CONFIG GET is here
            // and CONFIG SET is not; the same split runs through all of them.
            | ("CONFIG", "GET")
            | ("CLIENT", "ID" | "INFO" | "LIST" | "GETNAME" | "SETNAME" | "SETINFO" | "NO-EVICT" | "NO-TOUCH")
            | ("MEMORY", "DOCTOR" | "STATS" | "MALLOC-STATS")
            | ("SLOWLOG", "GET" | "LEN" | "HELP")
            | ("COMMANDLOG", "GET" | "LEN" | "HELP")
            | ("LATENCY", "LATEST" | "HISTORY" | "DOCTOR" | "GRAPH" | "HELP")
            // DRYRUN simulates a command and runs none, which is what the
            // access-mode probe asks it for; it is a read however much the
            // command it is asked about is not. `ACL LOG` (the security
            // log) is a read; `ACL LOG RESET` is judged separately because
            // RESET is the *second* argument.
            | ("ACL", "WHOAMI" | "CAT" | "LIST" | "USERS" | "GETUSER" | "DRYRUN" | "HELP" | "LOG")
            | ("CLUSTER", "INFO" | "MYID" | "NODES" | "SHARDS" | "SLOTS" | "LINKS" | "SLAVES" | "REPLICAS")
            | ("CLUSTER", "COUNTKEYSINSLOT" | "GETKEYSINSLOT" | "KEYSLOT" | "COUNT-FAILURE-REPORTS")
            | ("FUNCTION", "LIST" | "DUMP" | "STATS" | "HELP")
            | ("MODULE", "LIST" | "HELP")
            // Valkey's hot-keys sampler: GET reads what it collected, RESET
            // and STOP change what it is doing.
            | ("HOTKEYS", "GET")
            // Sentinel administration is mostly reads; FAILOVER, SET, MONITOR,
            // REMOVE and RESET are not, and are absent.
            | ("SENTINEL", "MASTERS" | "MASTER" | "SENTINELS" | "REPLICAS" | "SLAVES" | "CKQUORUM")
            | ("SENTINEL", "GET-MASTER-ADDR-BY-NAME" | "IS-MASTER-DOWN-BY-ADDR" | "INFO-CACHE" | "HELP")
            | ("SCRIPT", "EXISTS" | "SHOW" | "HELP")
            | ("PUBSUB", "CHANNELS" | "NUMSUB" | "NUMPAT" | "SHARDCHANNELS" | "SHARDNUMSUB")
            | ("OBJECT", "HELP")
            | ("XGROUP", "HELP")
    )
}

/// Module reads. The core command table has never heard of these, so without
/// naming them a read-only account could not open a RedisJSON, TimeSeries,
/// Bloom, Search or Vector-Set key at all — the refusal would be correct and
/// useless. Writes are absent on purpose: `JSON.SET`, `TS.ADD`, `FT.CREATE`,
/// `BF.ADD` and `VADD` fall through to the default refusal like any other
/// unknown command.
fn is_module_read(name: &str) -> bool {
    // spellchecker:off
    matches!(
        name,
        // RedisJSON
        "JSON.GET" | "JSON.MGET" | "JSON.TYPE" | "JSON.STRLEN" | "JSON.ARRLEN" | "JSON.ARRINDEX"
        | "JSON.OBJLEN" | "JSON.OBJKEYS" | "JSON.RESP" | "JSON.DEBUG"
        // RedisTimeSeries
        | "TS.GET" | "TS.MGET" | "TS.INFO" | "TS.RANGE" | "TS.REVRANGE" | "TS.MRANGE" | "TS.MREVRANGE"
        | "TS.QUERYINDEX"
        // RediSearch
        | "FT.SEARCH" | "FT.AGGREGATE" | "FT.INFO" | "FT.EXPLAIN" | "FT.EXPLAINCLI" | "FT._LIST"
        | "FT.PROFILE" | "FT.SPELLCHECK" | "FT.SUGLEN" | "FT.SUGGET" | "FT.TAGVALS" | "FT.CURSOR"
        // RedisBloom and friends
        | "BF.EXISTS" | "BF.MEXISTS" | "BF.INFO" | "BF.CARD" | "BF.SCANDUMP"
        | "CF.EXISTS" | "CF.MEXISTS" | "CF.COUNT" | "CF.INFO" | "CF.SCANDUMP"
        | "CMS.QUERY" | "CMS.INFO"
        | "TOPK.QUERY" | "TOPK.COUNT" | "TOPK.LIST" | "TOPK.INFO"
        | "TDIGEST.RANK" | "TDIGEST.REVRANK" | "TDIGEST.QUANTILE" | "TDIGEST.CDF" | "TDIGEST.INFO"
        | "TDIGEST.MIN" | "TDIGEST.MAX" | "TDIGEST.TRIMMED_MEAN" | "TDIGEST.BYRANK" | "TDIGEST.BYREVRANK"
        // Vector sets
        | "VSIM" | "VEMB" | "VCARD" | "VDIM" | "VINFO" | "VLINKS" | "VGETATTR" | "VRANDMEMBER" | "VISMEMBER"
    )
    // spellchecker:on
}

/// Whether a read-only caller may run `name` with the arguments after it.
///
/// The first argument is the subcommand for a container (`CONFIG GET`
/// against `CONFIG SET`). A second argument is consulted only for
/// `ACL LOG`: `ACL LOG [count]` reads, `ACL LOG RESET` writes. Case is
/// folded, because RESP carries whatever the caller typed.
pub fn is_read_only_command(name: &str, args: &[&str]) -> bool {
    let name = name.to_ascii_uppercase();
    if is_keyspace_read(&name) || is_server_read(&name) || is_module_read(&name) {
        return true;
    }
    let Some(sub) = args.first() else {
        return false;
    };
    let sub = sub.to_ascii_uppercase();
    if name == "ACL" && sub == "LOG" {
        return !args.get(1).is_some_and(|word| word.eq_ignore_ascii_case("RESET"));
    }
    is_container_read(&name, &sub)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed(line: &str) -> bool {
        let mut parts = line.split_whitespace();
        let name = parts.next().expect("a command name");
        let args: Vec<&str> = parts.collect();
        is_read_only_command(name, &args)
    }

    #[test]
    fn the_reads_a_panel_needs_are_allowed() {
        for line in [
            // The key tree and the value editors.
            "SCAN 0 MATCH * COUNT 10",
            "TYPE user:1",
            "TTL user:1",
            "GET user:1",
            "HGETALL h",
            "LRANGE l 0 -1",
            "SMEMBERS s",
            "ZRANGE z 0 -1",
            "XRANGE st - +",
            "XINFO STREAM st",
            "OBJECT ENCODING k",
            "MEMORY USAGE k",
            "DUMP k",
            // The observability panels.
            "INFO everything",
            "CONFIG GET maxmemory",
            "CLIENT LIST",
            "SLOWLOG GET 10",
            "LATENCY LATEST",
            "MEMORY DOCTOR",
            "CLUSTER NODES",
            "ACL WHOAMI",
            "ACL LOG",
            "ACL LOG 0",
            "ACL LOG 128",
            // Opening a connection at all.
            "HELLO 3",
            "AUTH user pass",
            "SELECT 3",
            "PING",
            "CLIENT SETNAME zedis",
            // Module reads.
            "JSON.GET doc $",
            "TS.RANGE series - +",
            "FT.SEARCH idx *",
            "BF.EXISTS filter item",
            "VSIM vset ELE a",
        ] {
            assert!(allowed(line), "should be allowed: {line}");
        }
    }

    #[test]
    fn the_writes_are_refused() {
        for line in [
            "SET k v",
            "DEL k",
            "FLUSHALL",
            "FLUSHDB",
            "HSET h f v",
            "EXPIRE k 10",
            "RESTORE k 0 payload",
            "XADD st * f v",
            "JSON.SET doc $ 1",
            "TS.ADD series * 1",
            "FT.CREATE idx ON HASH",
            "BF.ADD filter item",
            "VADD vset VALUES 3 1 2 3 elem",
        ] {
            assert!(!allowed(line), "should be refused: {line}");
        }
    }

    /// The four that make a denylist the wrong shape, and the reason this
    /// module is an allowlist: each one writes under a name that does not
    /// say so, and `danger::is_write_command` names none of them.
    #[test]
    fn a_write_that_does_not_look_like_one_is_still_refused() {
        for line in [
            "EVAL script 0",
            "EVALSHA sha 0",
            "FCALL f 0",
            "BITFIELD k SET u8 0 1",
            "GETDEL k",
            "GETEX k",
        ] {
            assert!(!allowed(line), "should be refused: {line}");
            assert!(
                !crate::is_write_command(line.split_whitespace().next().expect("name")),
                "if danger.rs learns this one, say so here: {line}"
            );
        }
        // Their read-only twins exist and are allowed, so the panels that
        // only evaluate still work.
        for line in ["EVAL_RO script 0", "FCALL_RO f 0", "BITFIELD_RO k GET u8 0"] {
            assert!(allowed(line), "should be allowed: {line}");
        }
    }

    /// An unknown command is refused rather than waved through — the whole
    /// point of the allowlist. A module that ships tomorrow lands here.
    #[test]
    fn an_unknown_command_is_refused() {
        for line in ["SOMETHING.NEW k", "QUUX", "GRAPH.QUERY g pattern"] {
            assert!(!allowed(line), "should be refused: {line}");
        }
    }

    /// A container command is judged with its subcommand, so the read half
    /// of a container passes and the write half does not.
    #[test]
    fn a_container_is_judged_by_its_subcommand() {
        assert!(allowed("CONFIG GET maxmemory"));
        assert!(!allowed("CONFIG SET maxmemory 0"));
        assert!(!allowed("CONFIG RESETSTAT"));
        assert!(allowed("SLOWLOG GET"));
        assert!(!allowed("SLOWLOG RESET"));
        assert!(allowed("CLIENT LIST"));
        assert!(!allowed("CLIENT KILL ID 4"));
        assert!(allowed("ACL WHOAMI"));
        assert!(allowed("ACL LOG"));
        assert!(allowed("ACL LOG 0"));
        assert!(!allowed("ACL LOG RESET"));
        assert!(!allowed("ACL SETUSER bob on"));
        assert!(allowed("CLUSTER NODES"));
        assert!(!allowed("CLUSTER FAILOVER"));
        assert!(allowed("FUNCTION LIST"));
        assert!(!allowed("FUNCTION LOAD code"));
        // A container with no subcommand at all is not a read.
        assert!(!allowed("CONFIG"));
        assert!(!allowed("ACL"));
    }

    #[test]
    fn the_admin_commands_are_refused() {
        for line in [
            "DEBUG SLEEP 100",
            "SHUTDOWN NOSAVE",
            "REPLICAOF host 6379",
            "SLAVEOF NO ONE",
            "FAILOVER",
            "BGSAVE",
            "BGREWRITEAOF",
            "MONITOR",
            "SWAPDB 0 1",
            "MIGRATE host 6379 k 0 1000",
        ] {
            assert!(!allowed(line), "should be refused: {line}");
        }
    }

    #[test]
    fn case_does_not_decide_anything() {
        assert!(allowed("get k") && allowed("GeT k"));
        assert!(allowed("config get maxmemory") && allowed("CONFIG get maxmemory"));
        assert!(!allowed("set k v") && !allowed("Config Set maxmemory 0"));
    }

    /// A transaction is allowed only because the caller checks every command
    /// in the batch — if that ever stops being true, MULTI becomes a hole.
    #[test]
    fn a_transaction_opens_but_carries_nothing_through() {
        assert!(allowed("MULTI") && allowed("EXEC") && allowed("DISCARD"));
        assert!(!allowed("SET k v"), "the write inside is judged on its own");
    }
}
