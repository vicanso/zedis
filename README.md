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
- **GPU Rendering**: Every pixel is drawn on the GPU. Experience zero-lag scrolling and instant tab switching.
- **Virtual Lists**: Fearlessly browse instances with millions of keys. Virtual scrolling combined with `SCAN` iteration ensures your UI never blocks.
- **Cross-Platform**: A truly native feel across **macOS**, **Windows**, and **Linux**, complete with Light, Dark, and System themes.

### 🧠 Smart Data Viewer
Stop manually decoding your data. Zedis automatically detects (`ViewerMode::Auto`) and formats your payloads on the fly:
- **Auto-Decompression**: Transparently unpacks `LZ4`, `SNAPPY`, `GZIP`, and `ZSTD` data.
- **Rich Content Decoding**:
  - **JSON & RedisJSON**: Full read/write support with pretty-printing and syntax highlighting. Smartly computes RFC 7396 Merge Patch diffs to send minimal `JSON.MERGE` commands instead of heavy document overwrites. **JSONPath filtering** works on plain string keys too — query nested fields with `$.user.email` or `$.items[?(@.price > 100)]` without needing the RedisJSON module.
  - **Protobuf & MessagePack**: Zero-config binary deserialization into readable JSON-like formats.
  - **Media & Hex**: Native preview for images (`PNG`, `JPG`, `WEBP`, `SVG`, `GIF`) and a fully **editable** hex view for raw binary — paste in hex (whitespace, commas, and `0x` prefixes are tolerated) and Zedis decodes it back into bytes on save. Bytes-per-row auto-adapts (16 / 24 / 32) to viewport width.
- **Custom Script Viewer**: Pipe any Redis value through an external shell command for fully custom decoding. Configure a shell command template with placeholders (`{KEY}`, `{VALUE}`, `{HEX}`, `{RAW_FILE}`) and Zedis runs it via `sh -c` (Unix/macOS) or `cmd /c` (Windows), displaying stdout as the formatted value. Perfect for base64, custom binary protocols, or any tool in your `$PATH`. Key patterns are matched by exact, prefix, suffix, or regex rules per server.
- **Hash Field-Level TTL** (Redis 7.4+): Set individual expiry times on specific hash fields using `HEXPIRE` / `HPERSIST` — no need to restructure your data model just to expire a subset of fields.
- **Redis Streams**: Full Stream support — browse entries, **live-tail** new messages in real time (`XREAD BLOCK`, ring-buffered so a hot stream never blows up memory), inspect Consumer Groups & Pending Entries via `XINFO`, and manage groups (`XGROUP CREATE` / `SETID` / `DESTROY`, with a confirm guard on destroy) — all without leaving the GUI.
- **Bulk Paste**: Add many entries to a Hash / List / Set / ZSet in one shot — paste TSV or CSV (tab preferred, comma fallback, per-cell trimmed) and Zedis fans the rows out through the normal `HSET` / `RPUSH` / `SADD` / `ZADD` paths.
- **Pub/Sub**: Built-in subscribe/publish interface. Subscribe to channel patterns, receive live messages in real time, and publish directly from the GUI without switching to `redis-cli`.
- **Local Write History**: The last 10 versions of every string value you save are kept in memory per key — one click rolls a previous version back into the editor for review or restore. Purely client-side (no Redis storage cost), scoped to the session, and cleared automatically on key delete or server switch. A **split-button Diff** sits next to the Restore dropdown — main click shows a side-by-side diff against the previous version, the dropdown lets you pick any older version. JSON keys also get an RFC 7396 merge-patch block below the panes, equivalent to the `JSON.MERGE` payload the Save path would send.
- **RediSearch Browser** (module): A dedicated panel for the `FT.*` command family — list indexes via `FT._LIST`, inspect schema and stats from `FT.INFO` (including indexing progress and `type mismatch` failure counters that explain "0 docs" mysteries), run raw `FT.SEARCH` with `HIGHLIGHT` / `RETURN` / `LIMIT` chips, or switch to `FT.AGGREGATE` with single-stage `GROUPBY` + `REDUCE` (`COUNT`, `SUM`, `AVG`, `QUANTILE`, `TOLIST`...). Create indexes from a structured form (HASH / JSON, prefixes, per-field SORTABLE / NOSTEM / NOINDEX), or alter / drop existing ones — all without leaving the app. Auto-hidden when the RediSearch module isn't loaded.
- **Functions Editor** (Redis 7+): Manage server-side Lua libraries through `FUNCTION LIST / LOAD / DELETE`. Cards show each library's engine, registered functions, and flags (`no-writes`, `allow-oom`, ...) at a glance; click to expand a read-only Lua viewer with proper tree-sitter syntax highlighting. Edit and "New library" share one Lua editor with line numbers, indent guides, and an explicit `REPLACE` toggle for safe iteration. Auto-hidden on Redis 6.x and earlier.

