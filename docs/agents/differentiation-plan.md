# Differentiation Feature Plan

Drawn up 2026-10-31 after comparing `docs/FEATURES.md` with what other Redis GUIs ship (Redis Insight, Another Redis Desktop Manager, Tiny RDM, Medis). Every item below is something the feature tour does not cover today. Each one builds on infrastructure Zedis already has, so it can be designed, implemented and reviewed as an independent step.

## Rules Every Step Follows

- **Desktop first (ADR 9)**: A change made for the browser must not alter desktop behavior. Gate at the widest level (module > item > statement), and run both `make lint` and `make check-web`.
- **Nothing in `src/` speaks Redis (ADR 10)**: Every new Redis operation lives in `crates/zedis-connection`, takes `&ServerDb`, returns a typed struct, and gets a live test under `crates/zedis-connection/tests/live/`. Server pushes and blocking reads use a dedicated connection type (like `TerminalSession`, `StreamTail`, `ChannelSubscription`).
- **Command gates**: New commands are registered in `tests/command_floors.rs` (`GATED` + `floors::` constant when newer than Redis 6.2) and in `crates/zedis-connection/tests/read_only_commands.rs` (`is_read_only_command` or `WRITES`).
- **Production safety**: Anything that writes, or that puts load on the server (benchmarks, replay, bulk sync), respects the Prod write window (ADR 14) and asks for confirmation through `views/danger_confirm.rs`.
- **Local storage**: New `zedis-db` stores carry `#[serde(default)]`, open their tables in `ensure_schema`, skip unreadable rows, and join `move_local_records` when they are keyed per Redis key.
- **Locale parity**: UI strings go into all 8 `locales/*.toml` files (`make check-locales`).
- **Timers & dates**: Intervals live in `src/helpers/pacing.rs`. User-facing times go through `src/helpers/datetime.rs`.
- **Docs parity**: Each shipped item updates `docs/FEATURES.md` + `docs/FEATURES_zh.md` (and the READMEs when it is a headline feature).
- **Gates**: `make fmt`, `make lint`, `make test`, plus `make check-web` / `make lint-fast` / `make deny` when applicable. Never commit; offer a single-line conventional commit message.

---

## A. Strong Differentiators (rarely or never seen in other Redis GUIs)

### A1. Lua Script Debugger (`SCRIPT DEBUG` / LDB)
- **Why**: Debugging Lua today means `redis-cli --ldb` in a terminal. No mainstream GUI offers breakpoints, stepping and variable inspection for Redis scripts. This would pair naturally with the existing Lua Script Library.
- **Builds on**:
  - `src/views/lua_script_library.rs`, `src/views/script_editor.rs`, `crates/zedis-db/src/lua_scripts.rs`
  - `crates/zedis-connection/src/lua_script.rs`, `crates/zedis-connection/src/terminal.rs` (a model for a dedicated, session-owning connection)
- **Scope**:
  - A dedicated `LuaDebugSession` connection type in `zedis-connection`: it sends `SCRIPT DEBUG YES` (forked, so changes roll back by default) or `SYNC` (opt-in, Prod-gated), then `EVAL`. It parses LDB replies (`step`, `continue`, `break <line>`, `print`, `eval`, `redis`, `abort`) into typed events: current line, local variables, Redis call log.
  - Editor gutter breakpoints, a step / continue / abort toolbar, a locals pane and a "Redis calls" log.
  - `KEYS` / `ARGV` input form, remembered per script in `lua_scripts.rs`.
