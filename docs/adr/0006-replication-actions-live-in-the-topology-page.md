# 6. Replication actions live in the Topology page

Date: 2026-09-06 · Status: accepted

## Context

A standalone primary / replica pair had no place in the app: the status
bar tooltip showed the replicas of whatever node the heartbeat polled, the
Topology page rendered a placeholder for a standalone server, and the
three commands that change the pair — `REPLICAOF host port`,
`REPLICAOF NO ONE`, `FAILOVER` — could only be typed into the terminal,
which did not even flag them as dangerous. The obvious fix was a fourth
surface, a "Replication" panel.

Replication is not one feature but three, one per deployment shape. A
cluster changes its links with `CLUSTER REPLICATE` / `CLUSTER FAILOVER`
and refuses `REPLICAOF`. A sentinel deployment fails over with `SENTINEL
FAILOVER`, and a hand-made `REPLICAOF` on one of its data nodes is
reverted by the sentinels within seconds. Both of those already live in
the Topology page. Managed clouds reject the standalone commands outright.
`REPLICAOF host port` is also the most destructive command after
`FLUSHALL` — the node throws its dataset away for a full sync — and
`FAILOVER` pauses writes on the primary until the replica has caught up.

## Decision

The Topology page's standalone mode *is* the replication view. It renders
the pair from the heartbeat's `INFO replication` (kept whole in
`RedisInfo::replication`, so the page is live at the heartbeat's cadence
with no extra polling): this server with its replicas and their lag, or
this server under the primary it follows with the state of the link. The
three commands are buttons in that body and nowhere else — Sentinel and
Cluster entries never render it, so the shape decides which failover a
user can reach.

Each command goes through the alert dialog with the production escalation,
and the body says what is at stake (the dataset for `REPLICAOF`, the
unsynced writes for `FORCE`). `FAILOVER` always carries a `TIMEOUT`
(`FAILOVER_TIMEOUT_MS`): without one a stalled replica can hold the
primary's writes indefinitely; on expiry the failover is abandoned and
writes resume, which is the safe default. `FORCE` is a second, quieter
button because Redis only accepts it with a target and a timeout, and
because it is the one variant that loses writes.

The gates are the usual ones: `Capability::ReplicationWrite` (read-only
mode, and `REPLICAOF` probed through `ACL DRYRUN` so a managed cloud shows
the unavailable chip), `floors::FAILOVER` for the 6.2 floor, and
`ServerCommand::Failover` for a proxy that has the version but not the
command. The terminal classifies `REPLICAOF` / `SLAVEOF` / `FAILOVER` as
destructive (`DangerKind::Replication`).

## Consequences

- One page per deployment shape for "who replicates whom, and how to
  change it"; no fourth surface to keep in step with the status bar.
- A user on a Sentinel or Cluster entry cannot send a standalone
  `REPLICAOF` from the UI at all, only from the terminal, where it now
  asks for confirmation.
- The live suite runs the three commands against a pair of its own
  (`scripts/it/up.sh`'s `replication` scenario) — promoting, re-linking
  and failing over the sentinel's pairs would upset the sentinel tests
  running alongside.
- After a `FAILOVER` the entry still points at the same node, now a
  replica: the status bar's replica chip says so, and writes fail with
  `READONLY` until the user switches entries or fails back.
