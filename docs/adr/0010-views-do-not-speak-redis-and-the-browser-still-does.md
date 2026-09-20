# 10. Views do not speak Redis, and the browser still does

Date: 2026-09-19 · Status: accepted

## Context

ADR 9 cut the web build at `RedisAsyncConn`, and said why the two layers above
it could not be the cut: 35 view files, 90 `get_connection_manager()` calls and
65 hand-written `cmd(...)` sites went around them. That was a measurement taken
to choose a seam. It is also a description of a view layer that knows what a
Redis command looks like, holds connections, and matches on `redis::Value` —
and so cannot be lifted out, reused, or tested without a server behind it.
None of those 65 commands had a live test: a command inside a view is not
reachable from `tests/live.rs`.

Two different things could be meant by "the UI should not depend on Redis",
and they have different answers.

**The view layer should not build commands.** The repository already shows it
can be done: `acl_manager`, `function_editor` and `search_manager` contain no
`cmd(` at all, because `zedis-connection` has typed operations for them. The
other views predate that habit.

**The browser bundle should not contain the `redis` crate** — that is, the
bridge should speak features instead of RESP. Measured on the web build of this
date: the `redis` crate is 48 KiB of a 19.65 MiB code section, 105 KiB with
its parser (`combine`) — about 0.5%. In the browser it opens nothing (`aio` is
not compiled there); it packs a `Cmd` into bytes and parses bytes into a
`Value`. Against that saving stands what ADR 9 counted and what still holds:
102 exported operations plus 86 commands the state layer builds inline, each
needing an endpoint and `Serialize` on its arguments and result; a protocol
that grows with every panel; a bridge that must be released in step with the
page instead of never changing; and the terminal, which needs a raw passthrough
whatever else is decided.

## Decision

**Views do not speak Redis.** `src/views` draws and asks. An operation a view
needs lives in `zedis-connection`, typed, one module per Redis feature, and
returns a struct the view can draw rather than a `redis::Value`. It takes a
`ServerDb` — which database of which configured server — and finds its own
connection; `ServerDb::connection()` is `pub(crate)` so that a view cannot get
a connection out of it. It gets a live test in the same change, because the
move is what makes one possible.

This is held by a ratchet, not asserted as a fact: `tests/view_layering.rs`
counts, per file under `src/views`, commands built, commands run, connections
taken and `redis::` paths against `tests/view_layering.baseline`. A count may
only shrink, and the baseline must follow it in the same commit. Done is an
empty baseline. It started at 33 files / 65 / 68 / 90 / 26.

**The state layer follows** once the views are done: its 86 inline commands
become typed operations the same way. At that point no file of the GUI crate
builds a command, `redis` is an implementation detail of `zedis-connection`,
and the GUI crate's manifest can drop it.

**The browser still packs RESP.** ADR 9 stands. Removing `redis` from the
wasm bundle buys half a percent and costs the one property that made the
bridge cheap: one endpoint that never learns a command.

## Progress

2026-09-20 — done, in two steps on the same day.

The views reached an empty baseline first. Two shapes had to be found that
the earlier views did not need. What the server *pushes* (Pub/Sub, `MONITOR`,
a blocking `XREAD`) became types that own their connection and hand out the
next item — `ChannelSubscription`, `MonitorFeed`, `StreamTail` — so the view
keeps its loop and nothing else. And the terminal, whose job is to show a raw
reply, holds an opaque `TerminalReply` it can ask questions of and render,
with `TerminalSession` owning the dedicated connection; its tests write
replies as RESP bytes.

The scan was then widened to all of `src` and the state layer followed, which
took two more shapes. A **cluster** command is not a `ServerDb` operation:
`REPLICATE`, `FAILOVER`, `ADDSLOTS` and `SETSLOT` are not gossiped, so they
take a `ClusterNode` — which node of which cluster — while `MEET` and
`FORGET`, which are told to every master, keep the `ServerDb`. And the
connection crate has no gpui, so its structs are `String` where the views
want `SharedString`: the conversion is one function on the way in, not a
`SharedString` dragged into a lib crate.

The baseline is now empty for the whole GUI crate, and its manifest no longer
lists `redis` on either target. The test stays, as a rule rather than a
ratchet: a failure means the new code belongs in `zedis-connection`.

## Consequences

- One view per change, smallest first (`CLAUDE.md` lists the order), so each
  can be reviewed and reverted alone. A view's helper methods on its data move
  with the type: a view cannot `impl` a struct from another crate.
- Older operations that take `&mut RedisAsyncConn` are converted to `ServerDb`
  as their views are, not all at once.
- Every operation ends up with the shape `fn(at: &ServerDb, args…) ->
  Result<T>`. That is exactly where an RPC boundary would go, so this work
  makes the ADR 9 decision *reversible* without making it: a feature-level
  bridge would be a forwarding layer at `ServerDb`, not a rewrite of the views.
- Revisit "the browser still packs RESP" when one of these appears, and not
  before: the bridge has to authorize or audit *per command* (it would then be
  parsing RESP to find out what it is forwarding — `policy.rs` already does a
  little of this for destructive commands); a second front end needs an API
  (mobile, a CLI, an integration), for which RESP is not one; or RESP2 / RESP3
  differences start to be debugged on both sides of the bridge.
