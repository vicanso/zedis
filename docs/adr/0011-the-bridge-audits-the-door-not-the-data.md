# 11. The bridge audits the door, not the data

Date: 2026-09-25 · Status: accepted

## Context

A bridge with accounts, a read-only role, entries that are private or shared
and a confirmation for destructive commands is a shared door into production,
and the first thing a shared door is asked afterwards is who went through it.
`zedis-bridge` logged logins at `info` and refusals at `warn`, the refusals
without the account or the command; nothing recorded a `CONFIG SET`, an
`ACL SETUSER`, or an entry made shared.

ADR 10 named "the bridge has to authorize or audit *per command*" as the point
at which to revisit forwarding RESP frames, because the bridge would then be
parsing commands to know what it forwards. By the time audit came up it already
was: the read-only role (`policy::check`) judges every command in every batch.
The parsing that a feature-level bridge was supposed to bring is there, and the
audit line is written at that same place.

## Decision

One JSON object per line, appended to the file `--audit-log` names, written
**after the outcome is known** so it says what happened rather than what was
attempted (`error` carries an upstream failure). What is kept:

- **The bridge's own events**: a login, a failed login (passwords are guessable
  and a run of wrong ones shows nowhere else), a logout, a refusal by role.
- **Server entries**: added (the settings), edited (each setting from → to;
  the *names* of the secrets that changed, never a value; a private entry made
  shared, which hands its credentials to every account), deleted.
- **Commands that administer the server** rather than its data —
  `zedis_connection::is_administration_command`: who may connect (`ACL`),
  what code runs (`MODULE`, `FUNCTION`, `SCRIPT`), where the data comes from
  (`REPLICAOF`, the `CLUSTER` writes), whether it is running (`SHUTDOWN`,
  `CLIENT KILL`, `SAVE`), and `CONFIG SET` — plus **every command a person had
  to confirm**. The audit set contains the confirmation set by construction,
  so an entry with `require_confirm_writes` set is also an entry whose every
  write is logged. `policy::Verdict::Confirmed` exists to tell that apart from
  a plain `Allow`.
- **Data writes** only with `--audit-writes`, off by default.
- **Reads, never.** The key tree is a stream of `SCAN`s and every open tab a
  heartbeat of `INFO`s; a log that carried them would bury the rest.

Two things this deliberately is not:

- **A history of the data.** The applications write more in a second than every
  GUI user in a day, and none of it passes here. The log answers "did anyone do
  this by hand, and who" — which is the question an incident actually asks, and
  to which "nobody, it was the application" is a useful answer — not "who
  changed this key". That is why writes are opt-in and a per-entry switch
  covers the servers where by-hand matters.
- **The server's log.** `redis-cli` and the applications reach Redis without
  this process; `ACL LOG` and `MONITOR` are the server's own. This is the log
  of one door.

Passwords never reach the file: `zedis_connection::redact_secrets` blanks
`AUTH`, `HELLO … AUTH`, `ACL SETUSER >…`, `CONFIG SET requirepass` /
`masterauth`, `MIGRATE … AUTH` and `SENTINEL SET … auth-pass`, and a command
that takes a credential in an argument is added there in the change that
sends it. A value is cut to a length (a `SET` is not a backup), a batch of one
command name is one line with a count, and the file is owner-only.

A line that cannot be written is reported in the process log and the request
goes on. Refusing work when the disk is full would turn an audit failure into
an outage, and a bridge that stops is a bridge someone goes around — the
opposite of what an audit is for. A path that cannot be *opened* stops the
bridge at startup instead: an audit that was configured and is silently absent
is worse than none.

## Consequences

- The `X-Forwarded-For` value is recorded beside the peer address, never in its
  place: the header is whatever the previous hop wrote, and only the deployment
  knows whether that hop is its own proxy. The line carries both.
- No rotation: the bridge appends and never reopens. `logrotate` with
  `copytruncate`, or a fresh path on restart.
- ADR 10's revisit condition narrows. Audit *by command* is met here without
  the feature-level bridge; audit *by feature* ("exported key X as CSV") or a
  second role beyond read-only would still be the signal.
- A new mutating route calls `authorize_write` with an action name, and a new
  route that forwards commands goes through `audit::command_lines` — there is
  no third way to change something through this process.
