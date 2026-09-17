# 9. The web build bridges RESP, not features

Date: 2026-09-17 · Status: proposed

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
POST   /v1/login   { token }          -> Set-Cookie (HttpOnly, SameSite=Strict)
POST   /v1/logout                     -> clears it
GET    /v1/servers                    -> [{ id, name }]     # no hosts, no secrets
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
bridge process and the browser only ever names a server id.

The server side runs on tokio with axum. Owning an HTTP server that is exposed to
other people means wanting TLS termination, body limits, timeouts, connection
caps and graceful shutdown, which is not something to hand-roll. The dependency
weight is acceptable precisely because it lands in a separate binary that the GUI
build never compiles.

`--static <dir>` serves the web build from the bridge itself, which is what makes
the page same-origin with the API: no CORS to configure, and the login cookie can
stay `SameSite=Strict` — a cookie that had to travel cross-site could not be, and
the protection it gives against another site driving the API would be gone. Two
details are load-bearing rather than incidental: `.wasm` must be served as
`application/wasm` or `WebAssembly.instantiateStreaming` refuses the module, and a
request path is *refused* rather than sanitised when it could climb out of the
root, with a canonical check behind it so a symlink inside the root cannot point
outside. The assets are unauthenticated, because the page has to load before
anyone can log in; everything it then asks for is behind the cookie.

A browser never holds the bearer token. It posts it once to `/v1/login` and gets
a cookie carrying a server-side login id: `HttpOnly`, so no script on the page can
read it, `SameSite=Strict`, so it does not ride along with a cross-site request,
and revocable and idle-expiring, which a copy of the long-lived token in
`localStorage` would be neither. Scripts and the CLI keep using the bearer header,
and both credentials open the same routes.

The cookie is `Secure` unless `--insecure-cookie` is passed for a plain-http local
run. That default is deliberate: a deployment that forgets TLS then sees a login
that visibly does not stick, instead of a credential travelling in the clear.

This is not optional hardening. `master_key.rs` falls back to a hard-coded
`LEGACY_MASTER_KEY` when neither the keychain nor the key file is reachable, which
is exactly the state a wasm build is in. A browser build that read
`redis-servers.toml` would be decrypting credentials with a published key.

### 4. Feature layout

No manifest in this workspace has a `[features]` section today, so the skeleton is
new work and `default` must reproduce today's build exactly.

```toml
[features]
default       = ["native"]
native        = ["ssh", "local-db", "updater", "tray", "native-fs", "keyring"]
web           = ["bridge-client"]
bridge-client = []
bridge-server = ["native"]
```

Portability gates use `target_family = "wasm"`, never an OS allow-list.

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
| `zedis-connection` | 20,381 | Bigger than "gate eight files", and the shape is now measured rather than guessed. Done: the dialing dependencies are target-gated, seven native-only modules excluded, and `RedisAsyncConn` extracted to `conn.rs` so the type survives without them. Split along one line: what builds commands and reads replies is portable, what dials is not. `RedisClient` stays; `RClient`, `ConnectionManager`, the pool, the pubsub modules and slot migration are native, as are `compare`, `probe`, `sentinel`, `script_kill`, `multi_search` and `dump_restore` — each of those is server-side work, so the browser asks the bridge rather than carrying a copy. `config.rs` keeps `RedisServer` and gates its TOML persistence, the keychain crypto and the TLS material. `bridge.rs` now has one shared body per operation with a `ConnectionLike` adapter on the host and `BridgeQuery`/`BridgePipeline` in the browser. What remains is the mechanical part the probe predicted: one `#[cfg(target_family = "wasm")] use` per file that calls `query_async` |
| `zedis-db` | 3,734 | Needs a storage trait. Eleven modules `use redb::` directly; the eight tables are all KV and map onto IndexedDB. `scripts.rs` is gated out |
| `zedis-gui` | 93,129 | Gate updater, tray, single_instance, proxy, logger, local_data, diagnostics; re-route secondary windows and file dialogs |

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
   The `BridgeTransport` implementation over GPUI's `HttpClient` is written and
   tested in `src/helpers/bridge.rs`, behind the `bridge-client` feature and off by
   default. It is not yet *wired*: nothing constructs it, because deciding how the
   app learns a bridge's URL and token is a product question, and `ConnectionManager`
   would have to hand out `RedisAsyncConn::Bridge` once it does. Until then the
   feature build trips the dead-code rule on purpose rather than hiding behind an
   `allow`.

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
   pure logic.
6. `zedis-db` storage trait, then the app-crate gating.

Closed along the way: `fanout` for both master fan-out methods, cookie login, and
`--static`. Still open before a browser can run this: wiring `HttpBridgeTransport`
to a consumer, and everything under *Per-crate cost* below.

Steps 2 and 3 before step 4 is the point: a bridge bug and a wasm toolchain bug
should never be diagnosed at the same time.
