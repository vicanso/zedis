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

### A1. Lua Script Debugger (`SCRIPT DEBUG` / LDB) — **Done (core)**
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
  - Valkey keeps LDB; Dragonfly and Garnet do not.
  - Web build: currently desktop-only (`this_build::debugs_lua_scripts()`); web support requires a long-lived bridge session similar to the terminal.
- **Remaining**:
  - Cluster support: route the debug connection to the master owning `KEYS[1]`'s slot; reject multi-key cross-slot scripts up front with a hash-tag hint; provide a master node selector when `KEYS` is empty (`SYNC` still requires dangerous-command confirmation).

### A2. Key Timeline / Watch — **Done (core)**
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
- **Remaining**:
  - Prefix-level watch entry from a key tree folder's context menu → opens the Keyspace Notifications panel narrowed to `__keyspace@<db>__:<prefix>*`, plus a per-row action to open the single-key timeline dialog.
  - Persist single-key timeline snapshots in `zedis-db` (≤50 snapshots per key, each ≤ `MAX_INLINE_VALUE_SIZE`, 7-day retention, integrated with `move_local_records` on key rename).

### A3. Background Alerts & Desktop Notifications — **Done (core)**
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
- **Remaining**:
  - Desktop alert webhook + system tray / status-bar alert badge (deferred).

### A4. Keyspace Schema Inference & Data Dictionary Export — **Done (2026-10-31)**
- **Why**: Engineers who inherit a Redis instance have no documentation of what lives in it. An automatically inferred data dictionary (prefix → type → fields / JSON shape → TTL policy → size distribution) is unique, and it is easy to share inside a team.
- **Builds on**:
  - `crates/zedis-core` `key_segments`, `tally`, JSONPath
  - `src/views/memory_analysis/` (SCAN sampling, prefix drill-down, TTL histogram) and the offline RDB parser (no production load at all)
- **Scope**:
  - Sample N keys per prefix group (SCAN or an RDB file). For each group record: type and encoding mix, key count estimate, TTL policy (none / fixed / spread), size percentiles, and for hashes / JSON / strings that parse as JSON, the merged field set with value types and presence ratio.
  - Pattern collapsing: `user:123:profile` → `user:{id}:profile` (numeric / UUID / hash segment detection in `zedis-core`, unit-tested).
  - Export as Markdown and JSON. Optionally add AI-written one-line descriptions per group through the existing AI assistant (opt-in, values never sent unless the user allows it).