- **Constraints & open questions**:
  - **Implemented 2026-10-31**: `crates/zedis-connection/src/lua_debug.rs` (`LuaDebugSession`) on a new `raw_connection.rs` (`RawConnection`: TCP / TLS / Unix / SSH, replies read frame by frame). Two protocol facts found by probing Redis 8.6 shaped it: the last step is answered with two replies (log ending `<endsession>`, then the script result) and the server then closes the connection, which redis-rs's multiplexed connection cannot receive; and nothing may be pipelined while paused, because LDB reads the socket itself and never sees bytes already in the query buffer. `e` runs in its own call frame (globals only). UI: `src/views/lua_script_library/debugger.rs`, opened from a script card's **Debug** button.
  - Follow-ups: cluster support (connect to the master owning `KEYS[1]`'s slot), source syntax highlighting, keyboard shortcuts (F10/F5), remembering breakpoints per script, and a `ServerCommand` probe on managed clouds where `SCRIPT DEBUG` is disabled.
  - Valkey keeps LDB; Dragonfly and Garnet do not.
  - Web build: currently desktop-only (`this_build::debugs_lua_scripts()`); web support requires a long-lived bridge session similar to the terminal.

### A2. Key Timeline / Watch
- **Why**: "Who changed this value, and when?" is a common production question. Recording every change to a key (or a prefix) as a diffable timeline turns Zedis into a debugging tool, not just a viewer.
- **Builds on**:
  - `src/views/keyspace_notifications.rs`, `crates/zedis-connection/src/subscription.rs` (keyspace event triage already exists)
  - `src/views/value_diff.rs` (side-by-side diff)
  - `crates/zedis-db/src/history_manager.rs` (edit history for changes made inside Zedis)
- **Scope**:
  - "Watch this key / prefix" from the key's right-click menu. Subscribe to `__keyspace@<db>__:<key>`. On every event, re-read the value (size-capped by `MAX_INLINE_VALUE_SIZE`) and append a snapshot `{time, event, value_hash, value?}` to a new `zedis-db` store with a per-key ring-buffer limit.
  - Timeline panel: event list, value diff between any two snapshots, and an "expired / deleted" marker. Per `keep-expired-keys-in-tree`, the key's row stays in the tree.
  - Clear indication when `notify-keyspace-events` is off, with a guarded one-click enable (it is a `CONFIG SET`, so it follows the Prod write window).
- **Constraints**:
  - Notifications are fire-and-forget: rapid writes can coalesce between reads. Say so in the UI ("snapshots, not a full audit").
  - Cluster: subscribe on the node that owns the slot.

### A3. Background Alerts & Desktop Notifications
- **Why**: Today the client is useful only while someone is looking at it. Threshold alerts with OS notifications or a tray badge would make it a resident watchdog and greatly increase retention.
- **Builds on**:
  - Heartbeat and link health (`CLAUDE.md` line 22), `crates/zedis-db/src/metrics_history.rs` (7-day metrics), `src/views/metrics.rs`
  - `crates/zedis-connection/src/diagnosis.rs` / `troubleshoot.rs` (existing health rules)
- **Scope**:
  - Rule model in `zedis-core` (pure logic, unit-tested): metric, comparator, threshold, sustained duration, cooldown. Example metrics: used memory % of `maxmemory`, evicted keys/s, connected / blocked clients, replication lag, slow log growth, instantaneous ops, link offline.
  - Rules stored per server in `zedis-db`. Evaluation reuses the samples the metrics history already collects; it adds no extra polling unless the server's panel is closed (then it polls at a pacing interval from `pacing.rs`).
  - Desktop: OS notification + status-bar / tray badge, and clicking it opens the relevant panel. Web build: in-app toast, with an optional webhook on `zedis-bridge` as a follow-up.
- **Opt-in (decided 2026-10-31)**: Alerts are **off by default**. The user enables them per server connection with a checkbox (connection settings or the alerts panel). `INFO` metrics are instance-wide, so the switch is per server, not per logical db. No rule is evaluated and no extra connection is opened for a server whose switch is off.
- **Constraints**:
  - Must not keep idle connections to servers the user never opened in this session unless the user explicitly marked them "monitor in background".
  - Respect `keep-alive` limits on managed clouds and back off like the heartbeat (ADR 5).

### A4. Keyspace Schema Inference & Data Dictionary Export
- **Why**: Engineers who inherit a Redis instance have no documentation of what lives in it. An automatically inferred data dictionary (prefix → type → fields / JSON shape → TTL policy → size distribution) is unique, and it is easy to share inside a team.
- **Builds on**:
  - `crates/zedis-core` `key_segments`, `tally`, JSONPath
  - `src/views/memory_analysis/` (SCAN sampling, prefix drill-down, TTL histogram) and the offline RDB parser (no production load at all)
- **Scope**:
  - Sample N keys per prefix group (SCAN or an RDB file). For each group record: type and encoding mix, key count estimate, TTL policy (none / fixed / spread), size percentiles, and for hashes / JSON / strings that parse as JSON, the merged field set with value types and presence ratio.
  - Pattern collapsing: `user:123:profile` → `user:{id}:profile` (numeric / UUID / hash segment detection in `zedis-core`, unit-tested).
  - Export as Markdown and JSON. Optionally add AI-written one-line descriptions per group through the existing AI assistant (opt-in, values never sent unless the user allows it).
- **Constraints**: Sampling must honor the Prod safety rules and SCAN pacing. Values are read only to infer structure, and only structure is persisted (in line with ADR 19's "keeps no values").

### A5. Bulk Migration & Sync Wizard (upgrade of the existing prefix copy)
- **Why**: Zedis already copies a single key, a prefix (streamed `DUMP` / `RESTORE`) and compares servers. Adding throttling, resume, verification and optional live sync covers the "move this data to the new cluster" job that otherwise needs redis-shake or custom scripts.
- **Builds on**:
  - `src/views/migration_window.rs` (import / export jobs with conflict modes and dry-run preview), `crates/zedis-connection/src/dump_restore.rs`, `logical_copy.rs` (cross-flavor copy, ADR 16), `compare.rs`
- **Scope**:
  - Multiple patterns plus a type filter. A rate limit (keys/s or MB/s) and a pause / resume control. Each job is persisted in `zedis-db` with the SCAN cursor, so it survives an app restart.
  - Post-copy verification: count, sampled or full value hash comparison, and a report of mismatches.
  - Optional "follow" mode: after the initial copy, apply changes seen via keyspace notifications until the user stops it (best-effort, documented as such).
- **Constraints**: The target is always a write; follow the Prod write window and `confirm_dangerous_command`. Cluster → cluster must route per slot.

---

## B. Medium-Effort Reputation Builders

### B1. Built-in Benchmark
- **Why**: A GUI `redis-benchmark` with charts, plus side-by-side comparison of two servers or two configs, fits the existing "why does prod differ from staging?" tooling (CONFIG diff).
- **Builds on**: `src/views/charts.rs`, `crates/zedis-connection/src/compare.rs`, connection pool in `manager/`.
- **Scope**: Presets (GET/SET/INCR/LPUSH/HSET/ZADD/pipeline N), concurrency, payload size, key-space size, duration. Results: ops/s, p50/p95/p99 latency, histogram. Keys use a random throwaway prefix that is cleaned up afterwards. Results can be compared across runs and exported to CSV.
- **Constraints**: Refused on Prod unless the write window is open. Writes go to a configurable db. Web build: run in the bridge or disable it (decide; the browser cannot open raw sockets).

### B2. MONITOR Recording & Aggregate Analysis
- **Why**: Live `MONITOR` already exists, and Slow Log already has a Top Commands view. Recording a bounded MONITOR window and aggregating it by command, key prefix and client answers "who is hammering what", which neither of them shows.
- **Builds on**: `src/views/monitor.rs`, `open_monitor_feeds` in `zedis-connection`, `key_segments` / `tally` in `zedis-core`.
- **Scope**: Record for N seconds or N commands (hard caps, since MONITOR costs throughput), aggregate into command × prefix × client tables with counts and share, export as CSV/JSON. Optional replay of the read-only subset against a test server (write replay only to non-Prod targets, behind confirmation).
- **Constraints**: Show the MONITOR performance warning up front. Not available on most managed clouds (probe through `ServerCommand`).

### B3. `zedis://` Deep Links & Sharing
- **Why**: A teammate pastes `zedis://open?server=<name>&db=3&key=order:42` into chat or a ticket, and one click opens that key. The web build supports this naturally with URLs.
- **Builds on**: Server config / groups, the key tree's "reveal key" path, the command palette.
- **Scope**:
  - Desktop: register the URL scheme (macOS `Info.plist`, Windows registry in the MSI, Linux `.desktop` `MimeType`). Parse the link in `zedis-core` (pure, unit-tested). Match the server by name or id, never by embedding credentials.
  - "Copy link to key" in the key menu. Web build: the same deep link as a page URL (no root-absolute paths; respect `--base-path`).
- **Constraints**: Links never carry passwords or values. An unknown server shows a "not configured here" prompt instead of a silent failure.

### B4. Plugin Decoders (WASM)
- **Why**: The Custom Script Viewer already lets users decode values with a script. Opening that up as sandboxed WASM plugins would let companies decode in-house serialization formats and would grow a community ecosystem.
- **Builds on**: Decode pipeline and Custom Script Viewer (`docs/FEATURES.md` § Smart Data Viewer).
- **Scope**: A minimal ABI (`decode(bytes) -> {format, text|json}` and optional `encode`). Runtime: a lean embeddable WASM engine (evaluate with `cargo info` before adding; keep the dependency surface small). Plugins live in the config directory and are enabled per server or key pattern. They have no network or filesystem access.
- **Constraints**: Decoded views stay read-only unless the plugin provides `encode` (ADR 8). Web build: browsers can run WASM natively, but plugin distribution needs a bridge-side store; decide later.

---

## Progress Tracker

| Step | Area | Summary | Status |
|---|---|---|---|
| A1 | Scripting | Lua script debugger (`SCRIPT DEBUG` / LDB) | Done (desktop, standalone/Sentinel; cluster is a follow-up) |
| A2 | Debugging | Key timeline / watch via keyspace notifications | Proposed |
| A3 | Monitoring | Background alert rules + desktop notifications | Done (opt-in per server via Safety tab or Metrics Alerts dialog) |
| A4 | Insight | Keyspace schema inference & data dictionary export | Done (online SCAN + offline RDB pattern collapsing, field type inference, Markdown/JSON export) |
| A5 | Data Ops | Bulk migration: throttle, resume, verify, follow mode | Proposed |
| B1 | Performance | Built-in benchmark with charts and run comparison | Proposed |
| B2 | Performance | MONITOR recording & aggregate analysis | Proposed |
| B3 | Workflow | `zedis://` deep links & "copy link to key" | Proposed |
| B4 | Extensibility | WASM plugin decoders | Proposed |
