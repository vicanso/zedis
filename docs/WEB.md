[中文](./WEB_zh.md) | English · [← Back to README](../README.md)

# Zedis Web: Self-Hosting Guide

Zedis also runs in the browser: the same app, compiled to WebAssembly and drawn on a canvas. A browser cannot open a TCP socket, so a small HTTP server — `zedis-bridge` — serves the page and talks to Redis on the browser's behalf. Host it once next to your Redis servers and the whole team reaches them from a browser tab, with nothing to install. Redis passwords stay on the bridge, encrypted at rest; the browser never receives them.

<p align="center">
  <img src="images/architecture.svg" width="100%" alt="Code-sharing diagram, not a deployment: the Zedis UI and the Redis layer are code compiled into more than one program, and neither runs as a service of its own. The desktop app is one native process holding both, and its UI calls the Redis layer in-process. In the web version the same UI runs as WebAssembly in a browser tab and calls zedis-bridge over an HTTP API; the bridge is a separate server process that turns those calls into Redis commands with the same Redis layer. The desktop app and the bridge each talk RESP to standalone, Sentinel and Cluster deployments of Redis and Valkey.">
</p>

> **Early preview.** The image is published for linux/amd64 and linux/arm64 (~26 MB): `:latest` and the release version for tagged releases, `:nightly` for the rolling build from `main`.

## Try it

```bash
docker run -d --name zedis-web -p 7379:7379 \
  -v zedis-data:/data \
  vicanso/zedis-web:latest --listen 0.0.0.0:7379 --insecure-cookie
docker logs zedis-web 2>&1 | grep "setup token"
```

Open <http://localhost:7379>. Until the bridge has an administrator, the page shows **Create the administrator**: give it the setup token from the log, a name and a password (at least 8 characters), and you are signed in as that administrator. Everyone else is invited from **Account…** in the title-bar menu (see [Accounts and sharing](#accounts-and-sharing)).

- **Accounts are registered, not configured.** The first one is the administrator, made with the one-time setup token that the bridge prints in its log and writes to `/data/setup-token` (owner-only, deleted once used); `-e ZEDIS_BRIDGE_SETUP_TOKEN=…` chooses the token in advance instead, for an install script. Everyone else registers through an invitation link — or, where `--signup approval` is set, asks for an account on the sign-in page and waits for an administrator. Scripts sign in as an account with HTTP Basic (`curl -u admin:… …/v1/servers`).
- **`/data`** holds the server list (secrets encrypted with the `master.key` file beside it), the accounts (passwords as Argon2id hashes) and the saved logins. Keep the volume, or every restart starts empty, without an administrator.
- **`--insecure-cookie` is for a plain-http trial only.** The login cookie is `Secure` by default, and a browser silently drops a `Secure` cookie that arrives over plain http from anything but `localhost` (and some browsers drop it even there). Without the flag the sign-in succeeds and the very next request gets a `401`.
- **A Redis on the Docker host** is not `127.0.0.1` from inside the container. Use `host.docker.internal` (on Linux, add `--add-host=host.docker.internal:host-gateway`) or `--network host`.

## Deploy it

Over plain http the account password and everything read from Redis cross the network in the clear. For anything beyond a trial, drop `--insecure-cookie`, publish the port to loopback only, and put an HTTPS reverse proxy in front:

```bash
docker run -d --name zedis-web -p 127.0.0.1:7379:7379 \
  -v zedis-data:/data \
  vicanso/zedis-web:latest
```

```caddyfile
zedis.example.com {
    reverse_proxy 127.0.0.1:7379
}
```

Create the administrator before the address is given to anyone: until then, whoever has the setup token is the one who does. After ten failed attempts in a quarter of an hour the bridge refuses more from that address (`429`) — counted per account name, and the same for the setup token and invitation codes. Behind a reverse proxy every request comes from the proxy's address, so that count is then per name for everyone; add a rate limit at the proxy as well if the bridge is reachable from outside your own network.

The bridge compresses what it sends by itself: the page's files once, when the image is built (brotli and gzip), and the API's answers as it sends them — zstd where the browser takes it (Chrome 123, Firefox 126, Safari 26.3 on macOS 26.3), gzip otherwise, from a kilobyte up. A proxy in front needs no compression of its own; Caddy's `encode` and nginx's `gzip` pass an answer that is already encoded through unchanged.

## Sharing a host name with other applications

When the host name is not Zedis's alone, give the bridge a path of its own with `ZEDIS_BRIDGE_BASE_PATH` (or `--base-path`). The page, its files and the API all move under it, nothing outside it answers, and the login cookie is scoped to it — so the other applications on that host never receive it.

