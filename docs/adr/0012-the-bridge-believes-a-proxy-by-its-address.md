# 12. The bridge believes a proxy's identity header by the proxy's address

Date: 2026-09-26 · Status: accepted

## Context

A team that already has single sign-on asks the same first question of every
self-hosted tool: can it use ours? Implementing OIDC in the bridge would
answer it for the providers it was tested against and add a client id, a
secret, a callback URL and a token cache to a process whose whole auth story
is a users file. The standard alternative is what Grafana, Gitea and most
self-hosted tools offer: an authenticating reverse proxy (oauth2-proxy,
Authelia, Pomerium, Cloudflare Access, Tailscale) signs the person in and
writes who they are into a request header, and the application believes the
header. The proxy speaks to the identity provider; the application speaks to
nobody.

The header is the whole weakness of that scheme: anyone can write
`Remote-User: alice` into a request.

## Decision

`--trusted-header <name>` names the header and `--trusted-proxy <cidr,…>` the
addresses it is believed from, and the bridge refuses to start with one and
not the other. The check is against the socket's peer address — never
`X-Forwarded-For`, which is itself a header — with a v4 peer of a `[::]`
listener compared as the v4 address it is. On a request from those
addresses the header's value is the caller; from anywhere else the header is
ignored as if absent, and the password paths (HTTP Basic, the login cookie)
answer as before. With neither setting the header is never read.

The name has to be an account. The proxy says who someone is, not what they
may do, and the role and the owned entries hang on the name — so the users
file stays the list of who may use the bridge, and its `password` becomes
optional for accounts the proxy signs in (an account without one is refused
at startup when there is no proxy, since nothing could sign it in). A name
that is no account answers `403` with the name in the message and an audit
line (`no_account`), rather than being provisioned as a new user: an
operator who wants a person in adds a line, which is one line, and an
operator who did not is told rather than surprised.

Not done, on purpose: mapping a groups header (`Remote-Groups`) onto the
read-only role. It is one more header name away, and it is the moment the
users file stops being the answer to "who may write" — wait for someone to
need it.

## Consequences

- The proxy must strip or overwrite the header on every request it forwards,
  and must be the only route to the bridge. A bridge that is also reachable
  directly is one where the address check protects nothing. README says both.
- The page needs no change: it shows its login form only when `/v1/servers`
  answers `401`, and behind the proxy that answers `200`. There is no sign-out
  control to hide; signing out is the proxy's.
- `authorize` takes the request's `Origin` mutably so that every audit line of
  a proxy-identified request carries `auth: "proxy"`, and the only login
  events such a deployment produces are the `no_account` refusals.
- Every handler now extracts the peer address, which the audit log wanted
  anyway.
