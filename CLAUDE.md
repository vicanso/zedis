# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Zedis is a native, GPU-accelerated Redis GUI client built in Rust with [GPUI](https://www.gpui.rs/) (the Zed UI framework) and `gpui-component`.

## Commands

- Build / typecheck: `cargo check`
- Lint: **run `make lint` once as the final step before completing any work** (and after every change) — it is the required gate and runs `typos` + `cargo clippy --all-targets --all -- --deny=warnings`. Never report work as done until `make lint` passes clean. `cargo clippy --tests -- -D warnings` alone is *not* enough: it skips `typos`, so a misspelled word in code/comments passes locally but fails `make lint`/CI.
- Format: **run `make fmt` (`cargo fmt`) after every code change**, before the final `make lint`.
- Tests: `cargo test` — run a subset by substring filter, e.g. `cargo test fuzzy`, `cargo test config::`.
- Run dev: `make dev` (`bacon run`); with logs: `make debug` (`RUST_LOG=DEBUG`).
- Release: `make release` (`cargo build --release --features mimalloc`).
- Toolchain: Rust **1.95.0**, edition 2024.

Clippy `unwrap_used = "deny"` is set crate-wide **including tests** — use `.expect("…")` or proper matching in test code, never `.unwrap()`.

## Build-time locale parity gate (bites immediately)

`build.rs` enforces that **every** `locales/<lang>.toml` has the exact same key set as `locales/en.toml`. The 8 locales are `en, zh, de, es, fr, ja, pt, ru`. Adding or removing any UI string means editing **all 8 files** or `cargo check` panics. `build.rs` only re-runs when `locales/` changes — `touch locales/en.toml` to force the check. Translate natively where a section is already translated in that locale (most are); English fallback only where the surrounding section is itself untranslated.

## Workspace layout

Cargo workspace: root binary crate `zedis-gui` (bin name `zedis`) plus `members = ["crates/*", "zedis-cmd-builder"]`. Shared dependency versions live in the root `[workspace.dependencies]`; member crates reference them with `{ workspace = true }`.

- `crates/zedis-core` — GUI-free pure logic: the `Capability` permission matrix, fuzzy match, hex/csv/diff, JSONPath, TTL helpers, `env::is_development`. No gpui, no i18n.
- `crates/zedis-connection` — the Redis layer: pooled clients (`manager/{client,pool,slots}.rs`), `RedisServer` config, SSH tunnels, plus the shared `error.rs` and the `fs`/`string`/`time` helpers. No gpui (strings are `String`, converted at UI boundaries) and no i18n (`danger.rs` returns `i18n_key()`s for the UI to translate). The embedded `commands.json` is injected at startup via `init_commands_json` — this crate has no access to the app's assets.
- `crates/zedis-db` — the local storage layer: redb-backed managers (tags, favorites, history, trash, scripts) and proto descriptors, with its own `error.rs` (redb + prost/protox variants live here, not in the app). New redb managers go in this crate.
- `crates/zedis-ui` — reusable widgets (`ZedisCard`, `ZedisDialog`, `ZedisForm`, ...). **Separate crate**: it cannot use `crate::helpers::*` from the app. Platform-specific values (e.g. monospace font family) must be passed in by the caller.
- `zedis-cmd-builder` — offline helper tool (`make build-cmd`).

The app re-exports the sub-crates through thin shims, so call sites keep their old paths: `crate::connection::*` (→ zedis-connection), `crate::db::*` (→ zedis-db), `crate::error::*` (the app-level `Error` — a thin wrapper that transparently passes through `zedis_connection::error::Error` and `zedis_db::error::Error`), `crate::helpers::*` (mixes app-only helpers with re-exports from the sub-crates). Add new pure logic to zedis-core, new Redis operations to zedis-connection, new local-storage managers to zedis-db — not to the app crate. UI strings never move into the sub-crates (rust-i18n is per-crate; translations live only in the app).

## Architecture

**State (`src/states/`)** — the source of truth, GPUI entities.
- `ZedisGlobalStore` / `ZedisAppState` (`app.rs`): app-wide config + selected server + view prefs. Persisted to `zedis.toml` via `update_app_state_and_save(cx, "action", |state, _| …)` (async, debounced). Add a field + getter/setter here to persist a new preference.
- `ZedisServerState` (`server.rs` + `server/*.rs`): per-connection state — loaded keys, selected value, and all type-specific ops (`string/hash/list/set/zset/stream/json`). One-shot Redis ops go through `self.spawn(ServerTask::…, op, on_done, cx)` or `exec_stream_op`. `reset()` clears it on server switch; `clear_if_removed()` drops it when the active server is deleted.
- Events: `GlobalEvent` (notifications, `ServerSelected`, `ServerListUpdated`, `RouteChanged`) and `ServerEvent` (`ValueUpdated`, `KeySelected`, …) drive view updates via `cx.subscribe`. `ServerTask` is the async-task identity enum (add a variant + string mapping in `server/event.rs` for a new task).
- i18n: `t!("section.key")` (rust-i18n). Use the `i18n_<section>(cx, key)` helpers in `states/i18n.rs`, each **individually** re-exported from `states.rs` (add both the fn and the `pub use` line for a new section).