```bash
docker run -d --name zedis-web -p 127.0.0.1:7379:7379 \
  -e ZEDIS_BRIDGE_BASE_PATH=/zedis \
  -v zedis-data:/data \
  vicanso/zedis-web:latest
```

```caddyfile
tools.example.com {
    # `handle`, not `handle_path`: the prefix is forwarded, not stripped.
    handle /zedis* {
        reverse_proxy 127.0.0.1:7379
    }
    # … the other applications
}
```

Open `https://tools.example.com/zedis/`. For nginx the equivalent is `location /zedis { proxy_pass http://127.0.0.1:7379; }` — no trailing slash on `proxy_pass`, which would strip the prefix. The health check moves too: `/zedis/v1/health`.

## Several bridges on one database

By default a bridge keeps everything in its data directory: one process, nothing else to run, and a backup is one directory to copy. To run more than one bridge — behind a load balancer, or for a bridge that may go down without signing everyone out — or to have an audit you can search, give every bridge the same PostgreSQL 14+ or MySQL 8.0.19+ database:

```bash
docker run -d --name zedis-web -p 127.0.0.1:7379:7379 \
  -v zedis-data:/data \
  -e ZEDIS_BRIDGE_STORE='postgres://zedis:…@db.internal:5432/zedis?sslmode=require' \
  vicanso/zedis-web:latest
```

It comes with the first release after v0.14.0; until then use `vicanso/zedis-web:nightly`. The [PostgreSQL and MySQL storage guide](./SQL_STORE.md) has the Docker commands for both databases, the database user and its privileges, TLS, backups, upgrades and the schema; what follows is the short version.

`ZEDIS_BRIDGE_STORE` (or `--store`) is a `postgres://…` or `mysql://…` URL. Prefer the environment: the URL carries a password, and a flag is visible to anyone who can list processes. The log only ever shows its host. The bridge creates its tables on start and brings them up to date with each release, under the database's own lock, so bridges starting together do it once; a bridge older than the tables refuses to start. MariaDB is not supported.

What the database then holds: the accounts, invitations, requests for an account and MCP tokens, the logins, the server list, the recycle bin, the Metrics history, the page's data, the setup token and failed sign-ins. A browser signed in on one bridge is signed in on all of them, ten failed sign-ins are ten across all of them, and a change made on one — an account disabled, a server rule removed, a sign-out — reaches the others within **two seconds**. The audit is on and kept there too (see [Audit log](#audit-log)).

Three things to arrange:

- **One `master.key` for every bridge.** It encrypts the server passwords and the recycle bin before they reach the database. The first bridge to open an empty database seals a check value with its key, and a bridge with another key refuses to start rather than write passwords the others cannot read. Mount the same file into each (`/data/master.key`), and back it up apart from the database: a dump of one is no use without the other, and should not travel with it.
- **Keep each browser on one bridge**, by the `zedis_bridge` login cookie (sticky sessions on the load balancer). Signing in does not need it — what does is what is not data: the terminal's session and the Pub/Sub, Monitor and keyspace-notification panels are open Redis connections inside one bridge, and their requests reaching another are refused; an unlocked write window is asked for again there. A bridge that goes down costs a page those, not its sign-in. Who is online (`GET /v1/clients`) and the MCP call limit are counted per bridge.
- **Health checks.** Point the load balancer at `/v1/ready`: it answers `503` while that bridge's database does not answer within two seconds, so the traffic goes to the others, and `200` again once it does. `/v1/health` only says the process is up — the one for a container's liveness probe, since restarting a bridge whose database is away mends nothing.
- **Synchronized clocks** (NTP), as for any service on several hosts: logins expire, the bin empties and the metrics are sampled by each bridge's clock.

MySQL 8 signs in with `caching_sha2_password`; ask for TLS (`?ssl-mode=REQUIRED`) — without it the bridge sends the password RSA-encrypted, but everything after it travels in the clear. `zedis-bridge setup-token` takes the same `ZEDIS_BRIDGE_STORE` and keeps the new token in the database.

**Moving an existing bridge.** Stop it, then copy its data directory into the empty database once — the accounts, invitations and waiting requests, logins (nobody signs in again), the server list, scripts, the page's data, the recycle bin and the Metrics history as they were, a waiting setup token and, with `--audit-file`, the audit file:

```bash
docker run --rm -v zedis-data:/data \
  -e ZEDIS_BRIDGE_STORE='postgres://…' \
  vicanso/zedis-web:latest import-local --audit-file /data/audit.log
```

It runs with that directory's `master.key`, which every bridge on the database uses from then on, and refuses a database that already holds accounts, server entries or scripts. There is no way back but the database's own backups.

## Accounts and sharing

