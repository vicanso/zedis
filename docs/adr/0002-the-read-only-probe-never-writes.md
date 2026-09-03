# 2. The read-only probe never writes a key

Date: 2026-09-03 · Status: accepted

## Context

`AccessMode::StrictReadOnly` is detected at connect so the UI can grey out
writes for an ACL user who may not write. On Redis 7+ that is `ACL DRYRUN`, a
pure simulation. Before 7.0, and for users who may not run DRYRUN (it is
`@admin`, which is exactly what a restricted user lacks), the fallback used to
execute `SET _zedis_auth_test_ 1 EX 1` — a real write on production that
fired `set` / `expire` / `expired` keyspace events on every connect.

## Decision

The fallback sends `SET <fresh uuid key> 1 XX`: the ACL check runs before
execution (so a denial is still a `NOPERM`), and `XX` on an absent key aborts
with nil before the dataset is touched — no key, no keyspace event, nothing
replicated or appended to the AOF. Denials are read for *what* was denied:
only "may not run the command" is read-only; "may not access the key" means a
key-scoped user (`~app:*`) who keeps the write UI. `READONLY` (a replica),
`OOM` and proxy refusals are inconclusive and lean writable, because a wrongly
locked UI blocks real work while a wrongly unlocked one just surfaces the
server's own `NOPERM` on the first write.

## Consequences

- The probe still shows up in `MONITOR`, `INFO commandstats` and, when denied,
  `ACL LOG`. Removing even that would mean not probing at all and flipping the
  mode on the first refused write — rejected for now; the lock would appear
  only after a failure.
- A `%R~*` read-only key permission is indistinguishable from a key scope and
  reads as writable; the server refuses the write and the UI degrades.
