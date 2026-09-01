[中文](./FEATURES_zh.md) | English · [← Back to README](../README.md)

# Zedis — Full Feature Tour

Zedis auto-detects (`ViewMode::Auto`) and formats payloads on the fly. This page is the exhaustive reference; the [README](../README.md) has the at-a-glance overview.

---

## 🚀 Native & Fast

### Native GPU Rendering
**Every pixel on the GPU, virtual-scrolled `SCAN` — millions of keys at 60+ FPS with minimal RAM.**

Virtual scrolling combined with `SCAN` iteration keeps the interface responsive no matter how large the keyspace — zero-lag scrolling, instant tab switches, minimal memory footprint.

### Cross-Platform & Appearance
**Native on macOS, Windows, and Linux — Light / Dark / System plus six bundled colour themes, and 8 UI languages.**

A truly native feel across all three desktop platforms. Beyond Light / Dark / Follow-system, the title-bar **Theme** menu carries six bundled palettes — **Ayu · Catppuccin · Flexoki · Gruvbox · Hybrid · Tokyo**. Settings adds a continuous **font-size slider** (12–20 px) and separate **UI** and **monospace** font pickers listing every installed family (the bundled **JetBrains Mono** is the mono default, so code and tables look identical on every platform). The interface itself ships in **8 languages** — English · 中文 · Русский · 日本語 · Português · Español · Deutsch · Français — switched from the title bar or Settings, applied live without a restart.

---

## 🧠 Smart Data Viewer

### Auto-Decompression
**Transparently unpacks LZ4, SNAPPY, GZIP, and ZSTD.**

Compressed values are unpacked in place so you read the real payload, not a blob.

### Rich Content Decoding
**JSON/RedisJSON, Protobuf, MessagePack, Unix timestamps, media & hex.**

**JSON & RedisJSON** with pretty-printing, syntax highlighting, and minimal `JSON.MERGE` diffs (RFC 7396) — **JSONPath** (`$.user.email`, `$.items[?(@.price > 100)]`) works on plain string keys too, no module needed. **MessagePack** deserializes with zero config, and **Protobuf** decodes against `.proto` schemas you register (matched to keys by exact / prefix / suffix / regex, per server); 10/13-digit **Unix timestamps** preview as local + UTC; **images** (`PNG/JPG/WEBP/SVG/GIF`) and a fully editable **hex** view round it out.

### Custom Script Viewer
**Pipe any value through an external shell command for custom decoding.**

Configure a command template with placeholders (`{KEY}`, `{VALUE}`, `{HEX}`, `{HEX_FILE}`, `{RAW_FILE}`); Zedis runs it via `sh -c` / `cmd /c` and shows stdout as the formatted value. **Starter template chips** fill the field with a working command for common tools (each needs that tool on `PATH`). Per-server key matching by exact / prefix / suffix / regex. The key and value reach the command as environment variables and are never spliced into the command line, so a hostile key name cannot turn into a shell command.

### Decode Pipeline
**A documented, deterministic order — your custom viewers always win.**

Every string value goes through the same pipeline, top priority first:

1. **Protobuf viewer** — a registered `.proto` schema matching this key
2. **Custom script viewer** — a configured script matching this key
3. **Native format detection** — MessagePack · GZIP · ZSTD · Snappy · Unix timestamp · images (`PNG/JPG/WEBP/SVG/GIF`)
4. **LZ4** (size-prepended) for non-UTF-8 payloads
5. **Text / pretty-printed JSON** fallback

A viewer that matches but fails to decode falls through to native handling, so a bad script never blanks the value. Manual overrides: the **hex** view always shows the raw bytes, and small opaque strings offer a **bitmap** toggle.

---

## 🗂️ Type & Module Viewers

### Collection Editors
**Hash, List, Set and Sorted Set open as paginated, editable tables — never one blocking `HGETALL`.**

