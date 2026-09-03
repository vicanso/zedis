# 1. Every version gate names its Valkey floor

Date: 2026-09-03 · Status: accepted

## Context

Valkey forked from Redis 7.2.4 and numbers its releases (7.2 → 8.0 → 8.1 → 9.0)
on a track that runs ahead of Redis's. A bare `version >= x.y.z` written for a
Redis feature therefore passes on a Valkey that never shipped it (`INFO keysizes`,
`HOTKEYS`, `XACKDEL`) or shipped it at another version (`SET IFEQ`: Redis 8.4 vs
Valkey 8.1; hash-field TTL: Redis 7.4 vs Valkey 9.0). The Valkey 8 CI run failed
exactly this way.

## Decision

The only version primitive is `Floor { redis, valkey: Option<…> }` in
`crates/zedis-connection/src/floors.rs`. Every gated feature is one constant
there, built with `since_fork` (every Valkey release has it), `both(redis, valkey)`
(different floors) or `redis_only` (Valkey never shipped it), and checked through
`RedisClient::supports(floor)` / `ZedisServerState::supports(floor)`. Live tests
gate with the same constants. There is no `is_at_least_version` to reach for.

## Consequences

- Adding a gate means researching and writing down the Valkey side — release
  notes, not guesswork; `None` is a claim too.
- Command *availability* on proxies and managed clouds is a separate axis and
  stays probed (`ServerFeatures`), never inferred from a version.
