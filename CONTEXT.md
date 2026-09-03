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
- **Tunnel target** (`SshTarget`) — where a tunnel session goes after
  `~/.ssh/config` filled the blanks (ADR 3); a **jump host** is one hop in
  front of it.

## Capability and versions

- **Access mode** — ReadWrite, SafeMode (the entry's read-only switch) or
  StrictReadOnly (the ACL user may not write, detected by the read-only probe,
  ADR 2). Drives `Capability`.
- **Capability** — one user-facing action (`DeleteKey`, `ConfigWrite`, …) the
  UI asks `ZedisServerState::can()` about; combines access mode with command
  availability.
- **Feature probe** (`ServerFeatures`) — per-server matrix of which commands
  the server / user allows, learned once after connect by read-only probes and
  refined by runtime `NOPERM` / `unknown command` replies. Proxies and managed
  clouds are never special-cased by brand.
- **Floor** — the first Redis version *and* the first Valkey version with a
  feature (ADR 1). The only way a version is ever compared.

## Keys and values

- **Key tree** — the namespace tree built from `SCAN` pages, split on the
  entry's separator, with TTL chips and local tags / notes.
- **Value** (`RedisValue`) — the loaded key: its type, a paged container
  (hash / list / set / zset / stream) or decoded bytes, and its TTL.
- **Decode pipeline** — the fixed order a string value is interpreted in:
  registered Protobuf schema, custom script viewer, native format detection
  (MessagePack, GZIP, ZSTD, Snappy, timestamp, image), LZ4, then text / JSON.
- **Recycle bin** — the local `DUMP` payload kept for 24h after a single-key
  delete, restorable from Tools.

## App

- **Workspace tab** — one connection with its own key tree, editor and
  terminal; up to eight side by side.
- **Route** — which panel a tab shows; tool panels are created on first visit
  and dropped on route change, so only `ZedisAppState` persists.
- **Server tool** — a panel about the server rather than a key: metrics,
  memory analysis, slow log, clients, config, ACL, topology, …