Every collection is walked incrementally (`HSCAN` / `SSCAN` / `ZSCAN`, `LRANGE` windows) with infinite scroll and a loaded/total counter, so a hash with a million fields opens as fast as one with ten. A keyword filter narrows the table — server-side through the scan's `MATCH` for hash / set / sorted set, client-side over the loaded window for lists. Rows edit inline in a resizable side panel — add, update, delete — with per-field **Hash TTL** columns where the server supports them and an `RPUSH` / `LPUSH` choice for lists. **Bulk add** takes pasted TSV/CSV (one row per line), and the visible table exports to CSV/JSON.

### Specialized Type Viewers
**Opaque values open in purpose-built, interactive viewers.**

**Bitmap/Bitfield** paints bits on a GPU grid (`SETBIT`/`BITCOUNT`/`BITFIELD`); **HyperLogLog** shows `PFCOUNT` cardinality; **Vector Set + KNN** (Redis 8) walks the HNSW graph via `VSIM`; **Geo Map** plots a sorted set's members (`GEOPOS`) on a tile-less, zoom/pan radar, with a `GEOSEARCH` radius filter and Shift-click distance measuring; **Probabilistic** (RedisBloom: Bloom/Cuckoo/Count-Min/Top-K/t-digest) and **Time Series** (RedisTimeSeries `TS.INFO` + bucketed `TS.RANGE` chart) each get a dedicated card. Dispatched by the key's type / module.

### Module Browsers
**Dedicated panels for RediSearch (FT.*) and Functions (Lua libraries).**

**RediSearch**: list/inspect indexes, run `FT.SEARCH` / `FT.AGGREGATE` with chips, create / alter / drop from a form. **Functions** (Redis 7+): manage libraries via `FUNCTION LIST/LOAD/DELETE` with a tree-sitter Lua editor, starter templates, direct **`FCALL`** invocation, and `DUMP` / `RESTORE` / `FLUSH` / `STATS`. Both auto-hide when the module / version isn't present.

### Redis Streams
**Browse, live-tail, and manage consumer groups without leaving the GUI.**

Browse entries, **live-tail** new messages (`XREAD BLOCK`, ring-buffered), inspect Consumer Groups & Pending Entries via `XINFO`, and manage groups (`XGROUP CREATE` / `SETID` / `DESTROY`, with a confirm guard on destroy).

### Pub/Sub
**Subscribe to channels and publish messages, with a live message log.**

Open it from the status-bar **Tools** menu or the **⌘K** command palette (same entry as the other server tools). Pattern-based subscriptions (`PSUBSCRIBE`), a `PUBLISH` composer, and incoming messages streamed into a ring-buffered `(time, channel, message)` table — Redis's other messaging primitive alongside Streams. A **Sharded** toggle (Redis 7+) switches to `SSUBSCRIBE` / `SPUBLISH` — on clusters messages are routed by the channel's hash slot instead of broadcast to every node, over a dedicated RESP3 push connection that survives failovers.

---

## 📊 Real-Time Observability

A built-in, GPU-accelerated dashboard for monitoring your instances.

### Live Metrics
**Real-time charts for CPU, memory, latency, clients, throughput and evictions — with 7 days of history.**

Eight headline stat cards (memory · clients · OPS · latency · hit rate · net in / out · evicted keys) sit above GPU-accelerated time-series charts for CPU usage, memory usage, latency, connected clients, total commands processed, output KB/s, key hit rate and evictions. Samples are also persisted locally (one per minute, kept for 7 days), so the **Live / 1h / 24h / 7d** ranges answer "did memory grow overnight?" even across app restarts.

### Memory Analyzer + Recommendations
**Hunt BigKeys, see the TTL distribution, get instant offline health checks plus optional AI tips.**