- **Constraints**: Sampling must honor the Prod safety rules and SCAN pacing. Values are read only to infer structure, and only structure is persisted (in line with ADR 19's "keeps no values").

### A5. Bulk Migration & Sync Wizard (upgrade of the existing prefix copy) — **Done (2026-11-01)**
- **Why**: Zedis already copies a single key, a prefix (streamed `DUMP` / `RESTORE`) and compares servers. Adding throttling, resume, verification and optional live sync covers the "move this data to the new cluster" job that otherwise needs redis-shake or custom scripts.
- **Builds on**:
  - `src/views/migration_window.rs` (import / export jobs with conflict modes and dry-run preview), `crates/zedis-connection/src/dump_restore.rs`, `logical_copy.rs` (cross-flavor copy, ADR 16), `compare.rs`
- **Scope**:
  - Multiple patterns plus a type filter. A rate limit (keys/s or MB/s) and a pause / resume control. Each job is persisted in `zedis-db` with the SCAN cursor, so it survives an app restart.
  - Post-copy verification: count, sampled or full value hash comparison, and a report of mismatches.
  - Optional "follow" mode: after the initial copy, apply changes seen via keyspace notifications until the user stops it (best-effort, documented as such).
- **Constraints**: The target is always a write; follow the Prod write window and `confirm_dangerous_command`. Cluster → cluster must route per slot.
- **Implemented**:
  - `KeySource::{Keys { keys }, Patterns { patterns, types }}` in `src/states/migration.rs` and `scan_pattern_batch` in `crates/zedis-connection/src/dump_restore.rs`, streaming multi-pattern + optional `TYPE`-filtered `SCAN` batches across standalone and cluster masters.
  - Generic `jobs` table (`crates/zedis-db/src/jobs.rs`: `StoredJobRecord`, `JobManager`) persisting migration checkpoints after every batch (`completed_offset` or `(pattern_index, type_index, node_cursors, processed_count)`), with an **Unfinished Jobs** banner in `ZedisMigrationWindow` for manual resume (guarded by `confirm_dangerous_command` and Prod write-lock checks) or discard.
  - Post-copy verification (`verify_copied_keys`) with capped mismatch/missing key samples and one-click **Open in Compare** (`open_compare_for_key`) to inspect diffs side-by-side.
  - Best-effort **Follow Live Changes** mode after pattern-based copy completion (`__keyspace@<db>__:<pattern>` via `ChannelSubscription`), with a guarded one-click `CONFIG SET notify-keyspace-events KEA` prompt when keyspace events are disabled on the source.

---

## B. Medium-Effort Reputation Builders

### B1. Built-in Benchmark — **Done (2026-10-31)**
- **Why**: A GUI `redis-benchmark` with charts, plus side-by-side comparison of two servers or two configs, fits the existing "why does prod differ from staging?" tooling (CONFIG diff).
- **Builds on**: `src/views/charts.rs`, `crates/zedis-connection/src/compare.rs`, connection pool in `manager/`.
- **Scope**: Presets (GET/SET/INCR/LPUSH/HSET/ZADD/pipeline N), concurrency, payload size, key-space size, duration. Results: ops/s, p50/p95/p99 latency, histogram. Keys use a random throwaway prefix that is cleaned up afterwards. Results can be compared across runs and exported to CSV.
- **Constraints**: Refused on Prod unless the write window is open. Writes go to a configurable db. Web build: run in the bridge or disable it (decide; the browser cannot open raw sockets).
- **Implemented**:
  - `crates/zedis-core/src/benchmark.rs`: `BenchmarkPreset` (`GET`, `SET`, `INCR`, `LPUSH`, `HSET`, `ZADD`), `BenchmarkConfig`, `LatencyBucket`, `ThroughputPoint`, `BenchmarkProgress`, `BenchmarkSummary`, `BenchmarkComparison`, exact percentile calculation (`p50`/`p95`/`p99`), adaptive log-scale latency histogram bucketing, baseline relative-delta comparison (`compare_summaries`), and latency formatters.
  - `crates/zedis-connection/src/benchmark.rs`: `run_benchmark` / `run_benchmark_with_prefix` with configurable concurrency (`1..=64`), pipeline depth (`P=1..=64`), payload size, keyspace cardinality, duration (`1..=60s`), and target `db`. Uses random throwaway prefix `zedis:bench:<12-hex>:` (with 16-way cluster hash tags `{s0}`..`{s15}` when `pipeline > 1` to avoid `CROSSSLOT` errors while spreading across slots), tracks all touched key indices via atomic bitset, and cleans them up in chunks of 1,000 on completion or cancellation.
  - `src/views/benchmark.rs` (`ServerView::Benchmark`): accessible from **Tools → Benchmark** and the command palette (`⌘K`). Enforces `Capability::CreateKey` (respecting read-only mode and the ADR 14 Prod write window) plus `confirm_dangerous_command`, renders headline metric cards, latency distribution bar chart + throughput timeline line chart, session run history with cross-run / cross-server baseline pinning (`Δ%`), and CSV export.

### B2. MONITOR Recording & Aggregate Analysis — **Done (core)**
- **Why**: Live `MONITOR` already exists, and Slow Log already has a Top Commands view. Recording a bounded MONITOR window and aggregating it by command, key prefix and client answers "who is hammering what", which neither of them shows.
- **Builds on**: `src/views/monitor.rs`, `open_monitor_feeds` in `zedis-connection`, `key_segments` / `tally` in `zedis-core`.
- **Scope**: Record for N seconds or N commands (hard caps, since MONITOR costs throughput), aggregate into command × prefix × client tables with counts and share, export as CSV/JSON. Optional replay of the read-only subset against a test server (write replay only to non-Prod targets, behind confirmation).
- **Constraints**: Show the MONITOR performance warning up front. Not available on most managed clouds (probe through `ServerCommand`).
- **Implemented**:
  - `crates/zedis-core/src/tally.rs`: extended `MonitorTally` with 2D cross-tabulations (`command_prefixes`: `Command × Key prefix`, and `client_commands`: `Client × Command`) alongside the 1D `clients`, `commands`, and `prefixes` tallies.
  - `src/views/monitor.rs`: added command-count-bounded sampling (`1,000`, `5,000`, `10,000` commands with a 60s safety timeout and live progress badge) alongside time-bounded sampling (`5s`, `10s`, `30s`), rendered the 5-column aggregate summary (`Clients`, `Commands`, `Key prefixes`, `Command × Key prefix`, `Client × Command`) with one-click multi-token drill-down into captured lines, and added `Summary (CSV)` / `Summary (JSON)` exports (`build_summary_csv` / `build_summary_json`).
- **Remaining**:
  - Read-only command subset replay (`is_read_only_command` allowlist) against a non-Prod target server at `1x / 2x / max` speed, reusing `BenchmarkSummary` to report latency percentiles, error count, and skipped write command count.

### B3. `zedis://` Deep Links & Sharing — **Done (2026-10-31)**
- **Why**: A teammate pastes `zedis://open?server=<name>&db=3&key=order:42` into chat or a ticket, and one click opens that key. The web build supports this naturally with URLs.
- **Builds on**: Server config / groups, the key tree's "reveal key" path, the command palette.
- **Scope**:
  - Desktop: register the URL scheme (macOS `Info.plist`, Windows registry in the MSI, Linux `.desktop` `MimeType`). Parse the link in `zedis-core` (pure, unit-tested). Match the server by name or id, never by embedding credentials.
  - "Copy link to key" in the key menu. Web build: the same deep link as a page URL (no root-absolute paths; respect `--base-path`).
- **Constraints**: Links never carry passwords or values. An unknown server shows a "not configured here" prompt instead of a silent failure.
- **Implemented**:
  - `crates/zedis-core/src/deep_link.rs`: `DeepLink`, `DeepLinkError`, `is_zedis_deep_link`, `DeepLink::parse`, `DeepLink::from_query_str`, `DeepLink::to_zedis_uri`, and `DeepLink::to_web_query` with RFC 3986 percent-encoding/decoding and strict rejection of embedded userinfo or secret query parameters.
  - Packaging & URL registration: registered `zedis://` alongside `redis://`/`rediss://` in `Cargo.toml` (`osx_url_schemes`), `assets/zedis.desktop`, `flatpak/io.github.vicanso.zedis.desktop`, and `wix/main.wxs` (`UrlSchemeZedis`).
  - Desktop & Web navigation: `cli_redis_urls` and `open_redis_urls` in `src/startup.rs` forward and handle `zedis://` links; `ZedisAppState::open_deep_link` in `src/states/app/servers.rs` resolves the server by ID or name (case-insensitive fallback), shows `common.deep_link_unknown_server` if not configured, and routes to the target DB, view, and key (`ZedisKeyTree::new` expands parent folders and scrolls to a pre-selected key); `zedis-web/src/lib.rs` applies `window.location.search` deep links once the initial server list arrives.
  - UI entry points: added **Copy link to key** (`key_tree.copy_key_link`) to the key tree right-click context menu (`src/views/key_tree/row_menu.rs`) and the value editor's `…` menu (`src/views/editor/more_menu.rs`).

### B4. Bidirectional Custom Decoder (`encode_command` Write-Back & JSON Sharing)
- **Status**: **Done** (desktop)
- **Why**: Instead of shipping a heavy embedded WASM runtime and forcing teams to compile bespoke WASM plugins, extending the existing Custom Decoder (`crates/zedis-db/src/scripts.rs`) with an opt-in `encode_command` lets teams decode *and* edit in-house serialization formats using standard CLI tools (`python3`, `jq`, `protoc`, `base64`, `xxd`, custom binaries) with zero binary size overhead, and share rules across a team via JSON import/export.
- **Builds on**: Decode pipeline and Custom Decoder (`crates/zedis-db/src/scripts.rs`, `src/states/server/string.rs`, `src/states/server/value.rs`, `src/views/script_editor.rs`).
- **Delivered**:
  - `ScriptConfig::encode_command` (`#[serde(default)]` for seamless schema upgrade) and `ScriptConfig::has_encoder(&self) -> bool` in `crates/zedis-db/src/scripts.rs`.
  - `ScriptManager::has_encoder_for_key`, `ScriptManager::execute_encode`, `ScriptManager::encode_for_key` (backed by raw-bytes `run_script_bytes` with identical out-of-band env-var safety, temp-file support, timeout, and non-zero exit rejection), and `ScriptManager::export_all`.
  - Value editor write-back: `RedisBytesValue::script_editable` & `is_text_editable()` unlock the text editor when a matched `DataFormat::Script` rule defines an `encode_command`; `ZedisServerState::update_value` runs `encode_with_script_viewer` before `SET`, surfacing an error notification and aborting the write if the encoder fails, and re-decoding the saved bytes so the editor stays in sync.
  - Custom Decoder UI (`src/views/script_editor.rs`): added optional **Encode Command** textarea field, table column showing the encoder or `Decode only`, and **Import** / **Export** JSON buttons.

---

## Progress Tracker

| Step | Area | Summary | Status | Remaining |
|---|---|---|---|---|
| A1 | Scripting | Lua script debugger (`SCRIPT DEBUG` / LDB) | **Done (core)** (desktop, standalone/Sentinel) | Cluster slot routing (`KEYS[1]`), cross-slot rejection, node selector when `KEYS` is empty |
| A2 | Debugging | Key timeline / watch via keyspace notifications | **Done (core)** (live keyspace push + polling, unified diff, Markdown export) | Folder "Watch This Prefix" → Keyspace Notifications + row "Open Timeline"; `zedis-db` snapshot persistence (≤50/key, 7d) |
| A3 | Monitoring | Background alert rules + desktop notifications | **Done (core)** (opt-in per server via Safety tab or Metrics Alerts dialog) | Desktop alert webhook + system tray / status-bar badge (deferred) |
| A4 | Insight | Keyspace schema inference & data dictionary export | **Done** (online SCAN + offline RDB pattern collapsing, field type inference, Markdown/JSON export) | — |
| A5 | Data Ops | Bulk migration: throttle, resume, verify, follow mode | **Done** (pattern + type `SCAN`, `zedis-db` job checkpoints & resume, rate limit, verify + Compare jump, best-effort `follow` live sync) | — |
| B1 | Performance | Built-in benchmark with charts and run comparison | **Done** (6 presets, pipeline/concurrency knobs, histogram + timeline charts, baseline diff, auto-cleanup) | — |
| B2 | Performance | MONITOR recording & aggregate analysis | **Done (core)** (time + command-count sampling, Command×Prefix & Client×Command cross-tabs, CSV/JSON export) | Read-only command subset replay (`is_read_only_command`) to non-Prod target at `1x / 2x / max` with latency summary |
| B3 | Workflow | `zedis://` deep links & "copy link to key" | **Done** (`zedis://open?server=&db=&key=&view=`, web query links, OS URL scheme registration, Copy link to key in tree & editor) | — |
| B4 | Extensibility | Bidirectional Custom Decoder (`encode_command` write-back & JSON import/export) | **Done** (opt-in `encode_command` write-back before `SET`, error toast on non-zero exit, JSON rule import/export) | — |

---

## Follow-Up Batches (Agreed 2026-11-01)

| Batch | Scope | Items |
|---|---|---|
| **Batch 1** | Docs, Polish & Cross-Navigation | ADR 8 `encode_command` amendment; `differentiation-plan.md` status sync; user-facing rename "Script Viewer" → "Custom Decoder" / "自定义解码器"; `Cargo.toml` redundant `homepage` cleanup; `wasm32` dead-code cleanup + `make check-web` `-D warnings`; cross-links between `Monitor`, `Hotkeys`, and `Slowlog` Top Commands; key-tree folder "Watch This Prefix" → Keyspace Notifications + row "Open Timeline" |
| **Batch 2** | A5 Migration Enhancements | Pattern-based source (`KeySource::{Keys, Patterns}`); generic `zedis-db` job store & manual restart resume banner; best-effort `follow` live sync mode after pattern copy; one-click jump from verification mismatch to compare diff window |
| **Batch 3** | A2 Persistence, A1 Cluster & B2 Replay | A2 single-key snapshot persistence in `zedis-db` (≤50/key, 7d, `move_local_records`); A1 cluster Lua debugging (`KEYS[1]` slot routing, cross-slot guard, node selector); B2 read-only `MONITOR` replay (`1x / 2x / max`) against non-Prod targets with `BenchmarkSummary` latency stats |
| **Batch 4** | Large-File Refactoring | Split `kv_table.rs`, `memory_analysis.rs`, `slowlog_editor.rs`, and `terminal.rs` per `refactor-plan.md` Phase 2 |
