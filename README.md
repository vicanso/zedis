[中文](./README_zh.md) | English

<h1 align="center">Zedis</h1>

<p align="center">
  <strong>A High-Performance, GPU-Accelerated Redis GUI Client Built with Rust 🦀 and GPUI ⚡️</strong>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License"></a>
  <a href="https://x.com/tree0507"><img src="https://img.shields.io/twitter/follow/tree0507?style=social" alt="Twitter Follow"></a>
  <img src="https://img.shields.io/github/downloads/vicanso/zedis/total" alt="Downloads">
  <a href="https://www.blazingly.fast"><img src="https://www.blazingly.fast/api/badge.svg?repo=vicanso%2Fzedis" alt="blazingly fast"></a>
</p>

<p align="center">
  <video src="https://github.com/user-attachments/assets/217cc0a7-cc7e-40d0-ac7e-1ec61c36a02b" autoplay loop muted playsinline width="100%"></video>
</p>

---

## 🤔 Why Zedis?

Tired of Electron-based Redis clients that eat gigabytes of RAM just to display a JSON string, or freeze entirely when you accidentally click a key with 100,000 elements? We were too. 

**Zedis** is built from the ground up for developers who demand native performance. Powered by **GPUI** (the same revolutionary rendering engine behind the [Zed Editor](https://zed.dev)), Zedis delivers a native, buttery-smooth 60+ FPS experience with a minimal memory footprint—even when navigating massive databases.

## ✨ Killer Features

### 🚀 Blazingly Fast & Native

<details><summary><b>GPU Rendering</b> — every pixel drawn on the GPU; zero-lag scrolling, instant tab switches.</summary>

Minimal memory footprint even when navigating massive databases.
</details>

<details><summary><b>Virtual Lists</b> — browse instances with millions of keys without ever blocking the UI.</summary>

Virtual scrolling combined with `SCAN` iteration keeps the interface responsive no matter how large the keyspace.
</details>

<details><summary><b>Cross-Platform</b> — native on macOS, Windows, and Linux, with Light / Dark / System themes.</summary>

A truly native feel across all three desktop platforms.
</details>

### 🧠 Smart Data Viewer

Zedis auto-detects (`ViewerMode::Auto`) and formats payloads on the fly.

<details><summary><b>Auto-Decompression</b> — transparently unpacks LZ4, SNAPPY, GZIP, and ZSTD.</summary>

Compressed values are unpacked in place so you read the real payload, not a blob.
</details>

<details><summary><b>Rich Content Decoding</b> — JSON/RedisJSON, Protobuf, MessagePack, Unix timestamps, media & hex.</summary>

- **JSON & RedisJSON**: Full read/write with pretty-printing and syntax highlighting. Computes RFC 7396 Merge Patch diffs to send minimal `JSON.MERGE` commands instead of full overwrites. **JSONPath filtering** works on plain string keys too — query nested fields with `$.user.email` or `$.items[?(@.price > 100)]` without the RedisJSON module.
- **Protobuf & MessagePack**: Zero-config binary deserialization into readable JSON-like output.
- **Unix Timestamps**: A string that is exactly a 10-digit (seconds) or 13-digit (milliseconds) epoch is auto-recognized and previewed as local + UTC dates — the raw value stays intact and editable.
- **Media & Hex**: Native preview for images (`PNG`, `JPG`, `WEBP`, `SVG`, `GIF`) and a fully editable hex view for raw binary — paste in hex (whitespace, commas, and `0x` prefixes tolerated) and Zedis decodes it back to bytes on save. Bytes-per-row auto-adapts (16 / 24 / 32) to viewport width.
</details>

<details><summary><b>Custom Script Viewer</b> — pipe any value through an external shell command for custom decoding.</summary>

Configure a command template with placeholders (`{KEY}`, `{VALUE}`, `{HEX}`, `{RAW_FILE}`); Zedis runs it via `sh -c` (Unix/macOS) or `cmd /c` (Windows) and shows stdout as the formatted value. Perfect for base64, custom binary protocols, or any tool in your `$PATH`. Key patterns matched by exact / prefix / suffix / regex rules per server.
</details>

<details><summary><b>Hash Field-Level TTL</b> (Redis 7.4+) — per-field expiry via HEXPIRE / HPERSIST.</summary>

Set individual expiry times on specific hash fields — no need to restructure your data model just to expire a subset of fields.
</details>

<details><summary><b>Redis Streams</b> — browse, live-tail, and manage consumer groups without leaving the GUI.</summary>

Browse entries, **live-tail** new messages in real time (`XREAD BLOCK`, ring-buffered so a hot stream never blows up memory), inspect Consumer Groups & Pending Entries via `XINFO`, and manage groups (`XGROUP CREATE` / `SETID` / `DESTROY`, with a confirm guard on destroy).
</details>

<details><summary><b>Bulk Paste</b> — add many Hash/List/Set/ZSet entries from TSV or CSV in one shot.</summary>

Paste TSV or CSV (tab preferred, comma fallback, per-cell trimmed) and Zedis fans the rows out through the normal `HSET` / `RPUSH` / `SADD` / `ZADD` paths.
</details>

<details><summary><b>Pub/Sub</b> — subscribe to patterns and publish, live, from the GUI.</summary>

Built-in subscribe/publish interface — subscribe to channel patterns, receive live messages, and publish directly without switching to `redis-cli`.
</details>

<details><summary><b>Local Write History</b> — last 10 versions of every string value, with diff & restore.</summary>

Kept in memory per key — one click rolls a previous version back into the editor. Purely client-side (no Redis storage cost), scoped to the session, cleared on key delete or server switch. A split-button **Diff** sits next to Restore — main click shows a side-by-side diff against the previous version, the dropdown picks any older version. JSON keys also get an RFC 7396 merge-patch block equivalent to the `JSON.MERGE` the Save path would send.
</details>

<details><summary><b>RediSearch Browser</b> (module) — a dedicated panel for the FT.* command family.</summary>

List indexes via `FT._LIST`, inspect schema and stats from `FT.INFO` (including indexing progress and `type mismatch` failure counters that explain "0 docs" mysteries), run raw `FT.SEARCH` with `HIGHLIGHT` / `RETURN` / `LIMIT` chips, or switch to `FT.AGGREGATE` with single-stage `GROUPBY` + `REDUCE` (`COUNT`, `SUM`, `AVG`, `QUANTILE`, `TOLIST`...). Create indexes from a structured form (HASH / JSON, prefixes, per-field SORTABLE / NOSTEM / NOINDEX), or alter / drop existing ones. Auto-hidden when the module isn't loaded.
</details>

<details><summary><b>Functions Editor</b> (Redis 7+) — manage server-side Lua libraries with syntax highlighting.</summary>

Manage libraries through `FUNCTION LIST / LOAD / DELETE`. Cards show each library's engine, registered functions, and flags (`no-writes`, `allow-oom`, ...); click to expand a read-only Lua viewer with tree-sitter highlighting. Edit and "New library" share one Lua editor with line numbers, indent guides, and an explicit `REPLACE` toggle. Auto-hidden on Redis 6.x and earlier.
</details>

<details><summary><b>Time Series Viewer</b> (RedisTimeSeries) — TS.INFO metadata plus a bucketed TS.RANGE chart.</summary>

Selecting a `TSDB-TYPE` key opens a dedicated chart — `TS.INFO` surfaces total samples / memory / retention / chunk count and labels, while `TS.RANGE` (server-side `AVG` aggregation, bucketed to ~240 points so even a multi-million-sample series stays responsive) feeds a GPU-rendered line chart with `15m / 1h / 6h / 24h / 7d / All` toggles. Self-gates by the key's existence.
</details>

<details><summary><b>Probabilistic Structures</b> (RedisBloom) — Bloom / Cuckoo / Count-Min / Top-K / t-digest viewers.</summary>

Keys that used to render as opaque binary now open a dedicated read-only viewer showing their `*.INFO` stats (capacity, size, error rate, ...). Top-K also lists current heavy hitters (`TOPK.LIST … WITHCOUNT`) and t-digest adds min / max / p50 / p90 / p99 (`TDIGEST.QUANTILE`). Dispatched by the key's module TYPE.
</details>

<details><summary><b>Vector Set + KNN</b> (Redis 8) — metadata plus interactive nearest-neighbour search.</summary>

Native `vectorset` keys open a viewer with `VINFO` / `VCARD` / `VDIM` metadata, a `VRANDMEMBER` sample, and an **interactive KNN search**: type (or click) an element to run `VSIM … WITHSCORES` and see its ranked nearest neighbours; click a neighbour to re-search from it and walk the HNSW graph hop by hop. Read-only.
</details>

<details><summary><b>Geo Map</b> — plot a geospatial sorted set on a tile-less radar canvas.</summary>

Flip any sorted set into **Map** mode to plot its members on a dark, GPU-rendered Web Mercator canvas — no map tiles, no network. `GEOPOS` decodes the geohash, fit-to-bounds frames the data, and scroll-zoom / drag-pan / hover (with a live coordinate readout and a side list that cross-highlights) make it a fast GEO debugging "radar". Enter a center lon/lat + radius to run `GEOSEARCH` and highlight the matching points (the rest dim out) with the radius circle drawn on the canvas. Capped for responsiveness; invalid / non-geo members are listed separately.
</details>

### 📊 Real-Time Observability

A built-in, GPU-accelerated dashboard for monitoring your instances.

<details><summary><b>Live Metrics</b> — real-time charts for CPU, memory, and network I/O.</summary>

Beautifully rendered, GPU-accelerated time-series charts.
</details>

<details><summary><b>Memory Analyzer + AI Advice</b> — hunt BigKeys, see the TTL distribution, get AI optimization tips.</summary>

Sort the Top-N table by **Size / Hottest / Coldest** — `OBJECT FREQ` or `OBJECT IDLETIME` is auto-selected from the server's `maxmemory-policy`. The same SCAN feeds a **TTL distribution histogram** (`<1m / <1h / <1d / <7d / ≥7d / No TTL`) alongside the BigKey tables — spot the "3 AM expiry cliff", see what share of keys is `PERSIST` (a memory-leak red flag), and read the estimated cluster-wide count even at `ratio < 1.0`. One click on **AI Analysis** turns the report into a Markdown summary and sends it to any **OpenAI-compatible** endpoint (Base URL + API key in Settings, key stored encrypted) for actionable advice rendered inline — only key *names*, sizes and TTLs are sent, never values. Works with OpenAI, Claude (via Anthropic's OpenAI-compatible endpoint, `https://api.anthropic.com/v1/`), and other compatible providers; advice is returned in the app's UI language.
</details>

<details><summary><b>Cluster Health</b> — inspect cluster/Sentinel topology as a tree with replication lag.</summary>

Hover the node indicator to see masters with their slot ranges, replicas grouped beneath, plus per-replica replication lag (bytes + seconds + link state) parsed from `INFO replication`.
</details>

<details><summary><b>Deep Diagnostics</b> — Slow Log ↔ Latency cross-linking, live MONITOR, client management.</summary>

Track Slowlogs, monitor live `MONITOR` streams with keyword filtering, and manage active clients (`CLIENT LIST/KILL`). The Performance panel cross-links Slow Log entries with `LATENCY` events: each slow command shows a chip naming the nearest fork/AOF/expire event within ±5 s, one click jumps to that event's `LATENCY HISTORY` sparkline; the reverse chip narrows the Slow Log to that window. Disabled `latency-monitor-threshold` can be flipped on (default 100 ms) from the panel (PROD servers go through the confirm dialog).
</details>

<details><summary><b>Persistence Management</b> — RDB/AOF status with one-click BGSAVE / BGREWRITEAOF.</summary>

A dedicated panel reads `INFO persistence` continuously — last RDB save time, changes since last save, AOF size with growth ratio against the rewrite baseline, plus per-fork failure banners. One-click `BGSAVE` / `BGREWRITEAOF` (cluster mode fans out to every master) through a confirm dialog with PROD escalation; buttons auto-disable while a fork runs or when read-only.
</details>

<details><summary><b>Keyspace Notifications</b> — live key-event triage from keyspace / keyevent channels.</summary>

"Which client just deleted user:42?" answered without leaving the GUI. Channels are parsed into a `(time, db, key, event, source)` table with severity-colored verbs, a ring-buffered 1000-row history, and post-subscription chip filters (event-type multi-select + key substring). When `notify-keyspace-events` is empty, an inline banner offers a one-click "Enable (AKE)" — PROD servers detour through the confirm dialog first.
</details>

### 🛡️ Enterprise-Grade Security & Productivity

<details><summary><b>Command Palette</b> (⌘K) — keyboard-first fuzzy search over servers and panels.</summary>

Switch connections or jump to any panel (Metrics, Performance, Memory, Config, ACL, RediSearch, Functions, Lua Scripts, Settings...) without the mouse. Arrow keys to move, Enter to run, Esc to dismiss.
</details>

<details><summary><b>Server Groups & Ordering</b> — named, collapsible groups; reorder; share as JSON.</summary>

Organize connections into named groups, reorder cards within a group, and share a single connection as JSON (credentials stripped by default — opt in to include secrets for personal backups). Collapse state persists across sessions.
</details>

<details><summary><b>Read-Only Mode</b> — lock a connection to prevent accidental writes.</summary>

Guards against accidental writes in production environments.
</details>

<details><summary><b>ACL Management</b> (Redis 6+) — GUI for the full ACL lifecycle.</summary>

List users, view flags / commands / key patterns / channel rules, and edit via a quick-preset toolbar (Full access / Read-only / Disabled) plus toggleable chips for command categories (`+@read`, `-@dangerous`, ...) and key/channel wildcards.
</details>

<details><summary><b>Connection Safety</b> — environment tags + confirm dialogs that escalate on production.</summary>

Tag each server (PROD / DEV / STAGING) with a colored chip surfaced in the sidebar and status bar. Dangerous commands (`FLUSHALL`, `FLUSHDB`, `CONFIG SET`, `SHUTDOWN`, `DEBUG`, `SCRIPT FLUSH`, `KEYS *`, batch `DEL`...) are intercepted with a confirm dialog that escalates wording on production-tagged servers.
</details>

<details><summary><b>Data Import / Export</b> — DUMP/RESTORE-based, binary-safe across all key types.</summary>

Dump any selection of keys (multi-select, single key, or whole folder prefix) to a framed binary file with magic header + CRC32, then restore on another instance.
</details>

<details><summary><b>Advanced Tunnels</b> — TLS/SSL (custom CA, client certs) and SSH tunneling.</summary>

Full support for TLS/SSL and SSH Tunneling (Password, Private Key, SSH Agent).
</details>

<details><summary><b>Integrated CLI & Workbench</b> — redis-cli terminal with completion plus a multi-line Batch mode.</summary>

Version-aware command completion and inline argument/summary hints. A one-click **Batch** mode swaps the single-line REPL for a multi-line editor — one command per line, run the whole script with `⌘`/`Ctrl`+`Enter` (dangerous lines still route through the confirm dialog).
</details>

<details><summary><b>Namespace Tree View</b> — keys grouped into a nested tree by <code>:</code>, with TTL chips.</summary>

Right-click any folder to refresh its contents or delete all keys under that prefix. Each leaf key carries a compact TTL chip — green for live TTL, red when expiring within 2 minutes, gray for permanent keys.
</details>

<details><summary><b>Multi-Select & Batch Delete</b> — mark and delete dozens of keys at once.</summary>

Toggle multi-select mode to delete many keys without writing a single command.
</details>

<details><summary><b>Key Favorites & Search History</b> — bookmark keys, revisit recent searches.</summary>

Bookmark frequently used keys for instant access and revisit recent searches from a persistent history panel.
</details>

<details><summary><b>Key Tags & Notes</b> <i>(client-side only)</i> — colour-tag and annotate keys, stored locally.</summary>

Annotate any key with a colour tag (red / orange / yellow / green / blue / purple) and a free-form note — everything lives in the local redb file, **zero Redis storage cost**, never leaves the machine. Tagged rows carry a 4 px colour bar on the left edge; hover reveals the note. Edit via right-click → "Edit tag & note…". Each tag colour can be used as a one-click filter from the tree's ⋯ menu; the filter sources keys directly from local metadata rather than the in-flight SCAN snapshot, so every tagged key shows up immediately. After a save, only the affected row re-renders. Schema is versioned on disk for future migration.
</details>

<details><summary><b>Auto-Refresh</b> — periodic key-tree refresh for fast-changing instances.</summary>

Configure an automatic refresh interval to keep the tree in sync with a live instance.
</details>

---

## 📦 Installation

Ready to feel the speed? Install Zedis via your favorite package manager:

### macOS
The recommended way to install Zedis is via Homebrew:

```bash
brew install --cask zedis
```

### Windows

```bash
scoop bucket add extras
scoop install zedis
```

### Linux (Arch)

```bash
yay -S zedis-bin
```

### Cargo (Cross-Platform via Source)

```bash
cargo install --locked zedis-gui
```

---

## 🤝 Contributing

We want to make Zedis the ultimate Redis client, and we'd love your help! Whether it's adding new features, translating the UI, or fixing bugs, all contributions are welcome.

Open an issue or a PR to get started. By submitting a PR, you agree to our lightweight [Contributor License Agreement (CLA)](./CLA.md).

## 📄 License

Zedis is open-source software licensed under the [Apache License, Version 2.0](./LICENSE).