Sort the Top-N table by **Size / Hottest / Coldest** (`OBJECT FREQ`/`IDLETIME` auto-picked from `maxmemory-policy`), with a **TTL histogram** alongside. On Redis 8+ a **key-size distribution** card reads `INFO keysizes` — exact per-type bucket counts straight from the server (strings by value bytes, containers by element count), no sampling, shown even before a scan runs and summed across cluster masters. The moment a scan finishes, an **offline rule engine** flags issues automatically — big keys, keys that can't be evicted under a `volatile-*` policy, `noeviction`, high fragmentation, many tiny strings that should be a Hash, and memory-dominating prefixes — no config or network needed. One click also sends the report (key *names*, sizes, TTLs only — never values) to any **OpenAI-compatible** endpoint for inline advice in your UI language.

Prefer not to touch production at all? **Analyze RDB** parses a local dump file offline — a streaming parser (every value encoding through Redis 8.6, values length-skipped so multi-GB files parse at I/O speed) feeds the same tables, TTL histogram, and rule engine, with zero commands sent to any server. Sizes are the keys' serialized bytes in the file: not equal to live memory, but a faithful ranking for big-key and prefix hunting. Both the live scan and the file parse show a progress bar, and the prefix / top-key tables export to CSV.

### Performance Diagnostics
**Slow Log ↔ Latency, live MONITOR, clients, and command stats.**

The Performance panel cross-links **Slow Log** entries with `LATENCY` events (±5 s chips jump to the `LATENCY HISTORY` sparkline), aggregates them into a **Top Commands** view ranked by total time consumed (one click filters back to the raw entries), and exports the filtered view to **CSV/JSON** — with a confirm-guarded `SLOWLOG RESET` to start a fresh window; plus live `MONITOR` with keyword filtering, pause, a live events/s badge with an auto-stop rate guard, and CSV/JSON export; client management (`CLIENT LIST` / `CLIENT KILL`, filterable by connection type — normal / replica / master / monitor / pub-sub / blocked — with a confirm-guarded batch kill of everything matching the filter); and a per-command **calls/second** table from `INFO commandstats` with a summary row, idle/self-connection noise filtering, and export.

### Hot Keys
**`HOTKEYS` tracking (Redis 8.6+): which keys burn the CPU and the bandwidth.**

Start a collection (CPU time and/or network bytes, top 10/20/50), watch the two ranked lists fill live, stop, read, reset. Each entry carries its share of the totals as a bar and a percentage, and a click copies the key name. On a cluster the tracking runs on every master at once and the lists merge — slots are disjoint, so nothing double-counts. Start/stop/reset are write-gated (a read-only connection still reads the report), and the panel degrades to an explanatory placeholder on servers without the command.

### Value Search
**Find which key *contains* some text (a guarded, sampled scan).**

Redis can't index values, so this `O(keyspace)` search runs behind guardrails: a mandatory key prefix, a 10k-key / 10s cap (cancellable), and skipped over-1 MiB values — searching string values, hash fields, and list/set/sorted-set members, with each hit showing where it matched. An empty-state guide and result-side key filter keep the flow clear; results are an explicit **sample**, never claimed exhaustive.

### Cluster Health & Management
**Topology tree with replication lag, a slot map, per-node load, and a reshard wizard.**

Inspect Cluster/Sentinel topology as a tree (masters, slot ranges, replicas, per-replica lag from `INFO replication`), then act: `CLUSTER FAILOVER` / `FORGET` / `MEET` / `REPLICATE` and `SENTINEL FAILOVER` / `RESET` / `REMOVE`, each through the confirm dialog with PROD escalation. Three more tabs go deeper: **Slots** maps every master's slot ranges and in-flight migrations plus a **Hot Slots** table (`CLUSTER SLOT-STATS`, Redis 8.2+) — top slots by key count, or by memory / CPU / network I/O on clusters running `cluster-slot-stats-enabled`, each row color-traceable to its owning master; **Load** samples memory / OPS / clients across the masters, and the **Reshard** wizard moves slots between masters — pick a target (and optionally a source, one click from a Load card), preview the plan, then execute a confirm-guarded `CLUSTER RESHARD` with a live per-slot progress bar. Only appears on multi-node deployments.

