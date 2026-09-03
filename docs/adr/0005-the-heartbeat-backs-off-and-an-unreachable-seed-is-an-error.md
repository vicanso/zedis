# 5. The heartbeat backs off, and an unreachable seed is an error

Date: 2026-09-03 · Status: accepted

## Context

Every workspace tab's status bar ticks a heartbeat every 2 seconds:
`get_client` + `PING` + `INFO`. When the pooled client is gone, `get_client`
is a full connect — DNS, the TLS or SSH handshake, server-type detection,
Sentinel / Cluster discovery. Against a server that closes every connection
that meant an attempt every 2 seconds for as long as the tab stayed open,
with no backoff and no cap, and each attempt overlapping the last while a
10 second connect timeout ran out. Node discovery made it worse: a seed the
helper could not reach was logged as "detect server type failed, use
standalone mode" and re-dialled as a standalone — the same endpoint, the same
credentials and TLS material — so every tick paid two handshakes and reported
the failure as a standalone whose `PING` then failed a second later.

## Decision

`refresh_redis_info` owns the cadence; the status bar's timer is only the
metronome. One attempt is in flight at a time. After a failed `PING` the
next attempt waits the heartbeat interval doubled per consecutive miss —
2s, 4s, 8s, 16s, 32s — and 60s from there; a healthy `PING`, any other task
that reaches the server, or a fresh select clears the wait. A background tab
still polls every 30s, on top of the backoff. The manual disconnect on the
status-bar dot stays the only way to stop the heartbeat altogether.

`get_redis_nodes` no longer treats a seed it cannot connect to as a
standalone: the connection error is returned. The standalone fallback is
kept for a server that answers but rejects the detection commands
(restricted proxies), which is the case it was written for.

## Consequences

- A user watching an unreachable server sees the health dot go Reconnecting
  then Offline as before, but the log carries one failure per backoff step,
  not one every 2 seconds, and the error names the real cause (TLS, network,
  tunnel) instead of a standalone `PING`.
- Recovery is noticed within the current backoff step (at most 60s) unless
  the user acts sooner — clicking a key or the reconnect dot dials at once.
- A Sentinel or Cluster entry whose every seed is down now fails the
  connect outright instead of pretending the first seed is a standalone.
