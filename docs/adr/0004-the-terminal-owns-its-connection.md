# 4. The terminal owns a connection; the pool never carries connection-scoped state

Date: 2026-09-03 · Status: accepted

## Context

The terminal ran lines on the pooled multiplexed connection shared with the
key tree, metrics and every panel. `SELECT`, `AUTH`, `CLIENT SETNAME`,
`CLIENT TRACKING` and `MULTI` change the state of whichever connection carries
them, so a `SELECT 3` typed in the terminal silently moved every later `SCAN`
to db 3.

## Decision

Connection-scoped commands only ever run on a connection whose lifetime one
owner controls. The terminal gets one through
`ConnectionManager::open_dedicated_connection` — built from the cached client's
topology (no discovery re-run), never cached, never heartbeat-checked: opened
lazily, reused across lines and batches (a `SELECT` on line one holds for line
two), replaced on a server or db switch, dropped after a link error so the next
line reconnects. The prompt shows `[n]` while the terminal sits on a different
db than the panel. Blocking commands are still refused up front: they would
park that connection until data arrives, and the response timeout would cut
the wait short and leave the connection out of step.

The same rule already applies to `MONITOR`, `XREAD BLOCK` and sharded Pub/Sub,
each on a dedicated connection.

## Consequences

- Anything new that sends a connection-scoped or blocking command gets its own
  connection; it must never borrow the pool's.
- A dedicated connection is not healed in the background; its owner reopens
  it after a link error.