### Persistence & Keyspace Events
**RDB/AOF status with one-click saves, plus live key-event triage.**

A persistence panel reads `INFO persistence` (last save, AOF growth, fork failures) with one-click `BGSAVE` / `BGREWRITEAOF` (PROD-escalated), per-node status rows on clusters, and a **Policy & paths** card showing the configured `save` rules and AOF settings from `CONFIG GET`. **FLUSHDB / FLUSHALL** sit in the same Tools → Administration group, disabled on a read-only connection and routed through the destructive-command confirm (PROD-escalated), so clearing a dev database no longer means dropping into the CLI. Keyspace notifications parse keyspace/keyevent channels into a filterable `(time, db, key, event, source)` table — "which client just deleted user:42?" — with one-click `notify-keyspace-events` presets, pause, and export.

### Raw INFO Browser
**Every `INFO everything` field in one filterable table.**

The structured panels cover the common fields; this page covers the long tail — `errorstats`, `latencystats` percentiles, fork/COW costs, `sync_full` counters, uptime — without dropping to the terminal. Filter matches section, field, and value; a **Snapshot → Compare** flow shows a field-level diff against an earlier capture (changed / added / removed — field reordering produces no noise); the visible view exports to CSV. On clusters every master is listed with its address in the section column, so filtering one field compares it across nodes. Read-only, and degrades to `INFO all` / plain `INFO` on older servers.

### CONFIG Editor
**A typed `CONFIG GET/SET` editor with inline parameter docs.**

Runtime parameters render with type-aware editors instead of raw strings, grouped by concern (memory & eviction, RDB, AOF, defrag, replication, cluster, network, security, TLS, latency, logging, data-type limits, scripting). The common parameters carry inline help lifted from the official `redis.conf` — what a knob does, right where you change it — in English and Chinese, with English as the fallback for the other UI languages. Writes go through `CONFIG SET` behind the PROD-escalated confirm dialog.

---

## 🔑 Keys & Data Management

### Key Organization
**Namespace tree with TTL chips, favorites, and client-side tags & notes.**

Keys group into a nested tree by `:` with compact TTL chips (green live / red expiring / gray permanent). Bookmark keys, revisit search history, and add colour **tags & notes** — stored in a local redb file, **zero Redis cost**, never leaving the machine. Expanding a folder whose only child is another folder opens the whole chain at once, so a deep `app:user:profile:…` namespace costs one click instead of one per level — and a folder you collapse by hand stays collapsed. The split is smarter than a plain `split(':')`: separators inside a cluster **hash tag** (`user:{tenant:42}:profile`), inside a **quoted JSON blob**, or inside an **ISO-8601 timestamp** are not level boundaries, so such keys stay whole instead of shattering into folders named `55` or `44.487892+00`. The separator, tree depth, scan size and TTL-chip visibility are all overridable per server.

### Key Editing & History
**Rename, per-field TTL, file import/export, bulk paste, and version history.**

Atomic **rename** (`RENAMENX`, overwrite-guarded), per-field **Hash TTL** (`HEXPIRE`/`HPERSIST`, Redis 7.4+), **value file export/import** (binary-safe, `KEEPTTL`), **bulk paste** of TSV/CSV into Hash/List/Set/ZSet, and a client-side **last-10-versions** write history with diff & one-click restore. Deleting a *single* key first stashes its `DUMP` payload into a **local recycle bin** (24h, restorable from Tools → Deleted Keys, TTL preserved; opt-out in Settings — batch deletes are never stashed) — a fat-finger delete on production is no longer final. Oversized String/JSON values are never loaded blindly: the editor shows the size and fetches only on an explicit **Load anyway**.

### Bulk Key Operations
**Multi-select delete, batch TTL, DUMP/RESTORE import/export, auto-refresh.**

