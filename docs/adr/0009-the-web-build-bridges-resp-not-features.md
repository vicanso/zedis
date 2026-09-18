# 9. The web build bridges RESP, not features

Date: 2026-09-17 · Status: accepted

## Context

gpui-kit 0.6.1 ships a real browser backend (`gpui-pre-web`: WebGPU with a WebGL2
fallback, a `fetch`-based `HttpClient`, events, IME, clipboard). A web build of
Zedis is therefore no longer blocked by the UI framework. A browser cannot open a
TCP socket, so the Redis traffic has to cross an HTTP bridge to a process that can.

Two questions decide the shape of that bridge, and they have very different answers.

**Where does the bridge cut?** Three layers were measured.

| Layer | Coverage | Why not |
|---|---|---|
| `ZedisServerState::spawn` (`states/server.rs:574`) | ~87 operations | Private `fn`. 35 view files, 90 `get_connection_manager()` calls and 65 hand-written `cmd(...)` sites bypass it |
| `ConnectionManager` (`manager/pool.rs:260`) | ~95% | Four escape hatches hand out raw connection types |
| **`RedisAsyncConn` (`async_connection.rs:397`)** | **all request/response traffic** | Three methods to implement |

`RedisAsyncConn` is already a three-variant enum implementing redis-rs's
`ConnectionLike`, and `RedisClient` holds one (`manager.rs:582`), so
`get_client(...)` and `get_connection(...)` both land on it.

**How much surface does the bridge protocol own?** A method-level RPC would need
roughly 210 endpoints (52 `RedisClient` methods, 63 free functions taking
`&mut conn`, 95 distinct commands the app builds inline) and `Serialize` on
about 118 public types, of which 3 have it today. It would also grow with every
new feature: `ServerTask` alone has 69 variants.

A RESP-level bridge owns one endpoint and never grows. `Cmd::get_packed_command()`
and `redis::parse_redis_value()` are both public in redis-rs, so the browser can
encode the request and decode the reply with the same parser it already links,
and the bridge server never learns a single Redis command.

**What actually costs work** is not the bridge. It is that nothing in this repo has
ever been compiled for wasm: zero `target_family` / `wasm` cfgs, zero `[features]`
sections across all six manifests, and 78 `cfg(target_os)` guards written as OS
allow/deny lists that misfire on wasm (`tray-icon` is gated
`not(target_os = "linux")`, so a wasm build pulls it in).

## Decision

### 1. The bridge is one RESP passthrough endpoint

```
GET    /v1/health                     -> { status }
POST   /v1/login   { username, password } -> Set-Cookie (HttpOnly, SameSite=Strict)
POST   /v1/logout                     -> clears it
GET    /v1/servers                    -> [{ id, name, host, …, secrets_set[] }]   # settings, never secrets (1b)
POST   /v1/servers { url | server }   -> { id, name, version, … }                 # saved only if it dials
PUT    /v1/servers/{id} { server, keep_secrets[] }   -> the same                  # an edit, dialled before it is kept
DELETE /v1/servers/{id}
POST   /v1/session { server, db }     -> { session }
POST   /v1/exec    { server, db, commands[], pipeline?, session?, confirm? }
                                      -> { replies[] }
DELETE /v1/session/{token}
```

`commands` and `replies` carry base64 RESP in both directions: one command for a
plain call, several plus `pipeline` for a batch. The generic `T: FromRedisValue`
decoding stays client-side and never enters the protocol.

`fanout: "masters"` runs the commands on every master and answers with a `nodes`
list of `host:port` labels beside the replies. **The caller never names a node.**
An earlier draft of this ADR proposed a `node` address field instead, and that was
wrong twice over: it would hand the browser dialable addresses, contradicting the
rule two paragraphs below that only a server id crosses the wire, and the callers
of `query_async_masters` take `Vec<RedisServer>` back — entries that carry
credentials. Reaching each master is the same category of knowledge as reaching
the server at all (SSH, TLS, Sentinel discovery), so it belongs on the same side
of the wire. `fanout` is a transport concept like `session`, not a per-feature
one, so the protocol still does not grow when Zedis gains a Redis feature.

In the web build both fan-out methods branch on a bridge connection.
`query_async_masters` asks for the plain fan-out and rebuilds display-only
`RedisServer`s from the labels — host and port, which is all its two callers
read, and no secrets.

`query_async_masters_with_option` sends `fanout_nodes`, one `host:port` label per
command, and re-aligns the replies by label. **Not by position**, and that is a
correctness requirement rather than tidiness: its two callers are strictly
positional — `cluster_slot_stats` labels reply *i* with node *i*, and `scan`
carries one SCAN cursor per node — while the browser and the bridge discover the
topology separately. A failover between the two discoveries leaves the lists the
same length in a different order, and an index would then resume a scan from
another node's cursor and silently return the wrong keys. With labels, a master
that moved is simply absent from the reply set instead.

`session` exists because the terminal owns a stateful connection (ADR 4): `SELECT`,
`AUTH`, `CLIENT SETNAME` and `MULTI`/`EXEC` are connection state, so a stateless
bridge would scatter a transaction across backend connections. A request without
`session` uses a pooled connection; one with it gets an affine connection. Sessions
expire on idle so a closed browser tab cannot leak a backend connection.

### 1a. Where the command is built, and why that is not where Redis lives

The two builds are meant to read as one pipeline with one substitution in it:

```
GUI      window ──────────────▶ redis cmd ─▶ connection manager ─▶ connection ─▶ Redis
browser  page ─▶ HTTP/JSON ─▶ [ redis cmd ]─▶ connection manager ─▶ connection ─▶ Redis
                                    ▲
                            the only thing that moved
```

The obvious drawing puts *everything* Redis-shaped behind the HTTP server, and
that is nearly what happens — but not quite, and the exception is deliberate.
The bridge does own the connection manager, the pool, dialling, TLS, SSH,
Sentinel and cluster discovery, the credentials and the server list; `/v1/exec`
goes through `get_connection_manager()`, not around it, so both rows above run
the *same* manager code. What stays on the browser side is the two pure ends of
the pipeline: packing a `Cmd` into bytes, and reading a `Value` back out.

They stay there because moving them is what would cost the 210 endpoints. A
command built on the far side has to be *described* on the near side, and a
description of every Zedis feature is a protocol that grows with every feature —
the thing the rest of this ADR exists to avoid. Leaving `Cmd` in the browser
costs nothing at runtime (it is a byte buffer) and buys the property that 201
`query_async` call sites, 63 free functions and 155 hand-written `cmd(...)`
sites compile for both targets untouched.

So the rule is not "no Redis code in the browser". It is **nothing that reaches
a server, holds a credential, or knows an address may be in the browser** — and
by that rule the browser holds none of it. (The last clause was given up on
2026-09-18 for the server list alone; see 1b. Nothing in the browser *dials*
an address, which is what the clause was protecting.)

### 1b. The server list carries its settings, never its secrets

Amended 2026-09-18. The first listing answered `{ id, name }` and nothing
else, on the rule above. What that cost only showed once the real form ran
against it: editing an entry opened a form with a name and every other field
blank, and saving it changed nothing — the store sent new entries and
deletions, and an edit was silently dropped because it could not be sent
faithfully. A dialog that closes as if it had saved is worse than no dialog.

The two honest options were to take editing out of the web build or to let
the settings through, and the settings went through. Whoever can log in can
already add a server, read every key and delete the entry; its address is not
a credential, and hiding it bought nothing that survived that. So
`GET /v1/servers` now answers each entry whole **minus its secrets** —
`RedisServer::SECRET_FIELDS`, the one list that encryption at rest, the
diagnostics redaction and this share — plus `secrets_set`, the names of the
secrets that hold a value. A password, a key or a passphrase still never
leaves the bridge. With accounts that have no roles, this does mean every
account sees every address.

An edit is `PUT /v1/servers/{id} { server, keep_secrets }`. The caller never
saw the secrets, so "the password field was not touched" cannot be said by
sending it back; it is said by naming the field in `keep_secrets`. A secret
that is neither named nor given a value is cleared — that is how a password
is removed — and one that is given a value is replaced. The bridge dials the
edited entry before it keeps it, exactly as it does a new one, and puts the
stored entry back if the dial fails: an edit must not be able to break a
working entry in a list everyone shares. Verified against the password-
protected test cluster: a rename that keeps the password dials and is kept;
the same edit without the password comes back `502 NOAUTH`, and one to a dead
port `502 Connection refused`, and after both the stored entry is unchanged
and still answers `PING`.

