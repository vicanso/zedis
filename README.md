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

<details><summary><b>Native GPU Rendering</b> — every pixel on the GPU, virtual-scrolled SCAN; millions of keys at 60+ FPS with minimal RAM.</summary>

Virtual scrolling combined with `SCAN` iteration keeps the interface responsive no matter how large the keyspace — zero-lag scrolling, instant tab switches, minimal memory footprint.
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

**JSON & RedisJSON** with pretty-printing, syntax highlighting, and minimal `JSON.MERGE` diffs (RFC 7396) — **JSONPath** (`$.user.email`, `$.items[?(@.price > 100)]`) works on plain string keys too, no module needed. **Protobuf & MessagePack** deserialize with zero config; 10/13-digit **Unix timestamps** preview as local + UTC; **images** (`PNG/JPG/WEBP/SVG/GIF`) and a fully editable **hex** view round it out.
</details>

<details><summary><b>Custom Script Viewer</b> — pipe any value through an external shell command for custom decoding.</summary>

Configure a command template with placeholders (`{KEY}`, `{VALUE}`, `{HEX}`, `{RAW_FILE}`); Zedis runs it via `sh -c` / `cmd /c` and shows stdout as the formatted value. Per-server key matching by exact / prefix / suffix / regex.
</details>

<details><summary><b>Specialized Type Viewers</b> — opaque values open in purpose-built, interactive viewers.</summary>

**Bitmap/Bitfield** paints bits on a GPU grid (`SETBIT`/`BITCOUNT`/`BITFIELD`); **HyperLogLog** shows `PFCOUNT` cardinality; **Vector Set + KNN** (Redis 8) walks the HNSW graph via `VSIM`; **Geo Map** plots a sorted set on a tile-less radar with `GEOSEARCH`; **Probabilistic** (RedisBloom: Bloom/Cuckoo/Count-Min/Top-K/t-digest) and **Time Series** (RedisTimeSeries `TS.INFO` + bucketed `TS.RANGE` chart) each get a dedicated card. Dispatched by the key's type / module.
</details>

<details><summary><b>Module Browsers</b> — dedicated panels for RediSearch (FT.*) and Functions (Lua libraries).</summary>

**RediSearch**: list/inspect indexes, run `FT.SEARCH` / `FT.AGGREGATE` with chips, create / alter / drop from a form. **Functions** (Redis 7+): manage libraries via `FUNCTION LIST/LOAD/DELETE` with a tree-sitter Lua editor. Both auto-hide when the module / version isn't present.
</details>

<details><summary><b>Redis Streams</b> — browse, live-tail, and manage consumer groups without leaving the GUI.</summary>

Browse entries, **live-tail** new messages (`XREAD BLOCK`, ring-buffered), inspect Consumer Groups & Pending Entries via `XINFO`, and manage groups (`XGROUP CREATE` / `SETID` / `DESTROY`, with a confirm guard on destroy).
</details>

<details><summary><b>Cross-Server Tools</b> — copy or diff a key, or diff full configs, between two servers.</summary>

**Copy** a key with value + TTL (`DUMP`/`RESTORE`), **diff** a string key against the same key elsewhere (side-by-side), or **diff** two servers' `CONFIG GET *` (striped table of only what differs). Built for "why does prod differ from staging?".
</details>

<details><summary><b>Key Editing & History</b> — rename, per-field TTL, file import/export, bulk paste, and version history.</summary>

Atomic **rename** (`RENAMENX`, overwrite-guarded), per-field **Hash TTL** (`HEXPIRE`/`HPERSIST`, Redis 7.4+), **value file export/import** (binary-safe, `KEEPTTL`), **bulk paste** of TSV/CSV into Hash/List/Set/ZSet, and a client-side **last-10-versions** write history with diff & one-click restore.
</details>

### 📊 Real-Time Observability

A built-in, GPU-accelerated dashboard for monitoring your instances.

<details><summary><b>Live Metrics</b> — real-time charts for CPU, memory, and network I/O.</summary>

Beautifully rendered, GPU-accelerated time-series charts.
</details>

<details><summary><b>Memory Analyzer + Recommendations</b> — hunt BigKeys, see the TTL distribution, get instant offline health checks plus optional AI tips.</summary>

Sort the Top-N table by **Size / Hottest / Coldest** (`OBJECT FREQ`/`IDLETIME` auto-picked from `maxmemory-policy`), with a **TTL histogram** alongside. The moment a scan finishes, an **offline rule engine** flags issues automatically — big keys, keys that can't be evicted under a `volatile-*` policy, `noeviction`, high fragmentation, many tiny strings that should be a Hash, and memory-dominating prefixes — no config or network needed. One click also sends the report (key *names*, sizes, TTLs only — never values) to any **OpenAI-compatible** endpoint for inline advice in your UI language.
</details>

<details><summary><b>Performance Diagnostics</b> — Slow Log ↔ Latency, live MONITOR, clients, and command stats.</summary>