Multi-select to delete dozens of keys at once; set / remove TTL across a whole selection or prefix (cluster-safe, PROD-escalated). **Export** is available from the tree selection *or* the status-bar **Tools → Export loaded keys…** entry (current SCAN subset for this connection/db — not a keys-only name list). The export window picks **binary DUMP** (full-fidelity, streams through `RESTORE`) or **readable JSON / CSV** (full values + TTLs for people, downstream tools — and hand-edit-then-re-import), and an optional **key-prefix filter** with a live matched/total count narrows the job before it starts. **Imports accept all three formats**, sniffed from file content rather than the extension: the binary bundle streams through `RESTORE`, while JSON/CSV files are parsed up front (a hand-edit typo fails with a key-addressed message before anything is written) and written back with type-native commands, TTLs restored — entries the export marked `truncated` are refused rather than silently importing partial data. Auto-refresh keeps the tree current on fast-changing instances.

### Cross-Server Tools
**Copy or diff a key, or diff full configs, between two servers.**

**Copy** a key with value + TTL (`DUMP`/`RESTORE`), **diff** a string key against the same key elsewhere (side-by-side), or **diff** two servers' `CONFIG GET *` (striped table of only what differs). Built for "why does prod differ from staging?".

---

## 🔐 Security & Privacy

### Privacy-First
**Your data and credentials stay on your machine; nothing is phoned home.**

Tags, notes, favorites and search history live in a **local redb file** — zero Redis cost, never sent anywhere. Connection secrets are **encrypted at rest**, and sharing a connection as JSON **strips credentials** by default. The optional AI analysis sends only key **names, sizes and TTLs — never your values**, and only to the OpenAI-compatible endpoint *you* configure. The custom script viewer runs locally through your own shell. **No telemetry, no accounts, no cloud.**

### Connection Safety
**Environment tags + confirm dialogs that escalate on production.**

Tag each server with a preset environment — **Dev / UAT / Prod** — shown as a colored chip in the sidebar and status bar; the title bar also shows the active **db** and a quiet **Prod** badge when the connection is high-risk. The status-bar **DB dropdown** lists each database with its **key count** (from `INFO keyspace`) so you can pick a non-empty db at a glance, and each server can **pin the database it opens on** (leave it empty to reopen the last one used). Lock any connection **read-only** — globally in its settings, or just for the current session from the status bar. Destructive actions (`FLUSHALL` / `FLUSHDB`, `CONFIG SET` / `REWRITE` / `RESETSTAT`, `SHUTDOWN`, `DEBUG`, `SCRIPT FLUSH`, `KEYS *`, a `DEL` of 50+ keys, key/server delete, `XGROUP DESTROY`, cluster ops…) are intercepted with a confirm dialog that escalates its wording on a **Prod** server. A per-server **Confirm Writes** switch widens that net to *every* write command on connections where nothing should happen by accident.

### ACL Management (Redis 6+)
**GUI for the full ACL lifecycle.**

List users, view flags / commands / key patterns / channel rules, and edit via quick presets (Full / Read-only / Disabled) plus toggleable chips for command categories and wildcards.

### Secure Connections & Groups
**TLS/SSL and SSH tunnels, with named, shareable server groups.**

Full **TLS/SSL** (custom CA, client certs) and **SSH tunneling** (password, private key, agent). When a connection fails, the server form's **Diagnose** button runs staged diagnostics — DNS → TCP → SSH auth → SSH tunnel → TLS → AUTH → PING — pinpointing the failing layer with a targeted fix hint instead of one opaque error. Organize connections into named, collapsible **groups** and reorder them. Export any selection of connections as JSON (credentials stripped by default) — or set a passphrase to emit a compact, portable **share token** (`ZEDIS1.…`, Argon2id + AES-256-GCM) that only opens with that passphrase; the import dialog detects the token and prompts for it. Migrate in by pasting a `redis://` URI, a **Redis Insight** database export, an **Another Redis Desktop Manager** export (`connections.ano`), or a **Tiny RDM** `connections.yaml` (the file inside its exported zip) — every connection lands at once, with groups, TLS and SSH settings mapped over. Those clients store secrets in the clear; once imported they live in Zedis's per-machine encrypted store.

