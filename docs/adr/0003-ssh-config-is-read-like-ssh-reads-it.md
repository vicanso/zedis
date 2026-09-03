# 3. `~/.ssh/config` is read the way `ssh` reads it — silently, only to fill blanks

Date: 2026-09-03 · Status: accepted

## Context

Users type a `Host` alias from their ssh config into the tunnel address and
expect its `HostName`, `User`, `Port` and `IdentityFile` to apply (issue #120).
The `~/.ssh` directory is sensitive, and the question was whether an app should
read it at all, and whether to ask first.

## Decision

Read it, without a setting and without a prompt, under the same discipline the
app already applies to `~/.ssh/known_hosts`: read-only, never modified, and only
what the user's own configuration implies. This is what every SSH client does
and what the file exists for. What is *not* silent is the effect: form values
always win over the file, only blank fields are filled, and the diagnostics SSH
stage lists every value the file contributed, so a `Host *` `IdentityFile` or a
`HostName` rewrite can be seen when something behaves unexpectedly.

Scope: `Host` patterns with globs and negation, first value wins, `Match`
skipped, `Include` not followed, `ProxyJump` one hop. An `IdentityFile` the
client cannot use as a file (encrypted without a passphrase, unreadable) falls
back to the agent instead of failing — the agent served it before the file was
read. Nothing resolved is persisted; the file is re-read on every connect.

## Consequences

- The sandboxed (App Store) build has no home directory and simply reads
  nothing — the same silent degradation as known_hosts there.
- Documentation states the behaviour, as it does for known_hosts.
