[中文](./REDIS_VALKEY_zh.md) | English · [← Back to README](../README.md)

# Redis and Valkey

Zedis treats the two as what they are: one wire protocol, two release lines that diverged at 7.2.4 and have shipped different things since. Valkey is not a compatibility mode here: every feature that depends on the server version has its minimum version recorded separately for Redis and for Valkey (`crates/zedis-connection/src/floors.rs`), so a Valkey server is never sent a command its version does not have, a feature Valkey shipped at a lower version number than Redis is enabled from that version, and whatever Valkey ships that a GUI can show has its panel or action — the table below is the whole list of divergences, checked against `COMMAND LIST` and `COMMAND DOCS` of Redis 7.4 / 8.0 / 8.10 and Valkey 8.0 / 8.1 / 9.0 / 9.1 on 2026-09-26. What is left over is what a GUI has no use for (`CLIENT CAPA`, `CLIENT IMPORT-SOURCE`, `DELIFEQ`, `MSETEX`, `CLUSTERSCAN`; on the Redis side `DELEX`, `HIMPORT`, `BACKUP` and the like), which is sent to neither. The live integration suite runs on Redis 6.2 / 7.2 / 8.0, Valkey 8.0 / 9.0, `redis-stack` and `valkey-bundle` (Valkey 9.1 with its modules) on every change — the Valkey versions are pinned to the *first* release of each line, which is where a wrong minimum version would show up.

| Feature | Redis | Valkey |
|---|---|---|
| `COMMANDLOG` — slow, large-request and large-reply logs | — | 8.1 |
| Atomic slot migration (`CLUSTER MIGRATESLOTS` on Valkey, `CLUSTER MIGRATION` on Redis — one Reshard tab, the server's own command behind it) | 8.4 | 9.0 |
| `CLUSTER SLOT-STATS` (per-slot keys, CPU, network) | 8.2 | 8.0 |
| Multiple databases in cluster mode | — | 9.0 |
| Hash field TTL (`HEXPIRE`, `HSETEX`, `HTTL`…) | 7.4 | 9.0 |
| `SET … IFEQ` | 8.4 | 8.1 |
| `CLIENT KILL … MAXAGE` | 7.4 | 8.0 |
| `SCRIPT SHOW` — the source behind an `EVALSHA` in the slow log | — | 8.0 |
| Availability zone per node (`availability-zone`, on the topology page) | — | 8.1 |
| `BGSAVE CANCEL` — a Cancel button beside the running snapshot | — | 8.1 |
| Clients paused, shown in the status bar (`paused_actions` in `INFO clients`) | from Zedis's own `CLIENT PAUSE` only | 8.1 — whoever paused them |
| Average latency per event (`LATENCY LATEST` sum / count) | mean of `LATENCY HISTORY` (last 160) | 8.1 — exact since the last reset |
| `INFO keysizes` histograms (memory analyzer) | 8.0 | — |
| `HOTKEYS` tracking | 8.6 | — |
| Stream `XACKDEL` / `XDELEX` and reference policies | 8.2 | — |
| `XNACK` | 8.8 | — |
| Vector sets | 8.0 | — |
| `allkeys-lrm` / `volatile-lrm` eviction | 8.6 | — |
| JSON | RedisJSON | valkey-json |
| Search | RediSearch | valkey-search — `FT.CREATE` / `SEARCH` / `AGGREGATE` / `INFO`; no `TAGVALS`, `SPELLCHECK`, `EXPLAIN`, `PROFILE` or `DROPINDEX DD`, which the panel reports as unavailable. Its `FT.AGGREGATE` groups only over fields `LOAD`ed into the working set (SORTABLE is no shortcut there), so Zedis loads the grouped and reduced fields on Valkey and not on RediSearch |
| Probabilistic | RedisBloom — BF, CF, CMS, TOPK, TDIGEST | valkey-bloom — BF |
| Time series | RedisTimeSeries | — |

A command a server does not have is never a crash: the panel that needs it says so, and everything else keeps working. A command that *would* crash the server is not sent either: Redis 8.0–8.2.6 and Valkey 8.0 segfault in `lookupKey()` when a `CLIENT NO-TOUCH` client unblocks another client, so Zedis withholds that flag there and the memory analyzer's heat column is a little less exact instead.

**Copying between the two.** `DUMP` payloads carry the RDB version of the server that wrote them, and Redis and Valkey number theirs independently — Redis 7.4 writes 12 and Redis 8.10 writes 15, Valkey 8 writes 11 and Valkey 9 writes 80 — so `RESTORE` refuses a copy in every direction but Valkey 8 → Redis. Zedis notices that refusal and re-creates the key by type from the source instead (string, hash, list, set, sorted set, stream with its entry ids, JSON), TTL included, so a migration or a single-key copy between a Redis and a Valkey works in both directions; the log says which keys travelled that way. What cannot travel like that is reported: a Bloom filter, a time series or a vector set cannot be read in a portable form, a stream's consumer groups and a hash's per-field TTLs are not part of the value. A `.zdis` file holds `DUMP` payloads, so its keys cannot be re-created on the other kind of server — export as JSON to move keys between the two by file.
