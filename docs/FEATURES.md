[中文](./FEATURES_zh.md) | English · [← Back to README](../README.md)

# Zedis — Full Feature Tour

Zedis auto-detects (`ViewerMode::Auto`) and formats payloads on the fly. This page is the exhaustive reference; the [README](../README.md) has the at-a-glance overview.

---

## 🚀 Native & Fast

### Native GPU Rendering
**Every pixel on the GPU, virtual-scrolled `SCAN` — millions of keys at 60+ FPS with minimal RAM.**

Virtual scrolling combined with `SCAN` iteration keeps the interface responsive no matter how large the keyspace — zero-lag scrolling, instant tab switches, minimal memory footprint.

### Cross-Platform
**Native on macOS, Windows, and Linux, with Light / Dark / System themes.**

A truly native feel across all three desktop platforms.

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

Configure a command template with placeholders (`{KEY}`, `{VALUE}`, `{HEX}`, `{RAW_FILE}`); Zedis runs it via `sh -c` / `cmd /c` and shows stdout as the formatted value. Per-server key matching by exact / prefix / suffix / regex.

---

## 🗂️ Type & Module Viewers

### Specialized Type Viewers
**Opaque values open in purpose-built, interactive viewers.**

**Bitmap/Bitfield** paints bits on a GPU grid (`SETBIT`/`BITCOUNT`/`BITFIELD`); **HyperLogLog** shows `PFCOUNT` cardinality; **Vector Set + KNN** (Redis 8) walks the HNSW graph via `VSIM`; **Geo Map** plots a sorted set on a tile-less radar with `GEOSEARCH`; **Probabilistic** (RedisBloom: Bloom/Cuckoo/Count-Min/Top-K/t-digest) and **Time Series** (RedisTimeSeries `TS.INFO` + bucketed `TS.RANGE` chart) each get a dedicated card. Dispatched by the key's type / module.

### Module Browsers
**Dedicated panels for RediSearch (FT.*) and Functions (Lua libraries).**

**RediSearch**: list/inspect indexes, run `FT.SEARCH` / `FT.AGGREGATE` with chips, create / alter / drop from a form. **Functions** (Redis 7+): manage libraries via `FUNCTION LIST/LOAD/DELETE` with a tree-sitter Lua editor. Both auto-hide when the module / version isn't present.

### Redis Streams
**Browse, live-tail, and manage consumer groups without leaving the GUI.**

Browse entries, **live-tail** new messages (`XREAD BLOCK`, ring-buffered), inspect Consumer Groups & Pending Entries via `XINFO`, and manage groups (`XGROUP CREATE` / `SETID` / `DESTROY`, with a confirm guard on destroy).

### Pub/Sub
**Subscribe to channels and publish messages, with a live message log.**

Pattern-based subscriptions (`PSUBSCRIBE`), a `PUBLISH` composer, and incoming messages streamed into a ring-buffered `(time, channel, message)` table — Redis's other messaging primitive alongside Streams.

---

## 📊 Real-Time Observability

A built-in, GPU-accelerated dashboard for monitoring your instances.

### Live Metrics
**Real-time charts for CPU, memory, and network I/O — with 7 days of history.**

Beautifully rendered, GPU-accelerated time-series charts. Samples are also persisted locally (one per minute, kept for 7 days), so the 1h / 24h / 7d ranges answer "did memory grow overnight?" even across app restarts.

### Memory Analyzer + Recommendations
**Hunt BigKeys, see the TTL distribution, get instant offline health checks plus optional AI tips.**

Sort the Top-N table by **Size / Hottest / Coldest** (`OBJECT FREQ`/`IDLETIME` auto-picked from `maxmemory-policy`), with a **TTL histogram** alongside. The moment a scan finishes, an **offline rule engine** flags issues automatically — big keys, keys that can't be evicted under a `volatile-*` policy, `noeviction`, high fragmentation, many tiny strings that should be a Hash, and memory-dominating prefixes — no config or network needed. One click also sends the report (key *names*, sizes, TTLs only — never values) to any **OpenAI-compatible** endpoint for inline advice in your UI language.

### Performance Diagnostics
**Slow Log ↔ Latency, live MONITOR, clients, and command stats.**

The Performance panel cross-links **Slow Log** entries with `LATENCY` events (±5 s chips jump to the `LATENCY HISTORY` sparkline) and exports the filtered view to **CSV/JSON**; plus live `MONITOR` with keyword filtering, client management (`CLIENT LIST/KILL`), and a per-command **calls/second** table from `INFO commandstats`.

### Value Search
**Find which key *contains* some text (a guarded, sampled scan).**

Redis can't index values, so this `O(keyspace)` search runs behind guardrails: a mandatory key prefix, a 10k-key / 10s cap (cancellable), and skipped over-1 MiB values — searching string values, hash fields, and list/set/sorted-set members, with each hit showing where it matched. Results are an explicit **sample**, never claimed exhaustive.

