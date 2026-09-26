# 16. A copy between flavors re-creates the key

Date: 2026-09-26 · Status: accepted

## Context

Copying a key to another server — the migration page, the key menu's
*copy to* — is `DUMP` on one side and `RESTORE` on the other: one payload,
any type, the encoding and the TTL carried whole. A `RESTORE` only takes a
payload up to the RDB version its own server writes, and the two flavors
number theirs apart. Measured on 2026-09-26: Redis 7.4 writes 12 and
Redis 8.10 15, Valkey 8.0 writes 11 and Valkey 9.1 80. So a copy between
them is refused — "DUMP payload version or checksum are wrong" — in every
direction but Valkey 8 → Redis, and a migration from a Redis 8 to a Valkey
9 failed every key with that line and nothing to be done about it.

The refusal is not a bug to route around: Redis 8 and Valkey 9 have each
changed what a payload can hold (a hash field's TTL, a new encoding), and a
server that accepted the other's payload would be guessing.

## Decision

The refusal is answered by re-creating the key from what the source holds,
by type, in commands both sides read and write: `GET` / `SET`, `HSCAN` /
`HSET`, `LRANGE` / `RPUSH`, `SSCAN` / `SADD`, `ZSCAN` / `ZADD`, `XRANGE` /
`XADD` under each entry's own id, `JSON.GET` / `JSON.SET`, paged at 500
elements and finished with `PEXPIRE` for the TTL the dump saw. It fires on
that one refusal (`is_foreign_payload`) and on nothing else: a `BUSYKEY`
is still the conflict question, an `OOM` is still the target's answer.
The outcome is its own status, `RestoreStatus::Recreated`, so the
migration log can say which keys travelled that way rather than fold them
into *written*.

What it cannot carry, it names rather than approximates: a module type
other than JSON (a Bloom filter, a time series, a vector set have no
portable read), a stream's consumer groups, a hash's per-field TTLs, a
stream with no entries. A `.zdis` file holds payloads and no source to
read from, so its import keeps the refusal and tells the person to export
as JSON instead — the readable formats were always the logical ones.

## Consequences

- A migration between a Redis and a Valkey lands in both directions; the
  cost is round trips per key instead of one, which is what a cross-flavor
  move is worth.
- `copy_key` and the migration's server-to-server path go through
  `restore_or_recreate_chunk`; a new path that restores from a *server*
  should too, and one that restores from a *file* cannot.
- The rule is measured, not read: the RDB versions above are what
  `DUMP` answered on the images named, and the live test
  `standalone_copy_lands_on_a_server_of_the_other_flavor` runs against a
  server of the other flavor by hand (`ZEDIS_IT_FOREIGN`), since the
  matrix has one flavor per lane.