### Link Health & Failover
**A 2-second heartbeat that tells you the truth about the connection — and recovers on its own.**

Every connection is pinged on a 2-second heartbeat; the status-bar dot reads **Connected / Reconnecting / Offline** (offline only after repeated failures, so one slow round-trip doesn't cry wolf), and the dot itself is the disconnect / reconnect button. Server-state replies — a dropped link, `LOADING`, `BUSY`, `READONLY`, `MASTERDOWN`, `CLUSTERDOWN` — are folded into **one throttled, localized notice** instead of a wall of raw errors, and the stale pooled client is dropped so the next call rebuilds it: on Sentinel and Cluster that re-runs discovery, so a **failover reconnects to the new master by itself**. When the heartbeat recovers, the selected key is reloaded and the panels refresh — you get "Connection restored", not a screen of stale data. A replica connection is labelled as such, since writes there come back `READONLY`.

---

## 🧭 Limited & Non-Standard Servers

### Capability Probe
**Panels and buttons grey out *with the reason* instead of failing.**

Proxies (Twemproxy / Codis / Envoy) answer `unknown command` outside their whitelist, managed clouds (ElastiCache / Azure Cache / Tair) rename or ACL-deny the administrative ones, and Redis-compatible servers (Valkey / Dragonfly / KeyDB / Kvrocks / Garnet) ship different subsets. Rather than a hand-maintained table per brand, Zedis **probes** the connection once right after connect — in the background, on a dedicated connection, read-only probes only (mutating commands are checked through `COMMAND INFO` + `ACL DRYRUN`, never executed) — and caches the verdict per server.

Everything downstream reads that matrix. A panel whose hard dependency is missing renders an explanatory placeholder instead of the panel; a button whose command is unusable is disabled with a suffix naming it (`CONFIG GET · not supported by this server`, `SLOWLOG GET · denied for this user (NOPERM)`). The key editor keeps working on a server without `SCAN` — type an exact key name and press Enter, and recent keys (**⌘P**) still work. A runtime `NOPERM` / `unknown command` reply feeds back into the same matrix: one localized notice, then the UI quietly degrades instead of repeating the error.

**Tools → Server capabilities** shows the full matrix — every probed command with its verdict — plus a **Re-probe** button for when an administrator has just granted you the permission.

---

## ⌨️ Productivity

### Workspace Tabs
**Multiple connections side by side — Cmd/Ctrl-click a server to open it in a new tab.**

Each tab keeps its own connection and view state (key tree expansion, scroll, selection survive tab switches). The tab strip appears only with two or more tabs (max 8): click (or **⌘1–8**) activates, middle-click or × closes, drag reorders, and right-click offers close / close-others / close-to-the-right. Background tabs relax their heartbeat to one refresh per 30 seconds, and the open-tab list is restored on the next launch with background tabs reconnecting lazily on first activation.

### Multi-Database Key Search
**⌘⇧F — find a key across many connections at once.**

An overlay that searches a key name across a chosen scope: every open tab's connection, one server group, or an explicit checkbox set of servers (with select-all; both the scope and the per-server scan limit are remembered). The cheap exact lookup runs first and shows instantly; if nothing matches exactly, a capped `SCAN` runs automatically — and when exact hits exist, the scan waits behind a button so the fast answer is never delayed. Clicking a hit jumps straight to that server, db, and key.

### Command Palette & Shortcuts
**⌘K fuzzy navigation, ⌘P recent keys, and a ⌘/ keyboard-shortcut reference.**

**⌘K** fuzzy-searches servers, panels, and the active connection's loaded keys (arrows to move, Enter to run, Esc to dismiss). **⌘P** is the quick-open for the current connection's recently opened keys — the Zed / VS Code gesture, available from tool pages too. **⌘/** opens a read-only, grouped overlay of every hotkey with per-platform symbols: ⌘N new key, ⌘S save, ⌘R reload the tree, ⌘⇧R reload the value, ⌘T set TTL, ⌘⌫ delete, ⌘E rename, ⌘F filter, ⌘J terminal, ⌘1–8 workspace tabs, Esc to step back.

### Integrated CLI & Workbench
**redis-cli terminal with completion, a multi-line Batch mode, and an AI command assistant.**

Version-aware command completion with inline argument/summary hints, each suggestion linking to its reference page on redis.io. Command history is kept **per server** and persisted locally: ↑/↓ walks it, and **Ctrl+R** opens an incremental reverse search over it. A pasted line keeps working when it still carries a leading `redis-cli`, and a blocking command (`BLPOP` / `BRPOP` / `BZPOPMIN`, `XREAD … BLOCK`, `WAIT`, …) is refused up front rather than parking the shared connection. A one-click **Batch** mode swaps the REPL for a multi-line editor — one command per line, run with `⌘`/`Ctrl`+`Enter` (dangerous lines still route through the confirm dialog). Type **`? <question>`** to ask the configured AI endpoint for the matching command in plain language — the suggestion lands in the input box for review (never auto-executed, so the danger-confirm and read-only gates apply unchanged), with a short explanation in your UI language. Only the question and server *metadata* (version, deployment, modules) are sent — never key values.

### Lua Script Library
**Save, reuse, and EVALSHA-run Lua scripts with hit-rate stats.**

A local library of named Lua scripts (source + precomputed SHA1) with starter templates, one-click **EVALSHA-first** execution, saved `KEYS` / `ARGS` defaults for one-click re-runs, and lifetime hit/miss counters to spot scripts being flushed from Redis's cache — plus cache control (**Warm** = `SCRIPT LOAD` without executing, and a guarded `SCRIPT FLUSH`) and library import/export. (Distinct from **Functions** — that manages server-side `FUNCTION` libraries.)

### First Launch & Onboarding
**A welcome card on the first run, one-time hints on the panels that need context.**

The first launch shows a three-step welcome card — add a server, browse decoded values, open the command palette. Panels with a learning curve (Topology, Memory Analyzer) show a one-time banner explaining what they sample and how to read the result. Every hint appears once and never comes back.

### Diagnostics & Recovery
**A one-click diagnostics bundle, crash reports, and a repair path for damaged local files.**

The title-bar menu's **Export Diagnostics** writes one zip to Downloads: a summary, your `zedis.toml` and `redis-servers.toml` with secrets redacted, the newest log files, and any crash reports — everything an issue needs, none of it collected by hand. If a session ends in a crash, the next launch surfaces a card pointing at the saved report and its stack trace. And if the config or the local database file is damaged, Zedis opens a recovery window offering **Back up & rebuild** instead of refusing to start. Logs roll to files under the config directory (**Open Logs Folder** in the same menu) and are pruned after ~3 months.

### Staying Current & System Integration
**An opt-out update check with a checksum-verified download, a system tray, and your own proxy.**

Zedis checks GitHub for a newer release on startup — at most once every two days, skippable per version, and switchable off entirely in Settings (there is also a manual **Check for Updates**). When one is found, a chip appears in the title bar; downloading it verifies the asset's **SHA-256** against the release manifest before anything runs. On **macOS the install then completes in place** — the DMG mounts silently, the bundle identifier is verified, the new app is copied over the old one (which is parked in temp, never deleted under the running process) and a **one-click Restart** relaunches into the new version; anything that blocks that path falls back to the classic drag window. Windows hands the verified MSI to its installer, Linux to the desktop handler. Progress shows both in the dialog and as a percentage on the chip.

On macOS and Windows an optional **system tray** icon shows the active connection, its memory and OPS, and offers quick-connect to any configured server. All of the app's own outbound requests (update check + AI) follow the OS system proxy by default and honour an explicit **HTTP / SOCKS5 proxy** you set in Settings — or `none` to force a direct connection.