### Cluster Health & Management
**Topology tree with replication lag, plus failover / forget / meet / replicate.**

Inspect Cluster/Sentinel topology as a tree (masters, slot ranges, replicas, per-replica lag from `INFO replication`), then act: `CLUSTER FAILOVER` / `FORGET` / `MEET` / `REPLICATE` and `SENTINEL FAILOVER` / `RESET` / `REMOVE`, each through the confirm dialog with PROD escalation. Only appears on multi-node deployments.

### Persistence & Keyspace Events
**RDB/AOF status with one-click saves, plus live key-event triage.**

A persistence panel reads `INFO persistence` (last save, AOF growth, fork failures) with one-click `BGSAVE` / `BGREWRITEAOF` (PROD-escalated). Keyspace notifications parse keyspace/keyevent channels into a filterable `(time, db, key, event, source)` table — "which client just deleted user:42?" — with a one-click `notify-keyspace-events` enable.

---

## 🔑 Keys & Data Management

### Key Organization
**Namespace tree with TTL chips, favorites, and client-side tags & notes.**

Keys group into a nested tree by `:` with compact TTL chips (green live / red expiring / gray permanent). Bookmark keys, revisit search history, and add colour **tags & notes** — stored in a local redb file, **zero Redis cost**, never leaving the machine.

### Key Editing & History
**Rename, per-field TTL, file import/export, bulk paste, and version history.**

Atomic **rename** (`RENAMENX`, overwrite-guarded), per-field **Hash TTL** (`HEXPIRE`/`HPERSIST`, Redis 7.4+), **value file export/import** (binary-safe, `KEEPTTL`), **bulk paste** of TSV/CSV into Hash/List/Set/ZSet, and a client-side **last-10-versions** write history with diff & one-click restore.

### Bulk Key Operations
**Multi-select delete, batch TTL, DUMP/RESTORE import/export, auto-refresh.**

Multi-select to delete dozens of keys at once; set / remove TTL across a whole selection or prefix (cluster-safe, PROD-escalated); export any selection to a framed binary file (magic header + CRC32) and restore on another instance; and auto-refresh the tree for fast-changing instances.

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

Tag each server with a preset environment — **Dev / UAT / Prod** — shown as a colored chip in the sidebar and status bar, and lock any connection **read-only**. Destructive actions (`FLUSHALL`, `CONFIG SET`, `SHUTDOWN`, `KEYS *`, batch `DEL`, key/server delete, `XGROUP DESTROY`, cluster ops...) are intercepted with a confirm dialog that escalates its wording on a **Prod** server.

### ACL Management (Redis 6+)
**GUI for the full ACL lifecycle.**

List users, view flags / commands / key patterns / channel rules, and edit via quick presets (Full / Read-only / Disabled) plus toggleable chips for command categories and wildcards.

### Secure Connections & Groups
**TLS/SSL and SSH tunnels, with named, shareable server groups.**

Full **TLS/SSL** (custom CA, client certs) and **SSH tunneling** (password, private key, agent). When a connection fails, the server form's **Diagnose** button runs staged diagnostics — DNS → TCP → SSH auth → SSH tunnel → TLS → AUTH → PING — pinpointing the failing layer with a targeted fix hint instead of one opaque error. Organize connections into named, collapsible **groups** and reorder them. Export any selection of connections as JSON (credentials stripped by default) — or set a passphrase to emit a compact, portable **share token** (`ZEDIS1.…`, Argon2id + AES-256-GCM) that only opens with that passphrase; the import dialog detects the token and prompts for it. Migrate in by pasting a `redis://` URI or a **Redis Insight** database export — every database lands at once.

---

## ⌨️ Productivity

### Command Palette & Shortcuts
**⌘K fuzzy navigation and a ⌘/ keyboard-shortcut reference.**

**⌘K** fuzzy-searches servers, panels, and the active connection's loaded keys (arrows to move, Enter to run, Esc to dismiss); **⌘/** opens a read-only, grouped overlay of every hotkey with per-platform symbols.

### Integrated CLI & Workbench
**redis-cli terminal with completion plus a multi-line Batch mode.**

Version-aware command completion with inline argument/summary hints. A one-click **Batch** mode swaps the REPL for a multi-line editor — one command per line, run with `⌘`/`Ctrl`+`Enter` (dangerous lines still route through the confirm dialog).

### Lua Script Library
**Save, reuse, and EVALSHA-run Lua scripts with hit-rate stats.**

A local library of named Lua scripts (source + precomputed SHA1) with one-click **EVALSHA-first** execution, saved `KEYS` / `ARGS` defaults for one-click re-runs, and lifetime hit/miss counters to spot scripts being flushed from Redis's cache. (Distinct from **Functions** — that manages server-side `FUNCTION` libraries.)