The Performance panel cross-links **Slow Log** entries with `LATENCY` events (±5 s chips jump to the `LATENCY HISTORY` sparkline) and exports the filtered view to **CSV/JSON**; plus live `MONITOR` with keyword filtering, client management (`CLIENT LIST/KILL`), and a per-command **calls/second** table from `INFO commandstats`.
</details>

<details><summary><b>Value Search</b> — find which key <i>contains</i> some text (a guarded, sampled scan).</summary>

Redis can't index values, so this `O(keyspace)` search runs behind guardrails: a mandatory key prefix, a 10k-key / 10s cap (cancellable), and skipped over-1 MiB values — searching string values, hash fields, and list/set/sorted-set members, with each hit showing where it matched. Results are an explicit **sample**, never claimed exhaustive.
</details>

<details><summary><b>Cluster Health & Management</b> — topology tree with replication lag, plus failover / forget / meet / replicate.</summary>

Inspect Cluster/Sentinel topology as a tree (masters, slot ranges, replicas, per-replica lag from `INFO replication`), then act: `CLUSTER FAILOVER` / `FORGET` / `MEET` / `REPLICATE` and `SENTINEL FAILOVER` / `RESET` / `REMOVE`, each through the confirm dialog with PROD escalation. Only appears on multi-node deployments.
</details>

<details><summary><b>Persistence & Keyspace Events</b> — RDB/AOF status with one-click saves, plus live key-event triage.</summary>

A persistence panel reads `INFO persistence` (last save, AOF growth, fork failures) with one-click `BGSAVE` / `BGREWRITEAOF` (PROD-escalated). Keyspace notifications parse keyspace/keyevent channels into a filterable `(time, db, key, event, source)` table — "which client just deleted user:42?" — with a one-click `notify-keyspace-events` enable.
</details>

### 🛡️ Security & Productivity

<details><summary><b>Privacy-First</b> — your data and credentials stay on your machine; nothing is phoned home.</summary>

Tags, notes, favorites and search history live in a **local redb file** — zero Redis cost, never sent anywhere. Connection secrets are **encrypted at rest**, and sharing a connection as JSON **strips credentials** by default. The optional AI analysis sends only key **names, sizes and TTLs — never your values**, and only to the OpenAI-compatible endpoint *you* configure. The custom script viewer runs locally through your own shell. **No telemetry, no accounts, no cloud.**
</details>

<details><summary><b>Command Palette & Shortcuts</b> — ⌘K fuzzy navigation and a ⌘/ keyboard-shortcut reference.</summary>

**⌘K** fuzzy-searches servers and panels (arrows to move, Enter to run, Esc to dismiss); **⌘/** opens a read-only, grouped overlay of every hotkey with per-platform symbols.
</details>

<details><summary><b>Connection Safety</b> — environment tags + confirm dialogs that escalate on production.</summary>

Tag each server with a preset environment — **Dev / UAT / Prod** — shown as a colored chip in the sidebar and status bar, and lock any connection **read-only**. Destructive actions (`FLUSHALL`, `CONFIG SET`, `SHUTDOWN`, `KEYS *`, batch `DEL`, key/server delete, `XGROUP DESTROY`, cluster ops...) are intercepted with a confirm dialog that escalates its wording on a **Prod** server.
</details>

<details><summary><b>ACL Management</b> (Redis 6+) — GUI for the full ACL lifecycle.</summary>

List users, view flags / commands / key patterns / channel rules, and edit via quick presets (Full / Read-only / Disabled) plus toggleable chips for command categories and wildcards.
</details>

<details><summary><b>Secure Connections & Groups</b> — TLS/SSL and SSH tunnels, with named, shareable server groups.</summary>

Full **TLS/SSL** (custom CA, client certs) and **SSH tunneling** (password, private key, agent). Organize connections into named, collapsible **groups**, reorder them, and share a single connection as JSON (credentials stripped by default). Migrate in by pasting a `redis://` URI or a **Redis Insight** database export — every database lands at once.
</details>

<details><summary><b>Integrated CLI & Workbench</b> — redis-cli terminal with completion plus a multi-line Batch mode.</summary>

Version-aware command completion with inline argument/summary hints. A one-click **Batch** mode swaps the REPL for a multi-line editor — one command per line, run with `⌘`/`Ctrl`+`Enter` (dangerous lines still route through the confirm dialog).
</details>

<details><summary><b>Key Organization</b> — namespace tree with TTL chips, favorites, and client-side tags & notes.</summary>

Keys group into a nested tree by `:` with compact TTL chips (green live / red expiring / gray permanent). Bookmark keys, revisit search history, and add colour **tags & notes** — stored in a local redb file, **zero Redis cost**, never leaving the machine.
</details>

<details><summary><b>Bulk Key Operations</b> — multi-select delete, batch TTL, DUMP/RESTORE import/export, auto-refresh.</summary>

Multi-select to delete dozens of keys at once; set / remove TTL across a whole selection or prefix (cluster-safe, PROD-escalated); export any selection to a framed binary file (magic header + CRC32) and restore on another instance; and auto-refresh the tree for fast-changing instances.
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
