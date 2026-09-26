[中文](./README_zh.md) | English

<h1 align="center">Zedis</h1>

<p align="center">
  <strong>The Redis GUI that opens your million-key database without the spinner — native, GPU-accelerated with Rust 🦀 and GPUI ⚡️</strong>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License"></a>
  <a href="https://x.com/tree_xie"><img src="https://img.shields.io/twitter/follow/tree_xie?style=social" alt="Twitter Follow"></a>
  <img src="https://img.shields.io/github/downloads/vicanso/zedis/total" alt="Downloads">
  <a href="https://www.blazingly.fast"><img src="https://www.blazingly.fast/api/badge.svg?repo=vicanso%2Fzedis" alt="blazingly fast"></a>
</p>

<p align="center">
  <video src="https://github.com/user-attachments/assets/d7007ecf-bbfd-4e68-bbaf-437091f711e7" autoplay loop muted playsinline width="100%"></video>
</p>

---

## 🤔 Why Zedis?

Tired of Electron-based Redis clients that eat gigabytes of RAM just to display a JSON string, freeze the instant you open a key with 100,000 elements, turn cluster mode into a chore, or render your compressed and binary values as garbled bytes? We were too.

**Zedis** is built from the ground up for developers who demand native performance. Powered by **GPUI** (the same rendering engine behind the [Zed Editor](https://zed.dev)), Zedis delivers a native, buttery-smooth 60+ FPS experience with a minimal memory footprint — even when navigating massive databases.

## ✨ Highlights

- 🦀 **Native, not Electron** — every pixel on the GPU, virtual-scrolled `SCAN`; millions of keys at 60+ FPS with tiny RAM.
- 🧠 **Understands your data** — auto-decompresses and decodes JSON/JSONPath, Protobuf, MessagePack, Java / PHP / pickle serialization, BSON, JWT, Base64, URL encoding, timestamps, images and hex, with purpose-built viewers for every Redis type and module.
- 📊 **Real-time observability** — live metrics, a memory analyzer (offline + AI recommendations, server-side key-size histogram), hot-key tracking (`HOTKEYS`), per-slot cluster stats, Slow Log ↔ Latency, `MONITOR`, and value search.
- 🔐 **Privacy-first & safe** — metadata stays in a local file, secrets are encrypted with a per-machine key, and destructive actions escalate their confirms on production.
- 🌐 **Connect anything** — TLS/SSL, SSH tunnels (incl. passphrase-protected keys), Cluster/Sentinel, import from Redis Insight / ARDM / Tiny RDM, and 8 UI languages.
- 🔀 **Redis and Valkey, both first-class** — every version gate carries a Valkey floor of its own, every Valkey-only feature has its panel or action (`COMMANDLOG`, atomic slot migration, `CLUSTER SLOT-STATS`, multi-database clusters, `SCRIPT SHOW`, availability zones), valkey-json / valkey-search / valkey-bloom are recognised, a copy between a Redis and a Valkey lands even though their `DUMP` payloads do not, and CI runs Valkey 8.0, 9.0 and the 9.1 bundle beside Redis 6.2–8 ([the matrix](#-redis-and-valkey)).
- ⌨️ **Built for power users** — ⌘K command palette, redis-cli with completion, table/JSON replies + AI command assistant, batch mode, and cross-server copy/diff.
- 🕸️ **In the browser too** — the same app compiled to WebAssembly, self-hosted from one ~26 MB Docker image ([Web version](#-web-version-self-hosted)).
- 🤖 **An AI assistant on a leash** — the web bridge is an MCP server too: a read-only account, the same command allowlist, every call audited ([MCP](#an-ai-assistant-at-the-same-door-mcp)).

> ### 🔄 Already using Redis Insight?
> **Paste its database export and every connection lands at once** — no re-entering hosts, ports, and passwords one by one. Point Zedis at your real setup in about a minute, then judge the speed for yourself.

## 📸 Screenshots

<!--
  Approach A — host images on GitHub: drag-and-drop each screenshot into any
  issue/PR comment (or a release) to get a
  https://github.com/user-attachments/assets/... URL, then replace each
  REPLACE-* placeholder below. Clicking a thumbnail opens the full-resolution
  image; width="260" keeps the 3-wide grid to ~one screen.
  Suggested shots (most important first):
    1. key-browser     — namespace tree + JSON/syntax-highlighted value editor
    2. memory-analyzer — Top-N table + TTL histogram + recommendations
    3. live-metrics    — real-time GPU charts (CPU / memory / network)
    4. geo-map         — a sorted set plotted on the radar
    5. vector-set      — Vector Set + KNN (VSIM) results
    6. command-palette — the ⌘K palette
-->

<table>
  <tr>
    <td><a href="https://github.com/user-attachments/assets/c06e4d80-7607-4d6c-807e-2a62a2ee556f"><img src="https://github.com/user-attachments/assets/c06e4d80-7607-4d6c-807e-2a62a2ee556f" width="260" alt="Key browser & data viewer"></a></td>
    <td><a href="https://github.com/user-attachments/assets/88091f50-ec77-41d5-acda-047a835079f8"><img src="https://github.com/user-attachments/assets/88091f50-ec77-41d5-acda-047a835079f8" width="260" alt="Memory analyzer"></a></td>
    <td><a href="https://github.com/user-attachments/assets/d5801a8c-da94-461b-83b6-6c9b70e2007d"><img src="https://github.com/user-attachments/assets/d5801a8c-da94-461b-83b6-6c9b70e2007d" width="260" alt="Live metrics"></a></td>
  </tr>
  <tr>
    <td align="center"><sub>Key browser & data viewer</sub></td>
    <td align="center"><sub>Memory analyzer</sub></td>
    <td align="center"><sub>Live metrics</sub></td>
  </tr>
  <tr>
    <td><a href="https://github.com/user-attachments/assets/2525cec9-5dd6-4049-9ea9-60fcb4cc249f"><img src="https://github.com/user-attachments/assets/2525cec9-5dd6-4049-9ea9-60fcb4cc249f" width="260" alt="Geo map"></a></td>
    <td><a href="https://github.com/user-attachments/assets/b4733051-2965-40ff-9d49-6bb909551513"><img src="https://github.com/user-attachments/assets/b4733051-2965-40ff-9d49-6bb909551513" width="260" alt="Vector Set + KNN"></a></td>
    <td><a href="https://github.com/user-attachments/assets/4335c12b-cbca-467e-abd9-7b50ffd568c5"><img src="https://github.com/user-attachments/assets/4335c12b-cbca-467e-abd9-7b50ffd568c5" width="260" alt="Command palette"></a></td>
  </tr>
  <tr>
    <td align="center"><sub>Geo map</sub></td>
    <td align="center"><sub>Vector Set + KNN</sub></td>
    <td align="center"><sub>Command palette (⌘K)</sub></td>
  </tr>
</table>

## 🧩 Features at a Glance

| Area | What's inside |
| --- | --- |
| 🚀 **Native & Fast** | GPU rendering · virtual-scrolled `SCAN`, 60+ FPS on millions of keys · macOS / Windows / Linux · Light / Dark / System + 6 bundled themes · configurable UI & monospace fonts |
| 🧠 **Smart Data Viewer** | Auto-decompress (LZ4 / Snappy / GZIP / ZSTD) · JSON & RedisJSON + JSONPath · Protobuf · MessagePack · Java / PHP / pickle serialization · BSON · JWT · Base64 · URL encoding · timestamps · images · hex · custom script viewer — inside Hash / List / Set / ZSet elements too, where the stored bytes stay what is edited |
| 🗂️ **Type & Module Viewers** | Bitmap (`BITOP`) · HyperLogLog (`PFMERGE`) · Vector Set (KNN) · Geo map (`GEOADD` / `GEODIST`, radius + box search) · Bloom / Cuckoo / Count-Min / Top-K · Time Series (`TS.ADD` / `TS.ALTER` / compaction rules, plus a multi-series `TS.MRANGE` explorer) · Streams (live-tail, `XSETID`, consumer admin) · Pub/Sub (incl. sharded) · RediSearch (index size, `FT.TAGVALS` values, `FT.SPELLCHECK` suggestions) · Functions |
| 📊 **Observability** | Live metrics + 7-day history with CSV export · `MEMORY DOCTOR` / `MEMORY STATS` and `LATENCY DOCTOR` reports · memory analyzer (live scan or offline RDB file) with type/encoding shares and drill-down prefixes + AI tips · Slow Log ↔ Latency (+ Valkey `COMMANDLOG` size logs) · `MONITOR` · value search · cluster health, slot reshard / repair / rebalance · primary/replica replication (`REPLICAOF` / `FAILOVER`) · persistence & keyspace events · typed CONFIG editor (+ `CONFIG REWRITE`) · raw INFO browser |
| 🔑 **Keys & Data** | Namespace tree with TTL chips · paginated Hash / List / Set / ZSet editors (`HSCAN`/`SSCAN`/`ZSCAN`) · multi-select batch delete · type-native ops (`LTRIM` / `LPOP` / `ZINCRBY` / `ZPOPMIN` / `HINCRBY` / `INCRBY` / `APPEND` / `GETEX`) · ZSet score-range filter (`ZRANGEBYSCORE`) · tags / notes / favorites · rename · field-level TTL · absolute expiry (`EXPIREAT`) · storage encoding / idle time in the key bar · version history · session change log with structured diff for collections · find & replace in the value editor · JSON tree view with path-level ops (`JSON.SET` / `JSON.DEL` / `JSON.NUMINCRBY` / `JSON.TOGGLE` / `JSON.ARRAPPEND` / `JSON.STRAPPEND` / `JSON.CLEAR` — applied locally for a plain string holding JSON) · JSON check, format & minify before save · local recycle bin (24h) · file import/export · bulk ops (Tools export, prefix filter, binary / JSON / CSV) · cross-server copy & diff — one key, or a whole prefix: bulk copy straight through `DUMP`/`RESTORE` and a two-database compare |
| 🔐 **Security & Privacy** | Env tags with PROD-escalated confirms · read-only lock · Prod starts write-locked, unlocked 15 min at a time · ACL editor with security log, dry-run tester, `ACL GENPASS` and aclfile save/load · TLS/SSL & SSH · staged connection diagnostics · self-healing link with Sentinel/Cluster failover · per-machine encrypted secrets · local-only, no telemetry |
| 🧭 **Limited servers** | Capability probe after connect: proxies (Twemproxy / Codis / Envoy), managed clouds (ElastiCache / Azure / Tair) and Redis-compatible servers (Valkey / Dragonfly / KeyDB / Kvrocks) get panels and buttons greyed out *with the reason* (`CONFIG GET` missing, `SLOWLOG` denied) instead of failing · key editor keeps working without `SCAN` · the full command matrix lives under Tools → Server capabilities |
| ⌨️ **Productivity** | Multi-connection workspace tabs · ⌘K palette · ⌘P recent keys · ⌘⇧F multi-database key search · ⌘/ shortcut reference · custom keybindings (`keybindings.toml`) · ⌘+/− zoom · one instance per profile + `redis://` links · redis-cli with per-server history, completion & `Ctrl+R` search · AI command assistant (`?` in terminal) · multi-line batch mode · Lua script library · opt-out update check with checksum-verified download (+ pre-release / nightly channel) · time zone & date format · local-data backup (tags, favorites, scripts) · optional system tray (macOS / Windows) · HTTP / SOCKS5 proxy for the app's own requests · rotating file logs · Export Diagnostics (one zip: logs, crash reports, redacted config, connection state) |

> 🔐 **Where connection secrets live:** passwords and SSH keys are encrypted with a random **per-machine** key — kept in the **macOS Keychain** or **Windows Credential Manager**, and in a `0600`-permission key file under the config dir on **Linux** (no Secret Service / D-Bus dependency, so it works headless too). The key never leaves the machine, so a copied config won't decrypt elsewhere — use the passphrase-protected export to move connections between machines.

📖 **[See the full feature tour →](./docs/FEATURES.md)**

---

## 🔀 Redis and Valkey

Zedis treats the two as what they are: one wire protocol, two release lines that diverged at 7.2.4 and have shipped different things since. Valkey is not a compatibility mode here: every feature that depends on a server version is gated by a floor per flavor (`crates/zedis-connection/src/floors.rs`), so a Valkey that never shipped a command is never sent it, a Valkey that shipped it earlier gets it earlier, and everything Valkey ships that a client can put in front of a person has its panel or action — the table below is the whole list of divergences, checked against `COMMAND LIST` and `COMMAND DOCS` of Redis 7.4 / 8.0 / 8.10 and Valkey 8.0 / 8.1 / 9.0 / 9.1 on 2026-09-26. What is left over is what a GUI has no use for (`CLIENT CAPA`, `CLIENT IMPORT-SOURCE`, `DELIFEQ`, `MSETEX`, `CLUSTERSCAN`; on the Redis side `DELEX`, `HIMPORT`, `BACKUP` and the like), which is sent to neither. The live integration suite runs on Redis 6.2 / 7.2 / 8.0, Valkey 8.0 / 9.0, `redis-stack` and `valkey-bundle` (Valkey 9.1 with its modules) on every change — the Valkey lanes are pinned to the *first* release of each line, which is where a floor is wrong if it is wrong.

| Feature | Redis | Valkey |
|---|---|---|
| `COMMANDLOG` — slow, large-request and large-reply logs | — | 8.1 |
| Atomic slot migration (`CLUSTER MIGRATESLOTS`; Redis 8.4 has its own `CLUSTER MIGRATION`, not used yet — a Redis cluster keeps the classic reshard) | — | 9.0 |
| `CLUSTER SLOT-STATS` (per-slot keys, CPU, network) | 8.2 | 8.0 |
| Multiple databases in cluster mode | — | 9.0 |
| Hash field TTL (`HEXPIRE`, `HSETEX`, `HTTL`…) | 7.4 | 9.0 |
| `SET … IFEQ` | 8.4 | 8.1 |
| `CLIENT KILL … MAXAGE` | 7.4 | 8.0 |
| `SCRIPT SHOW` — the source behind an `EVALSHA` in the slow log | — | 8.0 |
| Availability zone per node (`availability-zone`, on the topology page) | — | 8.1 |
| `INFO keysizes` histograms (memory analyzer) | 8.0 | — |
| `HOTKEYS` tracking | 8.6 | — |
| Stream `XACKDEL` / `XDELEX` and reference policies | 8.2 | — |
| `XNACK` | 8.8 | — |
| Vector sets | 8.0 | — |
| `allkeys-lrm` / `volatile-lrm` eviction | 8.6 | — |
| JSON | RedisJSON | valkey-json |
| Search | RediSearch | valkey-search — `FT.CREATE` / `SEARCH` / `AGGREGATE` / `INFO`; no `TAGVALS`, `SPELLCHECK`, `EXPLAIN`, `PROFILE` or `DROPINDEX DD`, which the panel reports as unavailable |
| Probabilistic | RedisBloom — BF, CF, CMS, TOPK, TDIGEST | valkey-bloom — BF |
| Time series | RedisTimeSeries | — |

A command a server does not have is never a crash: the panel that needs it says so, and everything else keeps working. A command that *would* crash the server is not sent either: Redis 8.0–8.2.6 and Valkey 8.0 die in `lookupKey()` when a `CLIENT NO-TOUCH` client unblocks another client, so Zedis withholds that flag there and the memory analyzer's heat column is a little less exact instead.

**Copying between the two.** `DUMP` payloads carry the RDB version of the server that wrote them, and the two flavors number theirs apart — Redis 7.4 writes 12 and Redis 8.10 15, Valkey 8 writes 11 and Valkey 9 80 — so `RESTORE` refuses a copy in every direction but Valkey 8 → Redis. Zedis notices that refusal and re-creates the key by type from the source instead (string, hash, list, set, sorted set, stream with its entry ids, JSON), TTL included, so a migration or a single-key copy between a Redis and a Valkey lands either way; the log says which keys travelled that way. What cannot travel like that says so: a Bloom filter, a time series or a vector set has no portable read, a stream's consumer groups and a hash's per-field TTLs are not part of the value. A `.zdis` file is `DUMP` payloads and cannot be re-created on the other flavor — export as JSON to move keys between them by file.

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

The `.msi` and the `.exe` in the `.zip` are Authenticode-signed — see the [Code signing policy](#-code-signing-policy).

### Linux

Arch Linux (AUR):

```bash
yay -S zedis-bin
```

Other distributions: `.deb`, `.rpm`, an AppImage and a plain tarball (x86_64 and aarch64) are attached to every [release](https://github.com/vicanso/zedis/releases/latest).

### Cargo (Cross-Platform via Source)

> **Note:** Zedis depends on an unreleased (git) version of GPUI, which crates.io
> doesn't allow — so the **latest** version can't be published to crates.io, and
> the crates.io build may lag behind. For the newest version, prefer Homebrew /
> Scoop / the AUR above or a [release download](https://github.com/vicanso/zedis/releases);
> to build from source, use the `--git` command below.

```bash
# From crates.io — may be an older version (see note above)
cargo install --locked zedis-gui

# Latest: build straight from GitHub (resolves the git dependencies)
cargo install --git https://github.com/vicanso/zedis --locked zedis-gui
```

---

## 🌐 Web version (self-hosted)

Zedis also runs in the browser: the same app, compiled to WebAssembly and drawn on a canvas. A browser cannot open a TCP socket, so a small HTTP server — `zedis-bridge` — serves the page and talks to Redis on the browser's behalf. Host it once next to your Redis servers and the whole team reaches them from a browser tab, with nothing to install. Redis passwords stay on the bridge, encrypted at rest; the browser never receives them.

> **Early preview.** The image is published for linux/amd64 and linux/arm64 (~26 MB): `:latest` and the release version for tagged releases, `:nightly` for the rolling build from `main`.

### Try it

```bash
docker run -d --name zedis-web -p 7379:7379 \
  -e ZEDIS_BRIDGE_USERS="admin@change-me" \
  -v zedis-data:/data \
  vicanso/zedis-web:latest --listen 0.0.0.0:7379 --insecure-cookie
```

Open <http://localhost:7379> and sign in as `admin` / `change-me`.

- **Accounts are required** — either `ZEDIS_BRIDGE_USERS="name@password,name2@password2"` or `--users-file` (see [Accounts and sharing](#accounts-and-sharing)). The bridge does not start without one of them, and refuses to start with both. Each inline entry is split at its first `@`, so a name cannot contain `@` or `:`, and a password cannot contain a comma. Scripts can use HTTP Basic (`curl -u admin:change-me …/v1/servers`).
- **`/data`** holds the server list (secrets encrypted with the `master.key` file beside it) and the saved logins. Keep the volume, or every restart starts empty.
- **`--insecure-cookie` is for a plain-http trial only.** The login cookie is `Secure` by default, and a browser silently drops a `Secure` cookie that arrives over plain http from anything but `localhost` (and some browsers drop it even there). Without the flag the sign-in succeeds and the very next request answers `401`.
- **A Redis on the Docker host** is not `127.0.0.1` from inside the container. Use `host.docker.internal` (on Linux, add `--add-host=host.docker.internal:host-gateway`) or `--network host`.

### Deploy it

Over plain http the account password and everything read from Redis cross the network in the clear. For anything beyond a trial, drop `--insecure-cookie`, publish the port to loopback only, and put an HTTPS reverse proxy in front:

```bash
docker run -d --name zedis-web -p 127.0.0.1:7379:7379 \
  -e ZEDIS_BRIDGE_USERS="alice@…,bob@…" \
  -v zedis-data:/data \
  vicanso/zedis-web:latest
```

```caddyfile
zedis.example.com {
    reverse_proxy 127.0.0.1:7379
}
```

Account passwords are guessable: add a rate limit at the proxy if the bridge is reachable from outside your own network.

### Sharing a host name with other applications

When the host name is not Zedis's alone, give the bridge a path of its own with `ZEDIS_BRIDGE_BASE_PATH` (or `--base-path`). The page, its files and the API all move under it, nothing outside it answers, and the login cookie is scoped to it — so the other applications on that host never receive it.

```bash
docker run -d --name zedis-web -p 127.0.0.1:7379:7379 \
  -e ZEDIS_BRIDGE_USERS="alice@…,bob@…" \
  -e ZEDIS_BRIDGE_BASE_PATH=/zedis \
  -v zedis-data:/data \
  vicanso/zedis-web:latest
```

```caddyfile
tools.example.com {
    # `handle`, not `handle_path`: the prefix is forwarded, not stripped.
    handle /zedis* {
        reverse_proxy 127.0.0.1:7379
    }
    # … the other applications
}
```

Open `https://tools.example.com/zedis/`. For nginx the equivalent is `location /zedis { proxy_pass http://127.0.0.1:7379; }` — no trailing slash on `proxy_pass`, which would strip the prefix. The health check moves too: `/zedis/v1/health`.

### Accounts and sharing

A server entry belongs to the account that added it and nobody else sees it, unless its **Shared** box is ticked — then it is everyone's. Any account that can see a shared entry may edit or delete it.

Accounts come from one of two places, never both:

```bash
# inline: ":ro" after the name makes the account read-only
-e ZEDIS_BRIDGE_USERS="alice@secret,bob:ro@hunter2"
```

```toml
# or a file: --users-file /data/users.toml (ZEDIS_BRIDGE_USERS_FILE)
[[users]]
name = "alice"
password = "secret"

[[users]]
name = "bob"
password = "hunter2"
read_only = true

[[users]]
name = "carol"
password = "s3cret"
servers = ["prod-*:ro", "staging", "id:0199…"]   # which shared entries, and where read-only
```

A file keeps the passwords out of the environment every `docker inspect` prints, and is the only form you can edit without restating the whole list. The role rides on the *name* in the inline form because a password may contain `:` and a name may not.

`servers` narrows which **shared** entries an account sees — by name, with `*` and `?` as wildcards, or by `id:` — and `:ro` on a rule makes the account read-only there while it keeps its full role elsewhere. Where rules disagree about one entry, write wins, so the broad rule is the restriction and the exceptions are named: `["prod-*:ro", "prod-eu"]` reads every prod and writes `prod-eu`. Left out, the account sees every shared entry; an empty list, none. Its own entries an account always sees and writes. Only the file has this; the inline form has no room for it. Names are whatever their owners typed, so an account that can edit an entry can rename it into or out of a pattern — use `id:` where that matters.

A **read-only** account may look at everything it can see and change none of it: no writes to Redis, and no adding, editing or deleting a server entry. The refusal is the bridge's, not the page's — it answers `403` to anything that is not a read, so a script posting straight to `/v1/exec` is refused exactly like the page, and a confirmation does not buy a way past it. The test is an allowlist of reads, not a list of writes to avoid: `EVAL` runs any script, `BITFIELD` writes under a name that reads, `GETDEL` and `GETEX` are writes spelled like gets, and every module command is one the core table has never heard of — so anything unrecognised is refused, and a read that was forgotten shows up as a panel saying it is unavailable.

Changing an account's password **or its role** ends the logins it already has, so a demotion reaches the browsers that are already signed in.

This is defence in depth and not a substitute for Redis's own: a Redis ACL user with `-@write` is enforced by the server on every connection, whatever talks to it, while this is enforced by the bridge, which is the only thing the browser can reach. Use both — the ACL is the guarantee, the account is what greys the buttons out before the round trip.

### Signing in through your own SSO

A company that already has single sign-on puts an authenticating reverse proxy — oauth2-proxy, Authelia, Pomerium, Cloudflare Access, Tailscale — in front of its applications: the proxy signs the person in and writes who they are into a request header. The bridge can believe that header:

```bash
-e ZEDIS_BRIDGE_TRUSTED_HEADER=Remote-User          # --trusted-header
-e ZEDIS_BRIDGE_TRUSTED_PROXY=10.0.0.0/8,172.17.0.5  # --trusted-proxy: the proxy's addresses
```

Both or neither: anyone can write `Remote-User: alice` into a request, so the header counts only on a connection *from the proxy* — by the socket's peer address, never by `X-Forwarded-For`. The name in the header must be an account in the users file, which may then leave that account's password out; a name that is no account is refused with a `403` (and an audit line), not signed in as somebody new. Roles stay in the users file (`read_only = true`), because the proxy says who someone is and not what they may do.

```toml
[[users]]
name = "alice@example.com"   # exactly what the proxy writes in the header
read_only = true
```

Two things the proxy must do: strip or overwrite that header on every request it forwards, and be the only way to reach the bridge — a bridge that is also reachable directly is one where the address check protects nothing. The header name is the only thing that differs between proxies: Authelia sends `Remote-User`, oauth2-proxy `X-Auth-Request-User` (with `--set-xauthrequest`), Cloudflare Access `Cf-Access-Authenticated-User-Email`, Tailscale `Tailscale-User-Login`. Without these two settings the header is never read.

### Locked writes on production

A **Prod**-tagged entry starts with its writes locked, on the desktop and in the browser alike (any entry can, through the *Writes* choice on its Safety tab — follow the tag, allowed, locked, read-only). The status-bar lock asks for the server's name and opens a **15-minute window** — the button shows what is left and locks again by itself. In the browser the bridge keeps the same window per account (`POST` / `DELETE /v1/servers/{id}/unlock`, audited as `unlocked` / `locked`) and refuses a write outside it with the same `428` a destructive command gets, so a script is held to what the page is. An existing Prod entry starts locked after this version; set its *Writes* to *Allowed* if that is not wanted.

### Audit log

`--audit-log /data/audit.log` (`ZEDIS_BRIDGE_AUDIT_LOG`) appends one JSON line per event: every login and failed login, every refusal of a read-only account, every server entry added, edited (which settings changed and from what; which secrets changed, never to what; a private entry made shared) or deleted, every command that administers the server — `CONFIG SET`, `ACL SETUSER`, `REPLICAOF`, `MODULE LOAD`, `CLIENT KILL`, `FLUSHDB` and the like — and every command someone had to confirm, so an entry with *confirm every write* switched on logs each of its writes. `--audit-writes` (`ZEDIS_BRIDGE_AUDIT_WRITES=1`) adds plain data writes; reads are never logged. Passwords in arguments are blanked, long values cut, a batch of one command is one line with a count, and the file is created owner-only.

```json
{"ts":"2026-09-25T08:12:03.417Z","account":"alice","peer":"10.0.0.7:51234","event":"command","server":{"id":"0199…","name":"prod"},"db":0,"command":"CONFIG","args":["SET","maxmemory","2gb"],"outcome":"confirmed","kind":"config_set","confirm":"type_name"}
```

It is the log of this door only: what reaches Redis from `redis-cli` or an application is not in it, and it cannot tell you who changed a key — it can tell you whether anyone did so by hand. The bridge appends and never reopens the file, so rotate it with `copytruncate`.

### An AI assistant at the same door (MCP)

`POST /v1/mcp` is a [Model Context Protocol](https://modelcontextprotocol.io) server, so Claude Code, Cursor or any other MCP client can read your Redis through the bridge — and only read. The assistant signs in like a script (HTTP Basic) as an account that **must be read-only** (`ai:ro@secret`, or `read_only = true`); a full account is refused there whatever it asks. Its tools are shaped for a model rather than a terminal: `list_servers`, `scan_keys` (paged, across every master of a cluster), `inspect_key` (type, TTL, memory, encoding, length and a short preview), `server_info` and `slowlog` (parsed, per master), and `read_command` for any other read-only command. Every command a tool sends goes through the same read-only allowlist as the page, plus a refusal of the commands that would change the shared connection (`SELECT`, `AUTH`, `CLIENT SETNAME`, `SUBSCRIBE`, …); writes, scripts and administration are refused with a reason the model can read. Large values are cut to a size that fits a context, an account may make 120 calls a minute, and **every call is one line of the audit log** — reads included, because the caller is a program acting for someone.

```sh
claude mcp add --transport http zedis https://bridge.example.com/v1/mcp \
  --header "Authorization: Basic $(printf 'ai:secret' | base64)"
```

It is the door the page uses, not a second one: the `servers` rules of the users file say which entries the assistant sees, the audit log says what it read, and nothing passes through a third party on the way.

### What the web version leaves out

Everything that is a request and a reply works: the key tree, every value editor, the terminal, metrics, slow log, config, clients, memory analysis, value search. Not available in the browser: the streaming panels (`MONITOR`, Pub/Sub, keyspace events), Topology and Sentinel administration, the Lua script library and the Protobuf schema editor, multi-database key search, migration (file import / export) and cross-server compare, connection diagnostics, the recycle bin and the 1h / 24h / 7d Metrics history (both need storage that survives a reload), and the shortcuts a browser keeps for itself (⌘N / ⌘T / ⌘W). The value editors show code as plain text there: `gpui-component` puts syntax highlighting behind a `tree-sitter` feature that no wasm build can enable, so JSON and the rest are uncoloured and nothing folds — formatting, JSONPath and editing are unaffected. Tags, notes, favorites and saved scripts work but live in the page only — a reload clears them. A page left in a background browser tab slows its polling down by itself. A panel that is left out says so instead of failing. The desktop app remains the complete client.

Without Docker, `make web-dist` builds the same thing as a single self-contained binary.

---

## 🔏 Code signing policy

Free code signing provided by [SignPath.io](https://signpath.io), certificate by [SignPath Foundation](https://signpath.org).

The Windows binaries attached to each release — `zedis-windows-*.msi` and the `zedis.exe` inside `zedis-windows-*.zip` — are Authenticode-signed with the SignPath Foundation certificate. What gets signed is exactly what GitHub Actions built from the tagged commit of this repository ([`publish.yml`](./.github/workflows/publish.yml)), and every release is approved by hand before it is signed. macOS builds are signed and notarized separately with the maintainer's Apple Developer ID.

**Team**

- Committers and reviewers: [@vicanso](https://github.com/vicanso)
- Approvers: [@vicanso](https://github.com/vicanso)

Changes from outside the team arrive as pull requests and are reviewed by a committer before they are merged.

**Privacy policy**

This program will not transfer any information to other networked systems unless specifically requested by the user or the person installing or operating it, with two exceptions, both under your control:

- **Update check.** On startup, at most once every two days, Zedis downloads the release manifest from GitHub Releases to learn whether a newer version exists. **If GitHub cannot be reached**, the same request is retried against the [Gitee mirror](https://gitee.com/vicanso/zedis), which the release workflow copies every tagged release to — so the check, and any download you start, still work on networks where GitHub does not. Either way the request carries nothing but the app version in its `User-Agent` — nothing about you, your machine or your data — and a download is verified against the SHA-256 in the release manifest, so a mirror serving anything else fails the same check a corrupted transfer would. Turn it off with *Settings → Check for Updates Automatically*; a new version is only downloaded when you click Update.
- **AI assistant.** Only after you enter an endpoint under *Settings → AI Base URL*, the text you explicitly hand it — a command description, a memory report — is sent to that endpoint and nowhere else.

Your Redis servers, SSH tunnels and the optional proxy connect only where you point them. Zedis has no telemetry and sends no crash reports: they stay on disk until you export a diagnostics bundle yourself.

---

## 🤝 Contributing

We want to make Zedis the ultimate Redis client, and we'd love your help! Whether it's adding new features, translating the UI, or fixing bugs, all contributions are welcome.

Open an issue or a PR to get started. By submitting a PR, you agree to our lightweight [Contributor License Agreement (CLA)](./CLA.md).

## 📄 License

Zedis is open-source software licensed under the [Apache License, Version 2.0](./LICENSE).