### 📊 Real-Time Observability
Transform how you monitor your Redis instances with a built-in, GPU-accelerated dashboard.
- **Live Metrics**: Beautifully rendered, real-time charts for CPU, Memory, and Network I/O.
- **Memory Analyzer**: Visually hunt down **BigKeys** and optimize storage efficiency to prevent OOMs. Sort the Top-N table by **Size / Hottest / Coldest** — `OBJECT FREQ` or `OBJECT IDLETIME` is auto-selected based on the server's `maxmemory-policy`, so you can find the keys actually worth caching (or evicting) in a single click. The same SCAN run also feeds a **TTL distribution histogram** (`<1m / <1h / <1d / <7d / ≥7d / No TTL`) shown in the same view alongside the BigKey tables — no tab switching — spot the "3 AM expiry cliff" pattern at a glance, see what share of keys is `PERSIST` (the memory-leak red flag), and read the estimated cluster-wide count even when sampling with `ratio < 1.0`.
- **Cluster Health**: Hover the node indicator to inspect cluster topology as a tree — masters with their slot ranges, replicas grouped beneath, plus per-replica replication lag (bytes + seconds + link state) parsed from `INFO replication`. Works for Cluster and Sentinel deployments.
- **Deep Diagnostics**: Track Slowlogs, monitor live `MONITOR` streams with powerful keyword filtering, and manage active clients (`CLIENT LIST/KILL`) via an intuitive GUI. The Performance panel cross-links Slow Log entries with `LATENCY` events: each slow command shows a chip naming the nearest fork/AOF/expire event within ±5 s and one click jumps to that event's GPU-rendered `LATENCY HISTORY` sparkline; the reverse chip on every Latency row narrows the Slow Log to commands fired during that window. Disabled `latency-monitor-threshold` can be flipped on (defaults to 100 ms) directly from the panel via a single button (PROD-tagged servers go through the standard confirm).
- **Persistence Management**: A dedicated panel reads `INFO persistence` continuously — last RDB save time, changes since last save, AOF current size with growth ratio against the rewrite baseline, plus per-fork failure banners. One-click `BGSAVE` / `BGREWRITEAOF` (cluster mode fans out to every master) routed through a confirm dialog with PROD-tag escalation, buttons auto-disable while a fork is running (showing elapsed time) or when the connection is read-only.
- **Keyspace Notifications**: One-click tap into `__keyspace@*__:*` and `__keyevent@*__:*` for live key-event triage — "which client just deleted user:42?" answered without leaving the GUI. Channel names are parsed into a `(time, db, key, event, source)` table with severity-colored event verbs, a ring-buffered 1000-row history, and post-subscription chip filters (event-type multi-select + key substring) that narrow the view without cycling the connection. When `notify-keyspace-events` is empty an inline banner offers a single-click "Enable (AKE)" button — PROD-tagged servers detour through the standard confirm dialog before flipping the runtime config.