**Connection (`crates/zedis-connection`, re-exported as `crate::connection`)** — `config.rs` holds `RedisServer` (server list persisted to `redis-servers.toml`, secrets encrypted; read through the in-memory `SERVER_CONFIG_MAP` ArcSwap cache via `get_servers()` / `get_server()`). `manager/pool.rs` hands out pooled multiplexed clients and probes the three-state `AccessMode` (ReadWrite / SafeMode / StrictReadOnly — the source for the `Capability` matrix); `manager/client.rs` is the `RedisClient` ops; `manager/slots.rs` is cluster parsing + reshard planning. For **blocking commands** (`MONITOR`, `XREAD BLOCK`) use `open_single_connection(&server, db, /*use_cache=*/false)` — a dedicated connection so blocking never starves the shared pool.

**Views (`src/views/`)** — one GPUI view per route/panel. `content.rs` is the route switcher: server tool panels live in a `HashMap<ServerView, AnyView>` (one `tool_view()` match arm per panel — add new tool pages there), created on first visit and **dropped on route change** (`clear_views`), so in-view-only state does not survive navigation (persist via `ZedisAppState` if it must); only the editor suite (key tree / value editor / terminal) survives within a server session. `main.rs`'s `Zedis` root holds sidebar + workspace tabs (`Vec<ContentTab>`, one `ZedisContent` per tab; only the active tab reacts to global route/server broadcasts) + title bar, and registers global `.on_action` handlers.

**Long-running background loops** (live tail, MONITOR): mirror the Monitor pattern — a cancellable `gpui::Task` stored on the *view*, a dedicated connection in a `cx.background_spawn` loop, a `smol::channel` ferrying batches to a foreground drainer that updates state. Dropping the `Task` (stop toggle / key or server switch / view teardown) cancels the loop and its connection. Always key-guard appends so a stale batch after a key switch is discarded.

**Actions / keybindings**: gpui `#[derive(Action)]` enums in `helpers/action.rs` and `states/app.rs`; bound in `new_hot_keys()` (`helpers/action.rs`); dispatched via `.on_action(cx.listener(…))`, mostly on the `Zedis` root in `main.rs`.

## GPUI gotchas (learned the hard way)

- `gpui_component::list::ListItem` does **not** impl `InteractiveElement` — `.group()`, `.hover()`, `.tooltip()` won't compile on it. Wrap it in a stateful `div().id(...)`/`div().group(...)` and put the behavior there.
- `InputState` placeholder / default-value strings must **not** contain `\n`. The single-line wrapped-lines cache sizes on the placeholder but renders the actual text, causing a byte-index panic. Put multi-line guidance in an adjacent `Label`, keep the input string single-line.
- `IconName` (gpui-component) is a fixed enum — verify a variant exists before using it; custom SVGs live in `crate::assets::CustomIconName`. Theme-derived colors must be read before a `move` render closure (can't borrow `cx` inside).
- **Bold needs a concrete font family.** The default UI font (gpui-component's `Root` cascades `theme.font_family` = `.SystemUIFont`, i.e. `.AppleSystemUIFont` on macOS) resolves heavy weights poorly, and GPUI does **not** synthesize (fake) bold — so `.font_weight(FontWeight::BOLD)` alone often looks unchanged. Render the text in a real family via `.font_family(get_mono_font_family())` on the element (or on an ancestor — font-family cascades, which is why the key-tree badge looks bold: its `ListItem` sets the family for the whole row). The app bundles **JetBrains Mono** (Regular + Bold, registered in `main.rs`) and `get_mono_font_family()` returns it, so the mono face is deterministic across platforms. Mind a family's actual faces: JetBrains Mono ships only Regular/Bold here, so `EXTRA_BOLD`/`BLACK` collapse to `Bold`.

## Conventions

- UI components: **prefer `gpui-component`'s built-in components first**. Only when `gpui-component` has no suitable component, use the shared widgets in `crates/zedis-ui` (`ZedisCard`, `ZedisDialog`, `ZedisForm`, ...). Hand-rolling a one-off widget is a last resort.
- Maintain README parity: `README.md` and `README_zh.md` are both kept in sync when features change.
- Destructive Redis ops (`FLUSHALL`, `XGROUP DESTROY`, key/server delete, …) route through a confirm dialog (`ZedisDialog::new_alert` + `dialog_button_props`); production-tagged servers escalate the wording.
- Keep the dependency surface lean (e.g. fuzzy matching is hand-rolled, Lua highlighting registers tree-sitter manually) — prefer a small in-crate implementation over a new dependency for self-contained needs.
- Imports: bring items into scope with `use` declarations at the top of the file and refer to them by their short name. Do **not** write fully-qualified paths inline (e.g. `crate::views::ZedisEditor::new(...)`, `crate::states::ZedisGlobalStore`); add `use crate::views::ZedisEditor;` and call `ZedisEditor::new(...)`. The only acceptable inline-path exceptions are disambiguating two same-named types or a single use inside a macro where a `use` would be awkward.
