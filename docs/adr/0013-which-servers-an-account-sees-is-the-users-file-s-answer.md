# 13. Which servers an account sees is the users file's answer

Date: 2026-09-26 · Status: accepted

## Context

An entry on the bridge is private (its owner's) or shared (everyone's), and
that is all: nothing lets an operator say "only these two people see
production". Two shapes were on the table. **Self-service**: a `shared_with`
list on the entry, edited from the page by whoever can see the entry, with a
new route listing account names and a multi-select in the form. **Operator**:
a `servers` list on the account in the users file, consulted when a shared
entry is filtered, with no page change at all.

## Decision

The operator's. "Only A sees production" is the operator's decision, not the
entry owner's; the role already lives in the users file, and ADR 12 has just
settled that what an account may do is that file's business — so restricting
which shared entries it may see belongs beside it, and it composes with an
identity a proxy asserts the same way the role does.

```toml
[[users]]
name = "carol"
password = "…"
servers = ["prod-*:ro", "prod-eu", "id:0199…"]
```

A rule names entries by name (`*`, `?`) or by `id:`; `:ro` grants an entry
read-only. `visible_to` asks the list for a shared entry and never for the
account's own, and `is_read_only_on` — the role *on this entry* — replaces
the account-wide `is_read_only` on every write path: `policy::check` for
commands, entry edit and delete for the list. Left out, the list is every
shared entry; empty, none.

Three choices inside that:

- **Write wins where rules disagree.** `["prod-*:ro", "prod-eu"]` reads every
  prod and writes `prod-eu`. The alternative, first match, makes a hand-edited
  list change meaning when a line moves; the alternative, read wins, makes
  the exception impossible to spell. With write winning, the broad rule is the
  restriction and the exceptions are named.
- **Its own entries are its own.** An account that added an entry holds its
  credentials; a rule that made it read-only on its own entry would restrict
  nothing and confuse.
- **Read-only per entry reaches the page per entry.** The pool's
  `set_account_read_only` was one flag for the whole session; it is now a set
  of entry ids (`set_account_read_only_on`), so `:ro` on production leaves
  staging's buttons lit.

## Consequences

- Only the file has this; the inline `ZEDIS_BRIDGE_USERS` form has no room
  for it and is not stretched to.
- Names are the owners' to change. An account that can edit an entry can
  rename it into or out of a pattern; that needs the access it already has, so
  it is not an escalation, and `id:` is there for the case where it matters.
  README says so.
- The self-service shape is not ruled out; it would sit on top of this one
  (an entry's `shared_with` narrowing further), and waits for someone to ask.