### 🛡️ Enterprise-Grade Security & Productivity
- **Command Palette** (`⌘K`): Keyboard-first fuzzy search over servers and navigation commands — switch connections or jump to any panel (Metrics, Performance, Memory, Config, ACL, RediSearch, Functions, Lua Scripts, Settings...) without touching the mouse. Arrow keys to move, Enter to run, Esc to dismiss.
- **Server Groups & Ordering**: Organize connections into named, collapsible groups, reorder cards within a group, and share a single connection as JSON (credentials stripped by default — opt in to include secrets for personal backups). Collapse state persists across sessions.
- **Read-Only Mode**: Lock down connections to prevent accidental writes in production environments.
- **ACL Management** (Redis 6+): GUI for the full `ACL` lifecycle — list users, view flags / commands / key patterns / channel rules, and edit via a quick-preset toolbar (Full access / Read-only / Disabled) plus toggleable chips for command categories (`+@read`, `-@dangerous`, ...) and key/channel wildcards.
- **Connection Safety**: Tag each server (PROD / DEV / STAGING) with a colored chip surfaced in the sidebar and status bar. Dangerous commands (`FLUSHALL`, `FLUSHDB`, `CONFIG SET`, `SHUTDOWN`, `DEBUG`, `SCRIPT FLUSH`, `KEYS *`, batch `DEL`...) are intercepted with a confirm dialog that escalates wording on PROD-tagged servers.
- **Data Import / Export**: Dump any selection of keys (multi-select, single key, or whole folder prefix) to a framed binary file with magic header + CRC32, then restore on another instance — built on `DUMP` / `RESTORE` so binary-safe across all key types.
- **Advanced Tunnels**: Full support for TLS/SSL (custom CA, client certs) and SSH Tunneling (Password, Private Key, SSH Agent).
- **Integrated CLI**: A built-in terminal for `redis-cli` allows you to leverage your command-line muscle memory without leaving the app.
- **Namespace Tree View**: Automatically groups keys separated by colons (`:`) into a nested directory tree. Right-click any folder to refresh its contents or delete all keys under that prefix in one action. Each leaf key carries a compact TTL chip — green for live TTL, red when expiring within 2 minutes, gray for permanent keys.
- **Multi-Select & Batch Delete**: Toggle multi-select mode to mark and delete dozens of keys at once without writing a single command.
- **Key Favorites & Search History**: Bookmark frequently used keys for instant access and revisit recent searches from a persistent history panel.
- **Key Tags & Notes** *(client-side only)*: Annotate any key with a colour tag (red / orange / yellow / green / blue / purple) and a free-form private note — everything lives in the local redb file, **zero Redis storage cost**, never leaves the machine. Tagged rows in the key tree carry a 4 px colour bar on the left edge, and hovering reveals the note as a tooltip. Edit via right-click → "Edit tag & note…" on any leaf row. Each tag colour can be used as a one-click filter from the tree's ⋯ menu (only appears once you've tagged something); the filter sources keys **directly from local metadata** rather than the in-flight SCAN snapshot, so every tagged key shows up immediately — no waiting for SCAN to reach the relevant cursor window, no silent misses if the key sits beyond your scan_count budget. After a save, only the affected row re-renders (no full tree rebuild) unless the filter is active and the new colour changes the row's visibility. Schema is versioned on disk so future multi-tag / per-tag-note shapes can migrate in place.
- **Auto-Refresh**: Configure an automatic refresh interval for the key tree to keep your view in sync with a live, rapidly-changing Redis instance.

---

## 📦 Installation

Ready to feel the speed? Install Zedis via your favorite package manager:

### macOS
The recommended way to install Zedis is via Homebrew:

```bash
brew install --cask zedis
```

### Windows

Bash

```
scoop bucket add extras
scoop install zedis
```

### Linux (Arch)

Bash

```
yay -S zedis-bin
```

### Cargo (Cross-Platform via Source)

Bash

```
cargo install --locked zedis-gui
```

---

## 🤝 Contributing

We want to make Zedis the ultimate Redis client, and we'd love your help! Whether it's adding new features, translating the UI, or fixing bugs, all contributions are welcome.

Please read our [Contributing Guidelines](https://www.google.com/search?q=CONTRIBUTING.md) to get started. By submitting a PR, you agree to our lightweight [Contributor License Agreement (CLA)](https://www.google.com/search?q=CLA.md).

## 📄 License

Zedis is open-source software licensed under the [Apache License, Version 2.0](./LICENSE).