A server entry belongs to the account that added it and nobody else sees it, unless its **Shared** box is ticked — then it is everyone's. Any account that can see a shared entry and may write may edit or delete it.

Accounts are kept by the bridge (`bridge-accounts.json` in `/data`, owner-only) and made on its pages, never written into its configuration. Each has one of three roles:

- **Administrator** — what a full account may do, plus the accounts, invitations and requests for an account on the account page.
- **Full access** — reads and writes on every entry it can see.
- **Read-only** — reads only (below).

The first account is the administrator, made with the setup token (see [Try it](#try-it)). Every account after it comes through an **invitation**: on the account page (**Account…** in the title-bar menu) an administrator chooses the role, the shared servers it sees and how many days the link is valid (7 by default, at most 90), and sends the link that comes back. It registers one account, with the name and password the person picks, and is then used up; the bridge keeps only a hash of its code, and an open invitation can be revoked on the same page. The code travels in the link's `#` part, which the browser does not send in a request, so it does not reach the access log of a proxy on the way.

**Requests for an account** are a second way in, off unless you turn it on: `--signup approval` (`-e ZEDIS_BRIDGE_SIGNUP=approval`). The sign-in page then offers **Request an account** — a name, a password and an optional note for the administrator. A request signs nobody in and sees nothing: it waits in the account page's **Requests** section, where an administrator picks the role and the shared servers (read-only and every shared entry unless changed) and approves it, which makes the account with the password the person chose, or rejects it. The answer to whoever asks is the same whether the name is taken or not, so the form does not tell anyone which accounts exist; a request for a name an account already has is marked *Name taken* and can only be rejected. Requests are counted against the address in the same ten-a-quarter-hour limit as failed sign-ins, at most 100 wait at once, and one nobody answers is forgotten after seven days. Each is an audit line (`signup_requested`, `signup_approved`, `signup_rejected`), and `--audit-webhook` pushes `signup_requested` by default — that is how an administrator hears someone is waiting. A name a sign-in proxy asserts never becomes a request: behind `--trusted-header`, accounts keep coming through invitations marked for the proxy.

**A demonstration account** that many people sign in to: tick **Locked** on its form (not offered for an administrator). It then cannot change its own password or MCP tokens — an administrator still can — and the bridge keeps no favorites, recent keys, search history or preferences for it, so one visitor's language or favorites never become the next one's: the preferences stay in each browser, the rest for as long as the page is open. Make it read-only too, with server rules that name only the demonstration's entries.

On the same page, the accounts table lets an administrator change an account's role and server rules, disable it, delete it, or make a **Reset link…** for someone who has forgotten their password — it sets a new one once. Everyone changes their own password on the account page. The last administrator cannot be demoted, disabled or deleted. If every administrator is lost anyway, run `zedis-bridge setup-token` on the host (`docker exec zedis-web /usr/local/bin/zedis-bridge setup-token`): it prints a new setup token and the page offers *Create the administrator* again, to a new account or to an existing one by its own password. Nothing over HTTP can do that — access to the host is access to the bridge.

Deleting an account ends its logins and tokens and cannot be undone; ticking **Disabled** instead stops it signing in and can be. What the account kept to itself — its private server entries and saved scripts — is not left on the bridge where nobody sees it: the confirmation lists them and asks what becomes of them. They are handed to another account (the administrator deleting, unless another is picked; never a read-only or disabled one), shared with every account, or deleted with it — an entry's recycle bin, metrics history and key notes too. Sharing is never what is picked for you, because a private entry carries its owner's Redis password. Each one is an audit line (`server_updated` with the owner from → to, `server_deleted`, `script_saved`, `script_deleted`), and `account_deleted` says where they went. A script deleting through `DELETE /v1/admin/accounts/{name}` says the same in its body — `{"private":{"action":"hand_to","account":"alice"}}`, `{"private":{"action":"share"}}` or `{"private":{"action":"delete"}}` — and without one they go to the account it signs in as. An account deleted before this existed may have left such things behind; the accounts section lists them under *Left behind by deleted accounts*, each with **Settle…** and the same three choices (`orphans_settled`).

The server rules narrow which **shared** entries an account sees, one rule a line — a name, with `*` and `?` as wildcards, or `id:` and an entry's id — and `:ro` at the end of a rule makes the account read-only there while it keeps its full role elsewhere. Where rules disagree about one entry, write wins, so make the broad rule the restriction and name the exceptions: with `prod-*:ro` and `prod-eu` the account reads every `prod-*` entry and writes `prod-eu`. With no rules the account sees every shared entry. An account always sees and writes its own entries. Names are whatever their owners typed, so an account that can edit an entry can rename it into or out of a pattern — use `id:` where that matters.

A **read-only** account may look at everything it can see and change none of it: no writes to Redis, and no adding, editing or deleting a server entry. The refusal is the bridge's, not the page's — it answers `403` to anything that is not a read, so a script posting straight to `/v1/exec` is refused exactly like the page, and sending a confirmation with the request does not get it through. The test is an allowlist of reads, not a list of writes to avoid: `EVAL` runs any script, `BITFIELD` writes under a name that sounds like a read, `GETDEL` and `GETEX` are writes spelled like gets, and no module command is in the core Redis command table — so anything unrecognised is refused, and a read the allowlist has missed shows up as a panel saying it is unavailable.

Changing an account's password, role or server rules, or disabling it, ends the logins it already has, so a demotion reaches the browsers that are already signed in. **Sign Out**, the last item of the title-bar menu, ends one login immediately — on the bridge, not only in that browser. Left alone, a login lapses after 30 days without use, or 8 hours when *Keep me signed in on this device* was unticked; restarting the bridge signs nobody out.

This is defence in depth and not a substitute for Redis's own ACL: a Redis ACL user with `-@write` is enforced by the server on every connection, whatever talks to it, while this is enforced by the bridge, which is the only thing the browser can reach. Use both — the ACL is the guarantee, the account is what greys the buttons out before the round trip.

### Upgrading from configured accounts

Before registration, accounts were configured: `ZEDIS_BRIDGE_USERS` (`alice@secret,bob:ro@hunter2`) or a TOML file of `[[users]]` tables (`--users-file`, `ZEDIS_BRIDGE_USERS_FILE`). A bridge that still sets either imports those accounts once, into an empty account store, at its first start after the upgrade: the passwords are hashed, and the read-only role, the `servers` rules and the MCP tokens come along — so what people sign in with keeps working. None of them becomes an administrator, which is a choice the configuration never made, so the bridge prints a setup token as well: open the page, choose *Create the administrator*, and give the token with an existing account's own name and password to make it the administrator. Each import is an `account_imported` audit line. From then on the setting is ignored — a warning at every start says so — and can be removed.

## Signing in through your own SSO

A company that already has single sign-on puts an authenticating reverse proxy — oauth2-proxy, Authelia, Pomerium, Cloudflare Access, Tailscale — in front of its applications: the proxy signs the person in and writes who they are into a request header. The bridge can be told to trust that header:

```bash
-e ZEDIS_BRIDGE_TRUSTED_HEADER=Remote-User          # --trusted-header
-e ZEDIS_BRIDGE_TRUSTED_PROXY=10.0.0.0/8,172.17.0.5  # --trusted-proxy: the proxy's addresses
```

Both or neither: anyone can write `Remote-User: alice` into a request, so the header counts only on a connection *from the proxy* — by the socket's peer address, never by `X-Forwarded-For`. The name in the header must be an account on the bridge; a name that is no account is refused with a `403` (and an audit line), not signed in as somebody new. Roles stay the bridge's, because the proxy says who someone is and not what they may do.

Such an account is registered with an invitation that has **Signs in through the proxy** ticked (offered while the two settings are on). Opened through the proxy, it registers the name the proxy writes in the header — exactly that, `alice@example.com` if that is what it sends — with the invitation's role and no password, so the account cannot sign in around the proxy. The administrator made with the setup token has a password; an invitation with the administrator role and the proxy box ticked makes another one that signs in through the proxy.

Two things the proxy must do: strip or overwrite that header on every request it forwards, and be the only way to reach the bridge — a bridge that is also reachable directly is one where the address check protects nothing. The header name is the only thing that differs between proxies: Authelia sends `Remote-User`, oauth2-proxy `X-Auth-Request-User` (with `--set-xauthrequest`), Cloudflare Access `Cf-Access-Authenticated-User-Email`, Tailscale `Tailscale-User-Login`. Without these two settings the header is never read.

## Locked writes on production

A **Prod**-tagged entry starts with its writes locked, on the desktop and in the browser alike (any entry can start that way, through the *Writes* choice on its Safety tab — follow the tag, allowed, locked, read-only). The lock button in the status bar asks for the server's name and then unlocks writes for a **15-minute window** — the button shows the time left, and the entry locks again by itself. In the browser the bridge keeps the same window per account (`POST` / `DELETE /v1/servers/{id}/unlock`, audited as `unlocked` / `locked`) and refuses a write outside it with the same `428` a destructive command gets, so a script is held to the same rule as the page. A Lua script or function call (`EVAL` / `EVALSHA` / `FCALL`, not the `_RO` forms) sent to such an entry from the browser has to be confirmed every time, inside the window too and by typing the server's name on Prod, because what a script writes cannot be seen from the command that runs it. After an upgrade an existing Prod entry starts locked too; set its *Writes* to *Allowed* if that is not wanted.

## Audit log

`--audit-log /data/audit.log` (`ZEDIS_BRIDGE_AUDIT_LOG`) appends one JSON line per event: every login, failed login and logout, every refusal of a read-only account, every change to the accounts — the administrator made with the setup token, an invitation created or revoked, a request for an account made, approved or rejected, an account registered, changed (role, server rules, disabled, locked: from what to what), deleted (and where what it kept to itself went) or imported, what a deleted account left behind settled, a password changed or reset, an MCP token issued or revoked; never a password, a code or a token itself — every saved Lua script saved or deleted, every server entry added, edited (which settings changed, from what and to what; which secrets changed, never their values; a private entry made shared) or deleted, every command that administers the server — `CONFIG SET`, `ACL SETUSER`, `REPLICAOF`, `MODULE LOAD`, `CLIENT KILL`, `FLUSHDB` and the like — every command someone had to confirm, so an entry with *Confirm Writes* switched on logs each write typed into its terminal, and every key taken out of the recycle bin (restored, or let go; the `RESTORE` of a restore is a write line of its own). `--audit-writes` (`ZEDIS_BRIDGE_AUDIT_WRITES=1`) adds plain data writes, and with them each deleted key the bin keeps; reads are never logged. Passwords in arguments are blanked, long values cut, the same command repeated in one batch is one line with a count, and the file is created owner-only.

```json
{"ts":"2026-09-25T08:12:03.417Z","account":"alice","peer":"10.0.0.7:51234","event":"command","server":{"id":"0199…","name":"prod"},"db":0,"command":"CONFIG","args":["SET","maxmemory","2gb"],"outcome":"confirmed","kind":"config_set","confirm":"type_name"}
```

It is the log of the bridge only: what reaches Redis from `redis-cli` or an application is not in it, and it cannot tell you who changed a key — it can tell you whether anyone did so by hand. The bridge appends and never reopens the file, so rotate it with `copytruncate`.

With a [database](#several-bridges-on-one-database) the audit is **on by default** and kept there — every bridge's lines in one table — for 365 days (`--audit-retention-days`, `ZEDIS_BRIDGE_AUDIT_RETENTION_DAYS`; `0` keeps them for ever). `--audit-log /path` writes a file instead (one per bridge), `--audit-log off` turns it off, and `--audit-writes` adds plain data writes as before. A line that cannot be written is an error in the bridge's log; the request it was about goes through.

**Reading it.** An administrator reads the audit on the account page (**Account…** → *Audit*): newest first, narrowed by account and event, a page at a time, the line picked shown whole. A script asks `GET /v1/admin/audit` with any of `account`, `event`, `server` (an entry's id), `since_ms` / `until_ms` (unix milliseconds) and `limit`, and continues with `before` set to the previous page's `next`. From a file, each page reads at most the last 16 MB back from where the previous one stopped, and says so (`"stopped_early": true`); a database is searched whole.

### Pushed to a chat

`--audit-webhook <url>` (`ZEDIS_BRIDGE_AUDIT_WEBHOOK`) sends the lines that need a person's attention to a group chat as they happen, with or without `--audit-log`. Give it a bot's webhook address and its host picks the message — Feishu / Lark (`open.feishu.cn`, `open.larksuite.com`), DingTalk (`oapi.dingtalk.com`), WeCom (`qyapi.weixin.qq.com`), Slack (`hooks.slack.com`) — and any other URL is sent the lines themselves as JSON, `{"source":"zedis-bridge","events":[…]}`, for a receiver of your own. Behind a relay whose host says nothing, name the format: `--audit-webhook-format json|feishu|dingtalk|wecom|slack` (`ZEDIS_BRIDGE_AUDIT_WEBHOOK_FORMAT`). A Feishu or DingTalk bot with signature verification switched on needs its secret in `ZEDIS_BRIDGE_AUDIT_WEBHOOK_SECRET` (there is a `--audit-webhook-secret` flag too, but a flag is visible to every user of the machine in the process list).

```bash
docker run -d --name zedis-web -p 127.0.0.1:7379:7379 \
  -e ZEDIS_BRIDGE_AUDIT_LOG=/data/audit.log \
  -e ZEDIS_BRIDGE_AUDIT_WEBHOOK="https://open.feishu.cn/open-apis/bot/v2/hook/…" \
  -e ZEDIS_BRIDGE_AUDIT_WEBHOOK_SECRET="…" \
  -v zedis-data:/data \
  vicanso/zedis-web:latest
```

The chat gets one sentence per line:

```text
[zedis-bridge audit, times in UTC]
08:12:03 alice ran, confirmed, CONFIG SET maxmemory 2gb on prod db0 from 10.0.0.7
08:13:00 mallory failed to sign in from 203.0.113.9
```

What is pushed is a subset of what the log records. By default it is every event except sign-ins, sign-outs and MCP tool calls, which happen all day; `--audit-webhook-events` (`ZEDIS_BRIDGE_AUDIT_WEBHOOK_EVENTS`) takes a list instead — `login`, `login_failed`, `no_account`, `logout`, `refused`, `command`, `server_added`, `server_updated`, `server_deleted`, `unlocked`, `locked`, `trash_put`, `trash_removed`, `setup_completed`, `invite_created`, `invite_revoked`, `account_registered`, `account_updated`, `account_deleted`, `orphans_settled`, `password_changed`, `token_issued`, `token_revoked`, `account_imported`, `script_saved`, `script_deleted`, `tool` — or `all`. Of the commands, only those that administer the server and those someone had to confirm or was refused are pushed: a plain data write never is, even with `--audit-writes`, or a busy morning would bury the chat — and neither is a deleted key going into the recycle bin, for the same reason.

Sending never holds up a request: lines are queued and go out three seconds after the first of them, up to twenty in one message, which also keeps under a bot's rate limit (WeCom allows twenty messages a minute). A send that fails is tried once more five seconds later and is then a warning in the process log; a bot that answers with an error code (a wrong signature, a disabled bot) counts as a failure. The URL carries the bot's key, so the process log names only its host — at startup the `audit webhook on` line gives the host, the format, the events and whether it signs. A bad URL, format or event name stops the bridge at startup instead, and so does a secret for a bot that is not signed (WeCom, Slack, JSON), rather than leaving you to believe its messages are.

## Who is online

The bridge counts who is using it. A client is online from its first request until it has made none for five minutes: each browser sign-in is one client (every tab of that browser shares its cookie), a script signing in with HTTP Basic is one per account and address, an MCP token is one per name, and an account behind the trusted proxy is one per account. A page with a server open sends a heartbeat every 10 seconds (60 in a background tab), so it stays online while it is open; a page left on the home screen sends nothing and goes offline after the five minutes. Signing out takes that browser off at once.

The process log says when one comes and goes, with the count:

```text
INFO zedis_bridge::api: client online account="alice" kind="browser" online=3
INFO zedis_bridge: client offline account="ops" kind="basic" online=2
```

`GET /v1/clients` lists who is online now, the most recently seen first, with the servers each client used in the same five minutes — the one a page has open, a terminal's, a subscription's, a tool call's — latest first. It names other accounts, where they call from and which servers they are on, so it is an administrator's to read, like the account list — anyone else is refused (`403`, and an audit line when the audit log is on); and it names only the servers the asker may see, so someone else's private entry is never confirmed to exist by who is on it:

```console
$ curl -s -u alice:secret https://zedis.example.com/v1/clients
{"process":"4c1f…","online":2,"window_secs":300,"clients":[
  {"account":"alice","kind":"browser","peer":"10.0.0.7","last_seen":"2026-10-17T04:21:57.000Z","seen_at":1792210917,"idle_secs":4,
   "servers":[{"id":"3f2a…","name":"prod"}]},
  {"account":"ai","kind":"token","token":"claude-laptop","peer":"10.0.0.9","last_seen":"2026-10-17T04:20:41.000Z","seen_at":1792210841,"idle_secs":80,
   "servers":[]}]}
```

In the page the same list is **Account… → Online**, an administrator's section: who, from where, on which servers, last seen — so an administrator about to restart or flush a shared server can see who else is on it first.

`peer` is the socket peer, which behind a reverse proxy is the proxy; `forwarded_for` is added when the proxy wrote `X-Forwarded-For`. The count is kept in memory: after a restart each client is back with its next request. With [several bridges](#several-bridges-on-one-database) each counts its own clients, and `process` says which bridge answered.

## Read-only access for AI assistants (MCP)

`POST /v1/mcp` is a [Model Context Protocol](https://modelcontextprotocol.io) server, so Claude Code, Cursor or any other MCP client can read your Redis through the bridge — and only read. The assistant signs in as an account that **must be read-only** — with a token of that account's (below), or like a script with HTTP Basic; a full account is refused there whatever it asks. Its tools are shaped for a model rather than a terminal: `list_servers`, `scan_keys` (paged, across every master of a cluster), `inspect_key` (type, TTL, memory, encoding, length and a short preview), `server_info` and `slowlog` (parsed, per master), four that summarise rather than list — `slowlog_summary` (the slow log grouped by command, heaviest first, with the slowest entries quoted), `hotkeys` (the report of a `HOTKEYS` collection already running, Redis 8.6+; starting one changes the server, so it is left to a person), `memory_summary` (a sample of up to 5,000 keys measured with `MEMORY USAGE` and grouped by key prefix, with the largest keys) and `troubleshooting_report` (a single-shot diagnostic snapshot combining summary, per-node `INFO`, `MEMORY DOCTOR`, `LATENCY DOCTOR`, `LATENCY LATEST`, redacted slow log and core server settings) — and `read_command` for any other read-only command. Every command a tool sends goes through the same read-only allowlist as the page, plus a refusal of the commands that would change the state of the shared connection (`SELECT`, `AUTH`, `CLIENT SETNAME`, `SUBSCRIBE`, …); writes, scripts other than the `_RO` forms, administration and reads that return credentials (`CONFIG GET` of a password, `ACL LIST`) are refused with a reason the model can read. Large values are cut to a size that fits a model's context, an account may make 120 calls a minute, and **every call is one line of the audit log** — reads included, because the caller is a program acting for someone.

Give the assistant a **token** rather than the account's password. Invite a read-only account for it, sign in as that account and open **Account…**: under *MCP tokens*, name the token after the machine or the client (`claude-laptop`), give it an expiry if it should have one, and issue it. An administrator can do the same for any read-only account from the account table (*Issue a token…*); that token expires in 90 days unless another day is picked, or the day is cleared for none. A day already gone cannot be picked. The token is shown once; the bridge keeps only its SHA-256.

The client sends it as a header:

```sh
claude mcp add --transport http zedis https://bridge.example.com/v1/mcp \
  --header "Authorization: Bearer ${ZEDIS_MCP_TOKEN}"
```

A token signs in at `/v1/mcp` and nowhere else — not the page, not the API — so what an MCP client keeps in plain text in its config file is not a password that opens the page. One machine is cut off by revoking its token on the same page, which takes effect at its next call; an expiry refuses it from the first second of that day, UTC; and every audit line of a call says which token made it (`"auth":"token","token":"claude-laptop"`). Tokens belong to read-only accounts only: a full account cannot be issued one, and a read-only account that has tokens cannot be given another role until they are revoked. HTTP Basic still works at `/v1/mcp`, for a client set up before tokens:

```sh
claude mcp add --transport http zedis https://bridge.example.com/v1/mcp \
  --header "Authorization: Basic $(printf 'ai:secret' | base64)"
```

The assistant goes through the same entry point as the page, not a separate one: the account's server rules say which entries the assistant sees, the audit log says what it read, and nothing passes through a third party on the way.

## Streaming panels

Live Monitor, Pub/Sub and keyspace notifications need a connection the server pushes on, which a page cannot hold. The bridge holds it (`/v1/watch`) and the page asks for what arrived with a request that waits up to 20 seconds — so a reverse proxy in front of the bridge must allow a request to be idle for longer than that (nginx's default 60 s is fine). Each open panel keeps one such request in flight; over plain HTTP/1.1 a browser allows six per host, which is another reason to serve the bridge over HTTPS (HTTP/2).

- **Pub/Sub and keyspace notifications** are observation: a read-only account may use them, on any entry it can see.
- **MONITOR** shows every client's commands, values included, and slows the server while it runs. It is for full accounts only; on an entry whose writes are locked (Prod by default) it asks the lock's question first — the server's name on Prod — without opening the write window; every start is a line of the audit log (and of the audit webhook); and the bridge stops it after ten minutes.
- A panel that is closed, or a tab that goes away, releases its connection: at once when the page can say so, otherwise within a minute or two.
- The *Enable* buttons of the keyspace panel send `CONFIG SET`, which the bridge asks about every time, so the page asks first.

## Recycle bin and Metrics history

Both are kept by the bridge, under its data directory (`/data` in the image), because a page keeps nothing past a reload:

- `bridge-trash/` — the recycle bin: a deleted key's value for 24 hours (values up to 1 MB; at most 1,000 keys / 128 MB per server, the oldest making room), encrypted with the bridge's `master.key` like the server passwords. A bin belongs to the server: everyone who can see the entry sees what was deleted from it and by whom, and restoring needs write access to it. Turn it off per browser in *Settings → Key Behavior*.
- `bridge-metrics/` — one metrics sample a minute per server, for a week, sent by whichever page has the server open. Like the desktop's, the history covers the times someone had it open; the bridge never polls a server by itself.

Deleting a server entry deletes both. The files are owner-only, like the rest of the bridge's.

## Your data on the bridge

What the desktop keeps in its local database the bridge keeps for the page, loaded before the application starts and saved as it changes — in the files named below, or in the [database](#several-bridges-on-one-database) when the bridge has one:

- **Each account's own** — favorites, recent keys, search history, Settings and proto viewers (`bridge-page-data/accounts/`). They follow the account to any browser, and nobody else sees them; a read-only account keeps its own too. Deleting the account deletes them.
- **Key tags and notes** (`bridge-page-data/key-notes/`) — a server's, seen by everyone who sees the server, each with who left it and when. They are kept one key at a time, so two people tagging two keys never undo each other. An account that may not write on the server — a read-only one, or one whose rules say `:ro` there — reads them and leaves none. Deleting the server entry deletes them.
- **Saved Lua scripts** (`bridge-scripts.json`) — the saver's own unless the script library's *Shared* box is ticked, which makes one every account's. Like a shared server entry, a shared script may be changed or deleted by any account that may write; a read-only account runs scripts and changes none. Every save and delete is an audit line (`script_saved`, `script_deleted`), because a shared script is run by others, on production too. A script's run counts stay in the page.

Not kept on the bridge: the terminal's command history, which holds what was typed, values included, and is covered by the audit log anyway. The page keeps it in that browser instead (`localStorage`, per account, the last 100 lines a server) with credentials blanked — `AUTH`, `CONFIG SET requirepass` and the like come back from ↑ as `***` — so it survives a reload but not a change of browser. What other accounts change — tags, notes, saved scripts, and this account's own lists from another browser — reaches an open page within a minute, and at once when a server or the script library is opened or the tab is looked at again; a tab nobody is looking at does not ask, and an unchanged answer is a `304`. It never lands over this page's own change on its way to the bridge, and never into a script form that is open, which keeps what is typed in it: the last save wins.

## What the web version leaves out

Everything built on a request and a reply works — the key tree, every value editor, the terminal, metrics with their 1h / 24h / 7d history, slow log, config, clients, memory analysis, value search, multi-database key search (⌘⇧F), cross-server compare (a dialog here, with *Copy to target*), server-to-server key copy, the troubleshooting report, the proto viewers (*Protobuf* in the menu: the schema is pasted or chosen as a `.proto` file and compiled in the page, the well-known `google/protobuf` types importable, no include folders), the recycle bin and the Lua script library — and so do the streaming panels: Live Monitor, Pub/Sub and keyspace notifications (see *Streaming panels* below). Every export is a download — CSV and JSON from the panels, a value, the terminal's output, a connection export, the troubleshooting report, and keys with their values (*Export Keys…* in Tools, or a key, a folder or a selection in the tree: JSON or CSV, the file the desktop's readable export writes, built in the tab and so at most 64 MB, or copied straight to another server) — and a file is picked through the browser to import a value, import keys (*Import Keys…* in Tools: a JSON or CSV export up to 64 MB, with conflict preview and skip/overwrite/abort), load a connection export into the import box, or analyze an RDB dump offline (up to 256 MB, read into the tab). A value written from the page is at most about 1.5 MB, the bridge's limit for one request (2 MB of JSON, the value base64 inside it): a larger string or JSON value opens read-only there and says why, and a value import stops at the same size. *Diagnose Connection* (a connection's menu, or the panel shown when a server cannot be reached) runs on the bridge — the DNS, TCP, SSH, TLS, AUTH and PING stages from where the bridge stands, which is what dials the server for a page (`POST /v1/servers/{id}/diagnose`); it diagnoses a saved entry, so the connection form has no Diagnose button here. Not available in the browser: Topology and Sentinel administration, the script viewers (they run a program), binary `.zedis-dump` key export and import, and the shortcuts a browser keeps for itself (⌘N / ⌘T / ⌘W / ⌘Q). Also unavailable is whatever needs a path or a second window: the About window, and the installed app's own housekeeping: updates, the logs folder, the diagnostics bundle. Settings opens as a dialog over the page, with the sections that apply to a page — appearance, date and time, key behaviour, workspace tabs — and without the ones about an installed program (connection timeouts, which in the browser are the bridge's to set; the AI assistant; the tray, updater and proxy; the backup file). What is set there is kept in that browser. The value editors colour JSON and Lua there as on the desktop, with a lighter highlighter (syntect — the desktop's tree-sitter cannot be built for WebAssembly): blocks fold by their braces, and a value over 512 kB is shown plain, because the page parses the whole of it again after each pause in typing, on its one thread. Tags, notes, favorites, recent keys, search history, saved scripts, proto viewers and Settings are kept by the bridge (see *Your data on the bridge*); the terminal's command history is kept in the browser. A page left in a background browser tab slows its polling down by itself. What is left out is not offered: its menu items, buttons and shortcuts are absent in the browser, rather than there and silent. The desktop app remains the complete client.

Without Docker, `make web-dist` builds the same thing as a single self-contained binary.
