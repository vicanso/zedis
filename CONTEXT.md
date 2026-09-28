# Zedis — domain context

The words the code and the docs use, with the one meaning each has here.
Architecture and conventions live in `CLAUDE.md`; decisions in `docs/adr/`.

## Connections

- **Server entry** (`RedisServer`) — one saved connection in
  `redis-servers.toml`: host, credentials, topology pin, TLS, SSH, per-server
  key-tree preferences. Secrets are encrypted at rest with the per-machine
  master key.
- **Seed** — the address(es) in the entry's host field: what discovery dials
  first. A Sentinel entry may list several; everything after the seed
  (masters, cluster nodes) comes from the server.
- **Topology** (`ServerType`) — Standalone, Sentinel or Cluster; `Auto` lets
  discovery decide from `ROLE` / `INFO cluster`, a pinned value skips it.
- **Pooled client** (`RedisClient`) — the cached, multiplexed connection per
  `(server, db)` every panel shares, rebuilt by the heartbeat after a link
  error. It never carries connection-scoped state (ADR 4).
- **Dedicated connection** — a connection one owner opens and drops: the
  terminal, `MONITOR`, live tails, sharded Pub/Sub, the feature probe.
- **Heartbeat** — the status bar's 2s tick, one per workspace tab. On a single
  master (standalone, Sentinel) it is one `INFO` on the pooled client's own
  connection, timed as the latency; on a cluster a `PING` there plus an `INFO`
  fanned out to every master, the beat stretched with the master count. It is
  what notices a dropped link and rebuilds the pooled client. While the server
  is unreachable it backs off (2s doubling to 60s), and a background tab — or
  an app nobody is looking at — polls every 30s (ADR 5).
- **Tunnel target** (`SshTarget`) — where a tunnel session goes after
  `~/.ssh/config` filled the blanks (ADR 3); a **jump host** is one hop in
  front of it.
- **`ServerDb`** — which database of which server entry: what every Redis
  operation in `zedis-connection` takes instead of a connection (ADR 10). A
  **`ClusterNode`** names one node instead, for the cluster commands that act
  where they land (`CLUSTER REPLICATE`, `FAILOVER`, `SETSLOT`, …).
- **Write lock** (`RedisServer::write_locked`) — an entry whose writes start
  locked: its *Writes* choice, else its Prod tag. It connects in SafeMode and
  the status bar opens an **unlock window** of `WRITE_UNLOCK_SECS` at a time
  (ADR 14).

## Capability and versions

- **Access mode** — ReadWrite, SafeMode (the entry's read-only switch, or a
  write-locked entry outside its unlock window) or StrictReadOnly (the ACL
  user may not write, detected by the read-only probe, ADR 2; or the bridge
  account is read-only on that entry, ADR 13). Drives `Capability`.
- **Capability** — one user-facing action (`DeleteKey`, `ConfigWrite`, …) the
  UI asks `ZedisServerState::can()` about; combines access mode with command
  availability.
- **Feature probe** (`ServerFeatures`) — per-server matrix of which commands
  the server / user allows, learned once after connect by read-only probes and
  refined by runtime `NOPERM` / `unknown command` replies. Proxies and managed
  clouds are never special-cased by brand.
- **Floor** — the first Redis version *and* the first Valkey version with a
  feature (ADR 1). The only way a version is ever compared.
- **Flavor** — Redis or Valkey: one wire protocol, two release lines since
  7.2.4. Every floor names both, and a `DUMP` payload crosses between them
  only from Valkey 8 to Redis (ADR 16).

## Keys and values

- **Key tree** — the namespace tree built from `SCAN` pages, split on the
  entry's separator, with TTL chips and local tags / notes.
- **Value** (`RedisValue`) — the loaded key: its type, a paged container
  (hash / list / set / zset / stream) or decoded bytes, and its TTL.
- **Decode pipeline** — the fixed order a string value is interpreted in:
  registered Protobuf schema, custom script viewer, native format detection
  (MessagePack, GZIP, ZSTD, Snappy, timestamp, image, Java serialization,
  pickle, BSON), LZ4, the text encodings (JWT, PHP serialize, URL, Base64 —
  `zedis-core/src/codec`), then text / JSON. Every decoded view is read-only.
- **Recycle bin** — the local `DUMP` payload kept for 24h after a single-key
  delete, restorable from Tools.
- **Logical copy** — re-creating a key by type from its source when
  `RESTORE` refuses the other flavor's payload; module types, consumer groups
  and field TTLs are named as lost (ADR 16).

## App

- **Workspace tab** — one connection with its own key tree, editor and
  terminal; up to eight side by side.
- **Route** — which panel a tab shows; tool panels are created on first visit
  and dropped on route change, so only `ZedisAppState` persists.
- **Server tool** — a panel about the server rather than a key: metrics,
  memory analysis, slow log, clients, config, ACL, topology, …

## Web build

- **Bridge** (`zedis-bridge`) — the HTTP server the browser build talks to: it
  serves the page and forwards RESP frames (ADR 9). Nothing in the browser
  speaks to Redis directly.
- **Account** — a sign-in on the bridge (`ZEDIS_BRIDGE_USERS` or a users
  file), required. A **read-only account** — or one whose `servers` rule says
  `:ro` for an entry — is refused every write by the bridge, before any
  confirmation (ADR 13); the **MCP** door admits read-only accounts only
  (ADR 15).
- **Owner** — the account a server entry belongs to on the bridge. An entry
  without one is **shared**; someone else's private entry answers like a
  missing one (ADR 9).
- **Audit log** — the bridge's one JSON line per event: sign-ins, refusals,
  entry changes, administration and confirmed commands, and every MCP call
  (ADR 11).