In the browser a stored secret is shown as eight bullets (`STORED_SECRET` in
`zedis-web`'s transport). Leaving it, emptying it and typing over it are the
three things an edit can mean, so the desktop's form needed no change and no
new string in eight locales; the placeholder is turned back into a name on
the way out and never crosses the wire. The store sends only entries that
differ from the copy the bridge last answered, so opening and saving a form
untouched is no request at all.

### 2. `RedisAsyncConn` gains an `Http` variant

Three methods: `req_packed_command`, `req_packed_commands`, `get_db`.

The transport behind them is a trait, not a concrete client: `zedis-connection`
must not depend on gpui (it has no gpui today, by design), so the variant holds an
`Arc<dyn BridgeTransport>` declared in that crate and implemented outside it. The
app supplies GPUI's `HttpClient`, which is the native client on desktop and
`FetchHttpClient` in the browser, so the same variant serves both targets and can
be exercised natively against a local bridge.

Redis is built with **both** runtime adapters, and picks per connection:
`Runtime::locate()` takes tokio when `Handle::try_current()` finds an ambient
runtime and smol otherwise (`redis-1.7.0/src/aio/runtime.rs`). The GUI's Redis
calls run on gpui's own thread pool, which has no tokio context, so they keep
using smol unchanged; the bridge's axum handlers have one, so they use tokio.
This holds even when a `--workspace` build unifies the features.

The cost is close to nothing: `tokio-comp` asks only for `tokio/net`, `tokio/rt`
and `tokio/time`, which the SSH stack already enables, so the added code is
redis's 185-line tokio adapter and the lock file grew by a single dependency
edge. The feature to enable is `tokio-rustls-comp`, not `tokio-comp`: a rustls
feature is globally on, so the tokio runtime must also implement
`connect_tcp_tls` or the crate does not build. `tokio-rustls` is likewise already
present for SSH.

`standalone_connects_from_inside_a_tokio_runtime` in the live suite pins the
tokio half; every other test there runs outside a runtime and covers the smol
half.

`ConnectionLike` returns `BoxFuture`, which requires `Send`, while the browser's
`fetch` and `JsValue` are `!Send`. The returned future therefore holds only a
oneshot receiver while the fetch runs under `spawn_local`. This is the shape
gpui-kit's own `FetchHttpClient::send` uses.

### 3. The bridge server is its own binary

A workspace member `zedis-bridge`, depending on `zedis-connection` only. Not a
subcommand of the GUI: that binary links X11/Wayland on Linux (CI needs Xvfb to
start it), plus wgpu, fonts, tree-sitter and the whole asset bundle, none of which
a process that forwards RESP bytes should carry onto a server.

Nothing is lost by splitting, because SSH tunnels, TLS, Sentinel discovery,
cluster topology, the server list and its credential encryption all live in
`zedis-connection`; the app crate adds only the GUI. Credentials stay in the
bridge process and the browser only ever names a server id (and, since 1b,
reads an entry's settings — never its secrets).

The server side runs on tokio with axum. Owning an HTTP server that is exposed to
other people means wanting TLS termination, body limits, timeouts, connection
caps and graceful shutdown, which is not something to hand-roll. The dependency
weight is acceptable precisely because it lands in a separate binary that the GUI
build never compiles.

The bridge serves the web build itself — compiled into the binary since
2026-09-18 (`static_files::WebBuild`, rust-embed), or a directory named by
`--static <dir>` instead — which is what makes
the page same-origin with the API: no CORS to configure, and the login cookie can
stay `SameSite=Strict` — a cookie that had to travel cross-site could not be, and
the protection it gives against another site driving the API would be gone. Two
details are load-bearing rather than incidental: `.wasm` must be served as
`application/wasm` or `WebAssembly.instantiateStreaming` refuses the module, and a
request path is *refused* rather than sanitised when it could climb out of the
root, with a canonical check behind it so a symlink inside the root cannot point
outside. The assets are unauthenticated, because the page has to load before
anyone can log in; everything it then asks for is behind the cookie.

Embedding makes a deployment one file, and three choices sit inside it. The
embed is raw, not the desktop's compressed one: rust-embed's compressing
variant inflates a file on every `get`, which for a 20 MB module fetched on
every page load is a copy and a decode per request, while a raw embed is
served from the binary's own read-only pages with no copy at all — the
binary grows by the bundle's size, and the tarball's gzip takes that back.
(In a whole-workspace build cargo unifies the desktop's `compression`
feature onto this derive; only the per-request cost changes, and `make
web-dist` builds the bridge alone.) A debug bridge reads `www/` from disk,
so `make web-serve` picks up a fresh `make web-bundle` with no rebuild. And
`build.rs` refuses a *release* bridge whose `www/wasm` is missing, because
rust-embed would otherwise embed the directory without its module and ship
a page that cannot start, silently.

**Superseded 2026-09-18 — the bearer token is gone; see "Accounts own
entries" below. The next two paragraphs are kept as the record of what it was.**

A browser never holds the bearer token. It posts it once to `/v1/login` and gets
a cookie carrying a server-side login id: `HttpOnly`, so no script on the page can
read it, `SameSite=Strict`, so it does not ride along with a cross-site request,
and revocable and idle-expiring, which a copy of the long-lived token in
`localStorage` would be neither. Scripts and the CLI keep using the bearer header,
and both credentials open the same routes.

`ZEDIS_BRIDGE_USERS="alice@secret,bob@hunter2"` (2026-09-18) switches the
bridge to named accounts: the page asks for a username and password, scripts
send HTTP Basic, logins and logouts are logged under the name, and the token
file is neither read nor created. `auth::Credentials` is the one type behind
both modes; `/v1/health` reports which one runs so the page shows the right
form, which gives nothing away that the login route's own shape does not.
A set-but-malformed variable — or an empty one — stops the bridge with the
entry named by position (never by content, because the content is a
password), rather than falling through to the token. The accounts carry no
roles: every one of them has the token's full access, and what the mode
buys is a name in the log and access that can be handed out and taken back
one person at a time. What it costs is that a password is guessable where a
128-bit token was not; the bridge does not rate-limit `/v1/login` itself,
and a deployment reachable beyond its own network wants that in front of it.

#### Accounts own entries, and the token is gone

Decided 2026-09-18, the three answers being the user's: entries are private
*and* shared; an entry with no owner is shared; token mode is removed.

The last follows from the first. A private entry needs an owner, and a caller
who is "whoever holds the token" cannot be one — so rather than keep a mode in
which the feature silently does not apply, there is one way in. The bridge
does not start without `ZEDIS_BRIDGE_USERS`; it used to generate a credential
nobody chose, and now says what to set. `Authorization: Bearer` opens
nothing, `bridge-token` is neither read nor written, `/v1/health` no longer
reports a mode, and the page has one form. `authorize()` answers *who*, not
*whether*, because every route below it needs the name.

Ownership is a field on the entry (`RedisServer::owner`), not a side file
mapping ids to names, which was the first plan. An entry and its owner have
to be saved, rolled back after a failed dial, and deleted *together*, and
two files cannot do that atomically where one entry in one file can. The
desktop app has no accounts: it never sets the field and must never drop it,
so its save carries over the owner the stored entry had — otherwise a
bridge's file opened on a desktop would make every private entry everyone's
on the first save.

The rule is one predicate, `visible_to`: an account sees its own entries and
the ones with no owner. Every route that names a server goes through it, and
an entry that exists but is someone else's answers exactly like one that does
not: `404` from exec, session and edit; `204` from delete with nothing
deleted; and a `POST` that arrives carrying someone else's id is given a
fresh one — never an overwrite, and never a refusal that would confirm the
other entry is there. **The caller says private or shared; it never says
whose.** The page writes `RedisServer::OWNER_SELF` for an unticked *Shared*
box and the bridge stores the name of whoever is signed in, so a request
claiming `"owner": "alice"` from bob's session becomes bob's entry. A new
entry is private by default, because an entry carries credentials.

**Shared has to be said; silence is not it.** The first cut read an empty
`owner` as "shared", on the reasoning that the form's ticked box leaves it
empty. It lasted one afternoon: the user imported a server and found it in
the file with no owner — shared with every account, password included. The
form is not the only way an entry is made. An import, a `redis://` link and
a script's bare `{server}` all arrive with no owner because none of them
passed a checkbox, and the safe default has to live where no creation path
can walk around it, which is the bridge and not the form. So the wire has
three states, not two: `OWNER_SELF` (private), `OWNER_SHARED` (shared —
spelled out, and never stored) and nothing, which the bridge settles itself:
a new entry is private, and an edited one keeps what it had, so a reorder or
a tag change neither publishes a private entry nor takes a shared one.

What this does not give: roles. Any account may edit or delete a shared
entry, and may make it private — which takes it from everyone else. That is
the same power as deleting it, which they already had, but it is quieter.
An administrator who alone may curate the shared set would need a second
kind of account, which `ZEDIS_BRIDGE_USERS` has no way to say yet.

Verified with two accounts against a live bridge: each lists its own entries
plus the shared and ownerless ones; bob gets `404` for exec, session and edit
on alice's private entry, his delete is a no-op, and his `POST` with her id
and `"owner":"alice"` lands as a new entry of his; an ownerless entry from a
hand-written file is usable by both until one of them takes it. In the page,
the form's *Shared* box is unticked for a new entry and ticked when editing a
shared one, and an entry added through it is its adder's alone.

The cookie is `Secure` unless `--insecure-cookie` is passed for a plain-http local
run. That default is deliberate: a deployment that forgets TLS then sees a login
that visibly does not stick, instead of a credential travelling in the clear.

This is not optional hardening. `master_key.rs` falls back to a hard-coded
`LEGACY_MASTER_KEY` when neither the keychain nor the key file is reachable, which
is exactly the state a wasm build is in. A browser build that read
`redis-servers.toml` would be decrypting credentials with a published key.

The bridge itself never reads the OS keychain. `disable_keychain()` runs at
start-up, before anything opens the server list, and pins the master key to the
`master.key` file in the config directory — the store Linux always uses. A
service has no session to answer a keychain prompt in, and on macOS an unsigned
or freshly rebuilt binary is asked for the login password on every start, which
is how every `make web-serve` used to begin. The cost is one to know about: a
`redis-servers.toml` the desktop app wrote on macOS or Windows is encrypted under
the keychain key, and those secrets do not open under the file key. A config
directory shared with the desktop app therefore needs its entries re-saved
through the bridge; a list the bridge wrote itself has no such seam.

### 4. The split is by crate and by target, not by feature

The original plan here was a `[features]` skeleton — `native` / `web` /
`bridge-client` / `bridge-server` — and it was dropped once the crates were in
place, because the same separation fell out of the layout for free and does it
better.

The two builds are two *binaries* from two entry points: `zedis` (the GUI) and
`zedis-bridge` (the server), with `zedis-web` compiling the library half to
`wasm32-unknown-unknown`. What is native and what is portable is then decided by
`cfg(target_family = "wasm")` — never by an OS allow-list — and by which
dependencies sit under `[target.'cfg(not(target_family = "wasm"))'.dependencies]`.

There are still **no `[features]` sections anywhere in the workspace**, and that
is the point: the requirement was that the web work must not disturb the desktop
build, and "the desktop build has no feature flags to get wrong" is a stronger
guarantee than "the default feature set reproduces it". A cfg cannot be enabled
by a sibling crate the way a cargo feature can — feature unification is
workspace-wide, and it is exactly how `zedis-connection`'s wasm build silently
depended on gpui-kit turning on someone else's `getrandom` backend until the
crate was checked on its own.

The gate for the browser half is `make check-web`. `make lint` cannot see it:
clippy there is native-only, so a native-only API leaking into shared code
compiles clean locally and fails only when someone builds the page.

### 5. The desktop client is authoritative; the web build is a subset

Availability is already expressed by `ServerView::required_commands()` rendering
`ZedisUnsupportedPanel`. The web build adds a transport-capability axis to that
same mechanism rather than inventing a second one.

Out of scope on the web: SSH tunnels, the four streaming panels (MONITOR, Pub/Sub,
keyspace events, stream live tail), import/export, the diagnostics bundle, the
updater, the tray, and the external script viewer (`scripts.rs` shells out to
`sh -c`). Secondary windows become in-page routes: the browser backend allows one
top-level window and rejects popup, floating and dialog kinds, and there are no
file dialogs.

In scope: connect, browse the key tree, view and edit every value type, single
commands in the terminal, and the snapshot panels (INFO, slow log, config,
clients, topology). Polling panels work unchanged because each tick is already a
request/response, and cursor-driven jobs (memory sampling, value search, prefix
compare) work because their cursors are explicit parameters.

### What works today

A browser can configure a Redis server and connect. `zedis-bridge --static
zedis-web/www` serves a page that signs in for a cookie, posts a
`redis://127.0.0.1:6379` connection string to `/v1/servers`, and gets back what
the server said about itself once the bridge reached it — version, type,
database count. Saving is not separable from connecting: a string the bridge
cannot dial is refused with the driver's own error and rolled back out of the
shared list, so a typo cannot sit there looking healthy.

This is the whole chain end to end (page → cookie → bridge → Redis) on a plain
HTML page. The wasm UI replaces that page later; it does not change the chain.

## Consequences

### What does not change

| Call sites left untouched | Count |
|---|---|
| `query_async` / `exec_async` in `zedis-connection` | 201 across 28 files |
| Free functions taking `&mut conn` (search, ACL, functions, …) | 63 |
| Hand-written `cmd(...)` in `src/` | 155, over 95 distinct commands |
| `ServerTask` variants | 69 |

A new Redis feature builds a `Cmd` and parses a `Value` as it does today. The
bridge protocol does not change.

### The four escape hatches

`RedisAsyncConn` covers every request/response path. These four do not go through it:

- `open_single_connection` used to return a bare `MultiplexedConnection` (6 call
  sites). Closed on 2026-09-17: it returns `RedisAsyncConn` and the concrete dialer
  is `pub(crate) open_multiplexed_connection`.
- `open_monitor_connection` -> `redis::aio::Monitor`, `get_pubsub_connection` ->
  `redis::aio::PubSub`, `get_sharded_pubsub` -> `ShardedPubSub`. All three are
  long-lived push streams; the web build drops the panels that use them.

### Per-crate cost

| Crate | Lines | Work |
|---|---|---|
| `zedis-ui` | 3,462 | None. No fs, net, thread or process use |
| `zedis-core` | 9,948 | One file. Gate `fs.rs`, move `home`/`directories`/`path-absolutize` to target-gated deps, swap `Instant`/`SystemTime` for `web-time` |
| `zedis-connection` | 20,381 | Bigger than "gate eight files", and the shape is now measured rather than guessed. Checked on its own since 2026-09-18: it used to compile only because gpui-kit happened to turn on a `getrandom` backend somewhere else in the graph, which is the feature-unification trap section 4 describes. Done: the dialing dependencies are target-gated, seven native-only modules excluded, and `RedisAsyncConn` extracted to `conn.rs` so the type survives without them. **Compiles for `wasm32-unknown-unknown`.** Split along one line: what builds commands and reads replies is portable, what dials is not. `RedisClient` stays; `RClient`, `ConnectionManager`, the pool, the pubsub modules and slot migration are native, as are `compare`, `probe`, `sentinel`, `script_kill`, `multi_search` and `dump_restore` — each of those is server-side work, so the browser asks the bridge rather than carrying a copy. `config.rs` keeps `RedisServer` and gates its TOML persistence, the keychain crypto and the TLS material. `bridge.rs` now has one shared body per operation with a `ConnectionLike` adapter on the host and `BridgeQuery`/`BridgePipeline` in the browser. The trait imports the probe predicted are in: 28 files carry one `#[cfg(target_family = "wasm")] use`, and no call site changed. The traits take `&mut RedisAsyncConn`, not `&mut BridgeConn` — the enum is what call sites pass, and getting that wrong is what kept 148 errors alive for one round |
| `zedis-db` | 3,734 | **Compiles for `wasm32-unknown-unknown`.** Done on 2026-09-18 by swapping the storage, not the managers: `mem_store.rs` is the slice of redb this crate uses over a `BTreeMap`, and each of the five manager files carries one cfg'd `use` — the same substitution `BridgeQuery` makes for `query_async`. The managers' own code is unchanged, so the 72 desktop tests cover both. `backup`, `protos` and `scripts` are native (a file dialog, `.proto` files off disk, `sh -c`). The rows do not survive a reload, and that is the first step rather than the destination: the deployment shares one server list held by the bridge, so the shared local data belongs there too, and `mem_store` is the seam a bridge-backed store slots into without touching a manager again |
| `zedis-gui` | 93,129 | The remaining work, and now measured rather than feared: `tokio`, `directories`, `home` and `tempfile` have **no call site in `src/` at all** and are manifest entries to target-gate; the real surface is 36 `smol::` uses across 15 files (20 of them `smol::channel`, which `async-channel` already serves on wasm, and 10 `smol::unblock`, which only wraps file work that is native anyway), 38 `std::fs`, 16 `std::process`, 9 `std::thread`, and single-digit counts for `ureq`, `os_info`, `tracing-appender`, `mimalloc`, `tree-sitter` and `rustls`. Roughly a hundred call sites over ~30 of the 146 files. Today the build stops before any of them: `getrandom 0.2` and `errno` fail in the dependency graph, so the manifest is the first step. Now a library with a five-line binary beside it: a `[[bin]]`-only crate cannot be depended on, and the browser build needs the 63k lines of views and states. The split surfaced two lints a binary crate had been hiding — a `pub fn from_str` (renamed `from_name`, the convention in CLAUDE.md) and a public constructor taking a `pub(crate)` type — plus two doc examples that had never been compiled. Still to gate: updater, tray, single_instance, proxy, logger, local_data, diagnostics; and secondary windows and file dialogs need re-routing |

`home` is a hard compile error on wasm (`home_dir_inner` exists only under
`cfg(windows)` and `cfg(unix)`) and it blocks `zedis-core`, therefore everything.
Fix it first.

### Dependency blockers

`smol` reaches `polling`, which has no wasm backend, and it is an unconditional
dependency of both `zedis-gui` and `zedis-connection`; upstream `gpui-base` and
`gpui-component` already target-gate it. `tracing-appender` reaches `symlink`, same
problem.

tree-sitter is the one that needs care, because the obvious reading is wrong.
gpui-component drops the tree-sitter *dependency* on wasm but still compiles the
code behind the *feature*, so enabling `gpui-kit/tree-sitter` anywhere in the
workspace fails a wasm build of any member with 37 errors. The feature is
therefore asked for by `zedis-gui` alone, not by `[workspace.dependencies]` —
which also stops the widget crate paying for 30 parsers it never uses.

**redis-rs's async API cannot be built for `wasm32-unknown-unknown` at all**, and
that is a constraint to design around rather than a gap to fill. `Cmd::query_async`
and `aio::ConnectionLike` sit behind the `aio` feature, which demands `tokio-comp`
or `smol-comp` and stops the build with a `compile_error!` without one; both reach
a socket backend that has no browser port (`mio`, `polling`). redis-rs's own README
offers `wasm32-wasip2` instead, which is a WASI host with real networking, not a
page.

What survives is the half that matters: `Cmd`, `Value`, `parse_redis_value`,
`FromRedisValue` and `get_packed_command` carry no feature gate. The browser can
therefore still build commands and read replies — only the *sending* is missing,
which is the bridge's job anyway. A probe against a real `wasm32-unknown-unknown`
build confirmed that a trait method fills the gap with **no change to any call
site**: inherent methods win over trait methods, so the same
`cmd("SET").arg(k).query_async(conn).await?` resolves to redis's own method on the
host and to `bridge::BridgeQuery` in the browser. `exec_async` and
`Pipeline::query_async` behave the same way. Each file needs one
`#[cfg(target_family = "wasm")]` import and nothing else, which is also what keeps
the desktop build provably untouched: on the host the trait is never in scope.

**The browser build runs on nightly, and that is upstream's own configuration,
not a workaround.** GPUI's web backend enables `gpui-pre-web`'s default
`multithreaded` feature, which reaches `wasm_thread` and its `#![feature(...)]`;
longbridge/gpui-kit pins `channel = "nightly"` in `crates/story-web/rust-toolchain.toml`
for exactly this. Zedis copies the shape: `zedis-web/rust-toolchain.toml` selects
nightly for that directory only, and the rest of the repo stays on the stable
channel pinned at the root.

redis-rs is not a blocker: its `aio` feature needs only tokio's `sync`, and the
crate already carries `cfg(not(target_family = "wasm"))` guards. The web build
takes `aio` without `smol-comp`, `cluster-async` or the TLS features.

tokio is only present for SSH, in an isolated two-worker runtime
(`ssh_tunnel.rs:61`), so dropping SSH drops tokio entirely.

`SystemTime::now()` and `Instant::now()` panic on `wasm32-unknown-unknown`;
`web-time` replaces both.

Unverified: gpui-component depends on `notify = "7.0.0"` unconditionally and that
crate has no wasm backend. This looks like an upstream gap and needs a real
compile to confirm.

### Order of work

1. ~~Return `RedisAsyncConn` from `open_single_connection`.~~ **Done, 2026-09-17.**
   The concrete dialer became `pub(crate) open_multiplexed_connection`; the public
   name kept its signature except for the return type. No call site outside this
   crate changed, because `RedisAsyncConn` is `ConnectionLike` and every caller
   only ran `query_async`. `tail_read` (`states/server/stream.rs`) and one live-test
   helper took the enum instead. Nothing outside `zedis-connection` names
   `MultiplexedConnection` any more, which is the invariant step 2 depends on.
2. The bridge server. **Done, 2026-09-17**: `zedis-bridge` serves `/v1/health`,
   `/v1/servers`, `/v1/exec` and the session routes, with the bearer token in the
   config directory and `danger.rs` gating destructive commands. Verified against a
   real server: a value round-trips as exact RESP, `FLUSHALL` is refused with 428
   and `danger.flushall` until confirmed, and `MULTI`/`SET`/`EXEC` commits through a
   session while a mismatched db on the same token is refused.
3. The client half. **Mostly done, 2026-09-17**: `RedisAsyncConn::Bridge(BridgeConn)`
   implements `ConnectionLike` over the `BridgeTransport` trait, single commands and
   pipelines both, with `PipelineSpec` carrying the framing so an atomic pipeline's
   `MULTI`/`EXEC` is rebuilt on the far side and the caller's offset still counts.
   The `BridgeTransport` implementation over GPUI's `HttpClient` lives in
   `zedis-web/src/transport.rs` — in the crate whose consumer it is, rather than in
   the desktop app behind a feature. It is not yet *wired* into
   `ConnectionManager`: nothing hands out `RedisAsyncConn::Bridge` yet, because
   that is the same decision as how the app learns a bridge's URL, and the browser
   reaches Redis through `zedis-bridge`'s own routes until the views are running.

   The protocol is verified against a real server: a plain command, a non-atomic
   pipeline, an atomic pipeline returning its `EXEC` array, and a `FLUSHALL`
   smuggled into position two of a pipeline, which is refused because every command
   in a batch is judged, not just the first.
4. Compile `zedis-ui` to wasm to prove the toolchain. **Done, 2026-09-17**, on
   nightly from `zedis-web/`. Two real portability fixes fell out: `zedis-core`'s
   `fs` module and its `home` / `directories` / `path-absolutize` dependencies are
   now target-gated (`home` has no wasm branch at all and simply fails to
   compile), and the tree-sitter feature moved off `[workspace.dependencies]`.
   `zedis-web` exists as the entry point, with `transport.rs` — the
   `BridgeTransport` implementation over GPUI's `HttpClient` — living there, since
   the browser is its consumer.

   Its wasm build does **not** link yet, and the reason is step 6's work, not the
   toolchain: it pulls `zedis-connection`, whose `tokio/net` reaches `mio`.
   `getrandom` also needs its `js` feature there.
5. Unblock `zedis-core` (`home`, `fs.rs`, `web-time`), which frees 9,300 lines of
   pure logic. **Done, 2026-09-17** — except that the `web-time` half was
   recorded here and never landed: no crate depended on it, and `cargo check`
   cannot tell, because `Instant::now()` compiles for wasm and only panics
   when called. The first browser run found it (step 13).
6. `zedis-db`'s storage seam. **Done, 2026-09-18** — `mem_store.rs`, see the table
   above. `make check-web` now covers `zedis-core`, `zedis-ui`,
   `zedis-connection`, `zedis-db` and `zedis-web`, all five of which compile for
   `wasm32-unknown-unknown`.
7. The app crate's manifest and its `smol` uses. **Done, 2026-09-18.** The
   dependency graph now resolves for `wasm32-unknown-unknown`, so the build
   reaches `src/` for the first time. What it took: the desktop-only crates moved
   under `cfg(not(target_family = "wasm"))`, `redis` split the way
   `zedis-connection` splits it, the `tray-icon` gate changed from an OS
   allow-list to a family gate, and `smol` — which reaches `async-io` → `rustix`
   → `errno`, a crate with no wasm target — reduced to the two pieces that are
   portable anyway: its channel is `async-channel` (17 sites now say
   `helpers::channel`) and its lock is `async-lock`. `smol::Timer` and
   `smol::spawn` became GPUI's own executor, which every platform backend
   implements. The rest — the migration job's file work, the session file, the
   SSH host-key prompt — is gated.

   `rust-embed`'s `include-exclude` was the second instance of the feature
   trap section 4 describes: `assets.rs` and `i18n_loader.rs` have always used
   `#[include = "..."]` and the feature was only ever on because another crate
   in the graph asked for it.

8. The app crate's source: **338 errors, and they are four piles, not 338
   problems.**

   | Pile | Count | What it is |
   |---|---|---|
   | `query_async` on `Cmd` / `Pipeline` | ~145 | Exactly what `BridgeQuery` substitutes for. One `#[cfg(target_family = "wasm")] use` per file, no call site touched — the pattern already proved on `zedis-connection`'s 201 sites |
   | Unresolved imports of gated APIs | ~90 | `zedis_core::fs`, `crate::connection::*`, `crate::db::*`. The app-side half of gates already made below it |
   | Methods on a type that is `_` or `!` | ~50 | All cascading from one hole: `ConnectionManager` is native-only, so every `get_connection_manager()` has no type and each method on it fails separately |
   | Small, mechanical | ~10 | `send_blocking` on a browser thread, `Assets::get`, a few signatures |

   So the only **design** decision left was the third pile: `ConnectionManager`
   had to exist in the browser and hand out `RedisAsyncConn::Bridge`.

   **Done, 2026-09-18, and it did not need a second manager.** `pool.rs` now
   compiles for both targets with the difference reduced to one function. The
   split is `Reached` — a server that has been reached, before anything is asked
   of it — and everything past it is unchanged, because it is all ordinary
   commands: the read-only access-mode probe (ADR 2), `MODULE LIST`, the database
   count, `INFO server`. `detect_server_type` came along for the same reason: its
   body is two commands, so the browser runs the desktop's own detection over the
   bridge and only the native wrapper's argument type is native.

   What the browser's `reach` does instead of dialling: build a `BridgeConn` from
   a process-wide transport, and — for a cluster — learn the masters from a
   `PING` fan-out. **Labels, not addresses.** That is the same rule as everywhere
   else here: a label is all `query_async_masters_with_option` aligns replies on,
   and handing a browser a dialable address is what the bridge exists to prevent.

   Three seams came with it, each because this crate deliberately has no HTTP
   client:

   - `set_bridge_transport` — the process-wide transport, installed by the web
     entry point, in the shape `init_commands_json` already established.
   - `BridgeTransport::open_session` / `close_session`, **required** rather than
     defaulted. What they protect is not a nicety: `SELECT`, `AUTH`,
     `CLIENT SETNAME` and `MULTI` are connection state (ADR 4), so a transport
     that cannot pin a connection must say so rather than return a shared one
     that looks dedicated. `RedisClient::open_dedicated_connection` in the browser
     is a bridge session.
   - `BridgeServerStore` — saving the server list is an HTTP request, because
     the file and the credentials belong to the bridge. Without an installed
     store `save_servers` fails loudly instead of appearing to save. Reading is
     the in-memory map both builds already share, filled by `set_servers_cache`
     from what the bridge answered.

   The pile it was blocking collapsed with it: **328 errors → 234**, and every
   "method on a type that is `_`" is gone. What is left is 144 `query_async`
   imports, 52 gated imports, and about a dozen small things.

9. The mechanical piles. **Done, 2026-09-18: 234 errors → 88.**

   - **145 `query_async` errors closed by 25 one-line imports**, one per file,
     no call site touched. Exactly what the probe predicted before any of this
     was written, and the second time the same substitution has paid off at
     this scale.
   - **`zedis_core::fs` has a second implementation** (`fs_web.rs`) instead of
     a `cfg` on the module. The surface is the same and it answers honestly:
     `NotFound` for directories that do not exist, `Unsupported` for writes
     that cannot happen. Not silent success — a save that returned `Ok` would
     look like it worked and lose the user's servers — and not a missing
     module, which would have meant gating ~20 call sites and every function
     above them.
   - **The feature probe is portable now.** It only ever sends commands
     (`INFO server`, `SLOWLOG GET 0`, `COMMAND INFO`, `ACL DRYRUN`), so typing
     it to `RedisAsyncConn` instead of `MultiplexedConnection` was the whole
     change. It matters for the web build rather than being tidiness: the probe
     is what decides which panels and buttons a server offers, so without it
     the browser would show every button and learn from the failures.

   The long tail after that is app-side gating, and it is under way: eight
   helpers (the updater and its `ureq`, the file logger, tree-sitter, the AI
   analysis, proxy validation, the local-data file export, the single-instance
   socket, the diagnostics zip), eleven views (MONITOR, Pub/Sub, keyspace
   events, prefix compare, connection diagnostics, migration, multi-search, the
   proto and script editors, the report dialog, the Sentinel dialogs), the tray
   and two server-state modules are gated. Three small things went the other
   way instead of being gated, because gating them would have been wrong:
   `os_info` became `helpers::platform_info` (the web answer is deliberately
   vague — `navigator.userAgent` in a crash report would ship the user's
   fingerprint, which ADR 7 would have to list), `install_crypto_provider` is a
   no-op where no rustls is linked, and `open_single_connection` in the browser
   is a bridge session, so the terminal and the live tail keep their call sites.

   Each round of gating exposes the callers of what was just gated, so the
   error count oscillates on the way down rather than falling monotonically.

   **The key tree needed the protocol's one remaining generalisation, and it
   has it. Done, 2026-09-18.**

   `RedisClient::scan` enriches each page with `TYPE` (and `TTL`) as *one
   pipeline per master*, and that cannot be flattened into a single pipeline
   over one connection: redis-rs refuses a cluster pipeline whose commands span
   slots (`route_for_pipeline` in
   `cluster_handling/async_connection/routing.rs` answers `CrossSlot`), and a
   page of scanned keys always does.

   So `fanout_nodes` now allows a label to **repeat**: the commands carrying it
   run as one pipeline on that node, in the order sent, and the reply carries
   one label per frame. No new field — a relaxed constraint, not a bigger
   protocol — and still a transport concept like `session` and `fanout`, so the
   rule that the protocol does not grow when Zedis gains a Redis feature
   survives.

   One new method carries it on the client side,
   `RedisClient::query_async_masters_pipelines`, and it replaced **six**
   hand-rolled `query_async_masters_pipeline` call sites: the scan's
   `TYPE`/`TTL` round, the memory sample, the value search, the bulk unlink and
   two collection readers. All five methods that were gated off wasm for this
   reason — `scan`, `first_scan`, `unlike_keys`, `scan_values_round`,
   `sample_scan_memory_usage` — are portable again.

   Verified against the live 3-master cluster, through HTTP: 40 keys seeded,
   `SCAN` fanned out one cursor per node (13 / 11 / 16), then 40 `TYPE`
   commands sent as three batches and 40 frames returned, every one labelled
   with the node that answered it. The first attempt failed with `MOVED`, which
   was the protocol being right and the test being wrong — a command aimed at a
   node that does not own its slot *should* be refused by the server, and the
   real caller only ever asks a node about the keys that node just returned.
   `aim_at_nodes` in the bridge is the pure half of the grouping and carries
   the regression tests, including the one that matters most: a label naming a
   master this process has never heard of is dropped, never guessed at.

10. The app crate. **Done, 2026-09-18: `zedis-gui` compiles as a library for
    `wasm32-unknown-unknown`, and `make check-web` now covers it.** The last
    58 errors went three ways, and the split is the point:

    - **Made portable rather than gated**, because the code was already only
      commands: `server_report` (`MEMORY DOCTOR` / `LATENCY DOCTOR`), which meant
      the metrics and slow-log panels needed no edit at all; `dump_restore`'s
      command half (`copy_key`, `ConflictMode`, `RestoreStatus`), split from its
      file half so copy-key stays in; `script_kill`'s two enums, so the three
      views that name them compile and the buttons are simply hidden. The
      update flow's *types* moved to `helpers/update_info.rs` so the chip and
      the dialog that show an update compile everywhere while nothing in a tab
      can go and fetch one.
    - **Gated at the view or state**, with an honest stand-in where a route
      still resolves: Topology, MONITOR and keyspace events render the same
      `ZedisUnsupportedPanel` a missing command gets (Topology is the Sentinel
      / cluster *administration* surface — 25 call sites that dial a node — and
      one placeholder beat 25 cfgs in a 3,000-line view); the Pub/Sub channel
      mode shows a one-line notice; the import / export / compare actions
      return early; the proto and script routes fall through to the default
      content. Multi-search, the diagnostics bundle, the updater, the tray, the
      single-instance socket, the file logger, the AI assistant, proxy
      validation and the local-data backup are compiled out with their
      callers.
    - **Given a browser body** where the surface had to exist: `fs` answers
      `NotFound` / `Unsupported` (never `Ok`), the crash hook logs the report to
      the console instead of a file, `take_pending_crash` is `None`, the proxy
      field accepts only clearing, and the two secret fields (proxy URI, AI key)
      store plain in memory — there is no master key in a tab and nothing is
      written to disk there, so the stored form *is* the plain form.

    Two things a reader should know. `assets.rs` no longer reaches the kit's
    embedded icons on wasm, because there the kit's `Assets` is a *fetching*
    source (`AllAssets::new(endpoint)`); the web entry has to compose the two
    sources — app-embedded first, kit-fetched behind it. And the 140 warnings
    the wasm build prints are dead code from `zedis-web` not yet depending on
    `zedis-gui`; they are the reason `check-web` is still `check`, not clippy.

11. Wired. **2026-09-18: `zedis-web` depends on `zedis-gui`, links, and the
    bundle is served by the bridge** — `make web-bundle` (wasm-pack, `--profile
    web`) then `make web-serve`. What the entry does, in order: installs one
    `HttpBridgeTransport` as both the transport and the `BridgeServerStore`,
    with **no token** — the page logged in for a cookie and a same-origin fetch
    carries it, so the transport now omits the `Authorization` header when the
    token is empty; runs `init_embedded_commands`, `init_database`
    (`mem_store`) and `init_caches`; fetches the UI font and `GET /v1/servers`;
    then, with both in hand, registers the font, fills the server cache and
    calls the desktop's own `launch`.

    Four decisions inside that:

    - **The font.** The web text system starts from an *empty* font database
      and hardcodes `.SystemUIFont` → `"IBM Plex Sans"` (`gpui-pre-web`
      `platform.rs:179`). Nothing in the dependency graph embeds a face, and the
      repo bundles only JetBrains Mono, so `zedis-web/www/fonts/` carries IBM
      Plex Sans (OFL, Google Fonts' variable width-and-weight file),
      fetched at startup rather than embedded so the desktop binary does not
      grow. Bold renders at the default instance until a static Bold face joins
      it.
    - **Two asset sources, one door.** On wasm the kit's `Assets` is a
      *fetching* source (`/assets/icons/<name>.svg`, answering `None` until the
      download lands), not the embedded one the desktop has. `WebAssets` asks
      the app's embedded source first and the kit's behind it; `web-bundle`
      copies the kit's 1,830 icons beside the page from wherever cargo has the
      crate, so they are not checked in.
    - **Saving the server list is a diff.** The browser's copy of an existing
      entry has no credentials, so sending the list whole would strip every
      saved password. The store posts what the bridge lacks (a fresh form
      submission, whole, for the bridge to stamp an id and dial) and deletes
      what the list no longer names; edits were not carried until 1b gave the
      browser the settings to edit and the bridge a `PUT` to receive them. The bridge grew
      `POST /v1/servers { server }` beside `{ url }`, and `DELETE
      /v1/servers/{id}`. The store answers with the bridge's list, and that —
      not the browser's — is what the cache holds afterwards.
    - **`rust-embed`'s `compression` is desktop-only.** Its inflater is the C
      zstd (`zstd-sys`, behind rust-embed's compression helper crate), which
      has no wasm objects and failed
      the link with `undefined symbol: ZSTD_*` — the fourth time this work has
      found a native-only backend hiding behind a feature. The browser embeds
      raw; the wire compresses.

    Two things the first browser run taught, both fixed the same night:

    - **The app must be owned by an `ApplicationHandle`.** On the desktop
      `Platform::run` blocks for the life of the app and `Application::run`'s
      stack frame owns the state. The browser's run loop belongs to the browser:
      `WebPlatform::run` schedules the launch closure and returns at once, so the
      closure was the last owner and the first fetch to complete afterwards
      panicked with `app was released before async operation completed`. gpui
      names this shape — "GPUI compiled into a Wasm guest" — and provides
      `Application::run_embedded`, whose returned handle is the owner; the entry
      keeps it in a thread-local for the life of the page.
    - **Keep the wasm name section.** `profile.web` inherited release's
      `strip = true`, and the first panic arrived as ten unreadable addresses.
      `strip = "none"` costs size (25.6 → 47 MB, before any wasm-opt) and buys a
      stack that names `AsyncApp::app` and the closure in `zedis_web::run` that
      called it. For a build that is still being brought up, that trade is not
      close. It is the *iteration* build's trade, though, not the shipped
      one: measured by section, the 47 MB is 20.5 MB of `name`, 19.5 MB of
      code and 4.8 MB of data, so the bundle now has the desktop's two forms.
      `make web-bundle` keeps `profile.web` as above; `make web-release` is
      `release`'s counterpart — `profile.web-release` inherits the desktop
      release profile whole (fat LTO, one codegen unit, `strip = true`,
      `panic = "abort"`) at `opt-level = "s"`, and `scripts/web-bundle.sh
      --release` runs `wasm-opt -Oz --strip-debug --strip-producers` over
      the result. The strip is the name section and nothing else that
      matters: the panic message and its file:line survive it, as they do in
      the desktop release. Both forms write the same `www/wasm/`, so
      `web-serve` serves whichever was built last.

      Measured on 2026-09-18, the same module at each step:

      | Step | raw | gzip -9 | brotli -9 |
      |---|---|---|---|
      | `profile.web` (names kept, thin LTO) | 44.9 MiB | 9.4 | 6.3 |
      | `profile.web-release` (stripped, fat LTO) | 22.9 MiB | 7.0 | 5.2 |
      | + `wasm-opt -Oz` | 20.4 MiB | 7.3 | 5.6 |

      Two things in that table decide the script. The strip is 22 MB and
      free; fat LTO on one unit took the code from 19.5 to 18.2 MB. wasm-opt
      is a trade rather than a win: `-Oz`, `-Os`, `-O3` and `-Oz --converge`
      all land within 1 MB of each other — about 11% off the raw size and 6
      to 8% *onto* the compressed one, presumably because the passes remove
      the repetition a compressor was already exploiting. Raw is what the
      bridge serves today and what the browser parses and keeps in memory,
      so the release keeps `-Oz`; if the wire ever becomes the constraint
      (a precompressed `.wasm.br` served with `Content-Encoding: br` is the
      obvious next step, and 5.2 MiB is the number to expect), dropping the
      optimizer is the cheaper compressed bundle. One mechanical detail:
      cargo's strip removes the `target_features` section too, so the script
      names the features itself — rustc's six-feature baseline plus
      `threads`, because gpui-pre-web's default `multithreaded` feature
      leaves atomic instructions in the module even for the single-threaded
      app, and wasm-opt refuses them without the flag.

      And speed, since the question follows from size. Both forms are
      *optimised* builds — `profile.web` inherits release too — so this is
      not the debug-versus-release gap of a desktop binary; there is no
      unoptimised wasm in this project. Measured in headless Chrome over
      loopback, three runs each, cold profile: the canvas is up in ~0.36 s
      with the iteration bundle and ~0.18 s with the release one (less to
      fetch and stream-compile), and a fixed interaction — open a server,
      expand three folders, select a key, open and close the palette six
      times — costs ~1.23 s of main-thread time against ~1.11 s, about 10%.
      Fat LTO's cross-crate inlining is most of that; `wasm-opt -Oz` is a
      size pass and not expected to help speed. The lever that would buy
      more is `opt-level = 3` instead of `"s"`, at a larger module — not
      taken, because nothing here is compute-bound: the frame is drawn by
      the GPU and the waiting is for Redis.

      **The deployment package is one tarball**, `make web-dist`
      (`scripts/web-dist.sh`): `zedis-bridge` built with the desktop release
      profile — the release bundle compiled into it, so page, glue, module,
      fonts and icons travel inside the binary (not the `.d.ts` and
      `package.json` wasm-pack writes for an npm consumer) — plus `LICENSE`
      and a `README.txt` that says how to run it, as
      `zedis-web-<version>-<host triple>.tar.gz` under `web-dist/` in
      cargo's target directory — asked of `cargo metadata`, because a
      `build.target-dir` in `~/.cargo/config.toml` moves it and the first
      run of this script found no binary at `target/release/` for exactly
      that reason — with a `.sha256`. The script rebuilds the bundle in
      release form rather than
      packaging `www/wasm/` as found, because that directory holds whichever
      form was built last. The binary is the host's, so the tarball is
      per-platform: a CI job per target is the natural next step, and it
      would be the bridge's first, since nothing builds it in release today.

    Also from that run: the UI font now rides in with the `run(origin,
    ui_font)` call and is registered before the kit's `init` (which does probe
    `all_font_names()`), rather than fetched from inside after it; the
    `tracing` subscriber is installed with `set_global_default` rather than
    `init`, because `web_init` already owns the `log` logger for the platform's
    console writer; and the bridge sends `Cache-Control: no-cache` on static
    files so a rebuilt bundle is picked up on the next load.

    Verified over HTTP against the served bundle: every path answers with its
    type (`application/wasm` for the module), the glue exports
    `run(origin)` as the page calls it, login trades the token for a cookie
    that opens `/v1/servers`, and a live standalone registered through the
    bridge (dial-verified) with keys seeded through `/v1/exec`. **Not yet
    verified: the canvas.** WebGPU/WebGL start-up, the font landing before the
    first frame, the sidebar listing the server and the key tree filling
    through the bridge need a browser, which this session had no way to drive.

12. After the browser confirms the frame: the sidebar hides the routes that only
    resolve to a placeholder, secondary windows and file dialogs are re-routed,
    and a static Bold face joins the UI font.

13. The first browser run. **2026-09-18: the canvas paints.** The evidence is
    indirect but conclusive: the first thing the console showed was the
    window-bounds save failing with `a config directory does not exist in a
    browser`, and that callback only runs from `Zedis::render` after the
    window has been created, laid out and given bounds — WebGPU start-up, the
    font and the first frame all happened before it. Two things it found:

    - **`time not implemented on this platform`**, a panic from
      `std::time::Instant::now()`. The swap step 5 promised is now real: the
      workspace depends on `web-time` (std's own types on every native
      target, the browser's clock on wasm), `zedis-core`'s `ttl_cache` and
      `codec` and the eight app files that time a scan, a heartbeat or a
      command import `Instant` from it, and the workspace manifest says which
      files keep std (the native-only updater and file logger). `Duration`
      stays std everywhere. `cargo check --target wasm32-unknown-unknown`
      cannot catch a regression here — the call compiles — so the rule is the
      import, not the check: a file that compiles for both targets never
      writes `std::time::Instant` or `std::time::SystemTime`.
    - **The window-bounds save** is desktop-only now
      (`Zedis::save_window_placement`): the browser sizes the page and a tab
      has no file, so `persist_window_state` there records the bounds for the
      next frame's comparison and stops. Every other `zedis.toml` save still
      fails loudly in a tab — that is the honest answer `fs_web` was written
      to give — but this one fired unprompted on every resize and its value is
      meaningless in a browser.

    Also from this run, on the bridge side: `make web-serve` began with a
    macOS keychain prompt, because the bridge resolved the master key the way
    the desktop does. It no longer does (section 3, `disable_keychain`). And
    it ran in the **installed app's** config directory, serving the real
    server list — nothing was rewritten, but a save from the page would have
    been, under a key the desktop app does not hold. `make web-serve` now sets
    `RUST_ENV=dev`, as `bacon.toml` does for `make dev`, so the bridge lives
    in `<config_dir>/dev` beside the desktop dev run: same list, same key
    file, and the installed app's files are never in reach.

    Still unverified after the panic: the sidebar listing the server and the
    key tree filling through the bridge. **Both verified later the same day**
    (step 14), and from a session for the first time: headless Chrome driven
    over the DevTools protocol — the login cookie set with
    `Network.setCookie`, clicks sent with `Input.dispatchMouseEvent`, the
    canvas read back with `Page.captureScreenshot`. WebGPU initialised
    (`BrowserWebGpu`), the sidebar listed the bridge's servers, a click
    connected through `/v1/exec`, the key tree filled and a value opened.

14. CJK text. **2026-09-18: drawn by the browser, not by a bundled font.**
    Chinese rendered as missing glyphs, for the reason step 11's font note
    implies: the web text system shapes with the fonts it is handed, those are
    IBM Plex Sans and JetBrains Mono, and a page cannot read the system's
    fonts to add one. The obvious fix is to ship a CJK face, and it was not
    taken: Noto Sans SC is about 10 MB for one weight of one script — half
    again on top of the whole 20 MB module — and Simplified Chinese alone
    would still leave Japanese and Korean keys as boxes.

    Upstream had already solved it. `gpui-pre-web` 0.3.4 added
    `CanvasFontFallback`: when the loaded fonts lack a glyph, an eligible
    grapheme is drawn on a canvas with the browser's own `sans-serif` —
    PingFang, YaHei, Noto, whatever the viewer's system has — and the default
    (`Emoji`) stops short of CJK. `EmojiAndCjk` covers Han, kana, Hangul and
    the full-width punctuation. Two things followed:

    - **The gpui family moved 0.3.3 → 0.3.5**, desktop included, because the
      family is one version (`cargo tree -i gpui-pre` shows one). gpui-kit
      0.6.1 asks for `^0.3.1`, so this is inside what it already allowed.
    - **`zedis-web` constructs `WebPlatform` itself.** The policy is fixed at
      construction and `gpui_platform::single_threaded_web()` takes no
      argument, so the entry spells that function's three lines out with the
      fallback it wants. That made `gpui-pre-web` a direct dependency
      (`gpui_web`), the fifth name that has to stay on the family version.

    What it costs is stated upstream as "approximate independent rendering":
    each grapheme is drawn on its own, so there is no shaping across them
    (which horizontal CJK does not need), the face is the viewer's rather
    than ours, and a machine with no CJK font at all still shows boxes.
    Bundling a face stays available if identical rendering everywhere ever
    matters more than the download.

    **Typing it** had a second, separate fault: every composition leaked its
    first keystroke, so `ni` → `你` arrived as `n你`. That is upstream's, and
    still there in 0.3.5: the backend's `keydown` handler decides "this key
    belongs to the input method" from `isComposing` alone, but the first key
    of a composition arrives *before* `compositionstart` — `isComposing` is
    false and, in Chrome on macOS, `key` is the letter — so the letter is
    inserted as text. `keyCode` 229 is what every browser sets on a key the
    IME took, and nothing checks it. The fix lives in `www/index.html`, not
    in a fork (the workspace takes crates.io sources only): a capture-phase
    `keydown` listener on `window` stops those events before they reach the
    backend's listener, without `preventDefault`, so the IME still sees them.
    It also keeps Enter, Backspace and the arrows from firing application
    bindings while they are choosing a candidate. Reproduced and confirmed
    fixed in headless Chrome with the event sequence macOS sends
    (`rawKeyDown` 229, `imeSetComposition`, `insertText`): `n你h好` before,
    `ab你好c` after, plain typing untouched. Delete the listener when upstream
    checks for 229 itself.

    Verified in headless Chrome against a live server: a server named in
    Chinese in the sidebar, the title bar and its tooltip; `用户` / `订单`
    folders and a `张三` key in the tree; and a value holding Chinese,
    Japanese kana, Hangul, full-width punctuation and an emoji in the editor,
    all drawn. On the desktop side the family bump passed `make lint`,
    `make test`, `make check-web` and the first-frame smoke run.

15. A cluster in the browser. **2026-09-18: the one command whose reply
    depends on which connection carried it.** Selecting a Redis 5 cluster
    failed with `Response type not string compatible`, the value being a map
    of node address → `INFO` text, after a log line saying detection had
    fallen back to standalone. Step 8 said the browser "runs the desktop's
    own detection over the bridge", and the body is indeed the same two
    commands — but not over the same kind of connection. The desktop asks
    `INFO cluster` on a single connection to the seed. The browser asks
    through `/v1/exec`, where the bridge, having already recognised the entry
    as a cluster, hands out redis-rs's *cluster-routed* connection; that one
    sends a keyless `INFO` to every node and answers a map
    (`ResponsePolicy::Special`). Read as text, that was an error, the entry
    became "standalone", and `INFO server` then took the non-cluster branch
    and met the same map — even though the cluster branch beside it has
    always read exactly that shape.

    The fix is `cluster_enabled(reply)` in `pool.rs`: either shape, any
    node's answer, since they are members of one cluster. It is the only
    place the asymmetry could bite, because it is the only command sent
    before the type is known; everything after it already branches on
    `ServerType::Cluster`, on both targets, because the desktop's pooled
    connection for a cluster is the same routed one. The earlier cluster
    verification in step 9 never saw this: it drove the protocol with
    explicit fan-outs and never selected a cluster in a page.

16. Shortcuts. **2026-09-18: a compile-time fact that is a run-time one in a
    browser.** On a Mac every shortcut in the page looked dead. They were not
    — Ctrl+K opened the palette — but nobody on a Mac presses Ctrl+K. The
    table in `helpers/action.rs` spells its defaults `secondary-k`, and GPUI
    resolves `secondary` when it parses the keystroke, with
    `cfg!(target_os = "macos")`: ⌘ in the macOS binary, Ctrl everywhere else,
    and a wasm module is "everywhere else" on every machine it runs on. The
    labels had the same fault for the same reason (`humanize_keystroke` was
    `#[cfg]`-branched), which is why the page told a Mac user "Ctrl+K".

    One module serves all visitors, so the answer cannot be a build. The
    browser build binds **both** spellings — each `secondary-…` keystroke and
    its `cmd-…` twin (`web_twin`) — which needs no detection to be correct: on
    a keyboard without a command key the twin is the Windows / Super key,
    which the OS keeps for itself. Detection is only for the *labels*: the
    page passes `run()` a third argument from `navigator`, and
    `uses_command_key()` is that at run time in the browser and the old
    compile-time fact on the desktop. A wrong guess mislabels and breaks
    nothing.

    What no spelling fixes: a page never receives the combinations the
    browser reserves for its own windows and tabs — ⌘N ⌘T ⌘W ⌘Q on a Mac,
    Ctrl+N Ctrl+T Ctrl+W on Windows and Linux. New key, new tab, close tab and
    quit are therefore desktop shortcuts only; in the page they are buttons.
    (On a Mac the Ctrl spellings of those do arrive, since Chrome reserves
    only the ⌘ ones there.) Verified in headless Chrome with synthetic key
    events: before, Ctrl+K opened the palette and ⌘K did nothing.

17. Preferences. **2026-09-18: in the browser, because they are the
    visitor's.** Until now a tab kept its settings in memory: `fs_web`
    refused the write, the console said so, and a reload forgot the language.
    That was the honest stand-in while nothing better existed, not a
    decision. The decision is that `zedis.toml` — language, theme, font size,
    layout, dismissed hints — is per person *and per device* (the font size
    that suits a laptop does not suit a wall display), needs no account to
    tell people apart (in token mode the bridge cannot), and costs the bridge
    no state. So it goes in `localStorage`. Shared, authored data — tags,
    favorites, the script library — is the other category and still belongs
    on the bridge, behind the `mem_store` seam; this step does not touch it.

    The shape follows the house pattern: the call sites do not change, the
    implementation behind the name does. `fs_web` keeps answering
    `Unsupported` for every real path and makes one exception that is spelled
    as one — a path from `browser_store_path(name)` lives under
    `BROWSER_STORE_DIR` and is a `localStorage` entry (`zedis:<name>`). The
    app asks for `zedis.toml` through it on wasm and through the config
    directory on the desktop; `load_config_with_recovery` and
    `write_file_atomic_with_backup` are the same two calls on both. A stored
    value that no longer parses is moved to `<key>.corrupt` and reported as a
    reset, the desktop's quarantine in miniature; there is no `.bak`, because
    `setItem` cannot be half-done. The two secrets the state can hold (the AI
    key, a proxy URI with credentials) are left out of what is stored: every
    script on the origin can read `localStorage`, and a tab has no master key
    to encrypt under.

    The first-run language follows the browser: `sys-locale` reads
    `navigator.languages` only behind its `js` feature, without which every
    visitor started in English. Verified in headless Chrome: a zh-CN browser
    opens in Chinese; switching to Japanese writes `locale = "ja"`, and a
    reload in the same profile stays Japanese with the welcome dialog gone; a
    corrupted entry is set aside and the page starts from defaults.

18. Two requests that were answered with something else. **2026-09-18.**

    *"Keep the username and password in `localStorage`, so nobody types them
    again until a 401."* The goal was taken and the means were not. What made
    people type their password over and over was that logins lived in the
    bridge's **memory**: every restart signed everybody out, and in
    development the bridge restarts all day. A password in `localStorage` would
    have fixed the symptom by undoing the one property the login design exists
    for — that the browser never holds the credential, only a revocable,
    expiring id no script can read. So the logins went to disk instead
    (`bridge-logins.json`, `0600`): the key is the SHA-256 of the cookie id,
    never the id, so the file holds nothing a reader could present; each
    record carries a salted tag of the account's password, so changing a
    password still ends every login opened under the old one, which is what
    changing a password is for; *Keep me signed in on this device* is thirty
    idle days instead of a working one. The form is a real `<form>` with
    `autocomplete`-tagged fields, which is what lets the browser's own
    password manager save and fill the credential — the OS-protected version
    of what was asked for. And a 401 that arrives while the application is
    already running reloads the page, which is the way back to the form.
    Verified: both logins answer 200 across a restart where both used to be
    401; the file contains neither the id nor the password; a changed
    password ends that account's login and no other; a logout stays a logout
    after a restart.

    *"The wasm is too big — split it into gpui-kit, the zedis crates, and
    the rest."* There is no such seam to cut along. Rust links every crate
    into one module, gpui-kit's generics are instantiated *inside* zedis's
    code, LTO inlines across crates, and the wasm-bindgen target has no
    dynamic linking; and three files are the same bytes as one, fetched no
    faster than a single streamed compile. What "too big" was actually
    costing was the wire, twice over. The bridge sent the module **raw**, so
    it now stores it compressed and only compressed — `.gz` always, since
    every browser accepts gzip, `.br` from a release bundle — and answers
    with the best coding the caller accepts (`static_files::negotiate`),
    inflating on the way out for the rare caller that accepts none. Leaving
    the raw module out of the embed makes the binary *smaller* than the
    module it serves. And every reload fetched the whole thing **again**:
    `Cache-Control: no-cache` means "revalidate", and with no validator there
    is nothing to revalidate against. An `ETag` makes it a `304`. Measured in
    Chrome: 21.6 MB raw → 5.1 MB on a first load (brotli; 7.7 MB where only
    gzip is on offer, which is plain http to a remote host), and 156 bytes on
    a reload.

    The second change exposed a bug that had always been there. The kit's
    browser asset source fetches icons on demand and, when one lands, puts it
    in its cache and tells nobody; the icon shows only if something else
    repaints afterwards. A first visit hides that. A reload, once icons came
    back in a millisecond, drew a home page with no plus sign on its button.
    `WebAssets` now remembers what it could not answer and
    `repaint_when_icons_land` refreshes the windows when an entry starts
    answering — giving up after a bounded number of looks, because asking the
    kit's source for a missing icon *starts a fetch*, and an icon that 404s
    must not be requested for as long as the page is open.

19. The image. **2026-09-18.** A single binary with the page inside it is
    most of the way to a container, and `./Dockerfile` is the rest:
    `vicanso/zedis-web`, the bridge on `gcr.io/distroless/cc-debian12:nonroot`
    — glibc, libgcc and nothing else, since the only native dependency is
    `ring` and TLS verifies against roots compiled into the binary.
    `ZEDIS_CONFIG_DIR=/data` is the one volume (server list, key file, saved
    logins), the default command listens on every interface because loopback
    inside a container reaches nobody, and `ZEDIS_BRIDGE_USERS` is required as
    it is everywhere.

    The Dockerfile is **self-contained**: `docker build .` on a fresh clone
    makes the image, with no bundle built beforehand. It therefore installs
    what `make web-release` needs — both toolchains (the pinned stable, and
    the nightly `zedis-web/` selects), `wasm-pack`, a *current* binaryen from
    GitHub rather than Debian's, which is years older than the wasm rustc now
    emits, `brotli`, and libclang, because gpui's build script runs bindgen on
    every target. The cost is that each architecture compiles the
    architecture-independent wasm for itself. Building it once and handing it
    to both would be faster and would make the Dockerfile depend on a step
    outside it; the release runs rarely enough that the simpler thing won.

    `publish.yml` builds it natively per architecture (`ubuntu-22.04` and
    `ubuntu-22.04-arm` — no QEMU, under which a fat-LTO Rust build takes
    hours), pushes each half under `<channel>-<arch>`, and `docker_manifest`
    joins them: a tag push becomes `:1.2.3` and `:latest`, main becomes
    `:nightly`, anything else builds without pushing. Per-channel halves, so a
    nightly and a release running together never overwrite each other. The
    on-switch is the Docker Hub secrets, the way `SIGNPATH_ORGANIZATION_ID` is
    for Windows signing: without them the image is still *built* — a broken
    Dockerfile still fails the run — and only the push is skipped, with a
    warning, rather than failing a release over an account not yet set up.
    No layer cache: the layer that costs the time is the compile, which every
    source change invalidates, and storing it would evict the desktop jobs'
    rust caches from the repository's quota.

Closed along the way: `fanout` for both master fan-out methods, cookie login, and
`--static`. Still open before a browser can run this: wiring `HttpBridgeTransport`
to a consumer, and everything under *Per-crate cost* below.

Steps 2 and 3 before step 4 is the point: a bridge bug and a wasm toolchain bug
should never be diagnosed at the same time.
