# AGENTS.md

This file is the auto-loaded workspace rule file for coding agents (within the 24 KB inline rule budget). Full historical notes and deep subsystem runbooks remain in `CLAUDE.md` — consult the specific line ranges indexed below whenever touching those areas.

Zedis is a native, GPU-accelerated Redis GUI client built in Rust with [GPUI](https://www.gpui.rs/) and `gpui-component`, both taken from crates.io through `gpui-kit`: GPUI is the `gpui-pre-*` snapshot family `gpui-kit` pins (renamed back to `gpui` / `gpui_platform` / `gpui_macros` in `[workspace.dependencies]`, so source keeps `gpui::…`), and the component library and its icon assets are reached as `gpui_kit::component::…` / `gpui_kit::assets::Assets`. Bump them together — `gpui`, `gpui_platform`, `gpui_macros`, `gpui-kit`, and `gpui_web` — and confirm one `gpui-pre` in `Cargo.lock`.

## Reference Index (`CLAUDE.md` & Docs)

When working on any of the following subsystems, read the corresponding lines in `CLAUDE.md` first:

- **Release, Gitee Mirror, Nightly & Windows SignPath**: `CLAUDE.md` lines 12–15, 30
- **Live Integration Tests (`crates/zedis-connection/tests/live/`) & Server Matrix**: `CLAUDE.md` lines 16, 19–21
- **Link Resilience, Heartbeat Pacing & `BUSY` Script Kill**: `CLAUDE.md` line 22
- **Troubleshooting Report (ADR 19) & Diagnostics Bundle**: `CLAUDE.md` lines 28–29
- **GPUI Fast Build (`gpui-fast` feature)**: `CLAUDE.md` line 34
- **Web Build (`zedis-web`, `zedis-bridge`, Auth, Accounts, RBAC, MCP, `/v1/watch`, SQL Stores, Page Data)**: `CLAUDE.md` line 35, lines 60–84
- **Nothing in the GUI Crate Speaks Redis (ADR 10)**: `CLAUDE.md` lines 85–101
- **Architecture, Server Feature Probe & Redis/Valkey Version Floors**: `CLAUDE.md` lines 103–121
- **GPUI Gotchas & UI Conventions**: `CLAUDE.md` lines 123–166
- **GPUI Skills (`.claude/skills/gpui-kit` & `.claude/skills/gpui-kit-design-guides`) & UI Tests**: `CLAUDE.md` lines 175–204
- **Domain Vocabulary, ADRs & Plans**: `CONTEXT.md`, `docs/adr/`, `docs/agents/domain.md`, `docs/agents/issue-tracker.md`, `docs/agents/optimization-plan.md`
- **Project Memory (`~/.gemini/projects/zedis/memory/`)**:
  - `MEMORY.md` — full index of historical decisions and verification recipes
  - `desktop-ui-click-test-recipe.md` — isolated `ZEDIS_CONFIG_DIR` + `osascript` clicks by PID + `screencapture` + accessibility tree
  - `verify-web-canvas-headless-chrome.md` — headless Chrome + DevTools protocol recipe for inspecting the web GPUI canvas
  - `linux-ui-repro-in-container.md` — Linux Xvfb + `xdotool` reproduction inside Apple `container`
  - `signpath-onboarding-status.md` — Windows SignPath status (`SIGNPATH_RELEASE_SIGNING` must stay unset until production cert is imported)
  - `gpui-kit-080-upgrade-pending.md`, `web-parity-decisions.md`, `cloud-managed-auth-deferred.md`, `refactor-plan.md`

---

## User Workflow & Session Rules (Must Follow)

- **Never commit or touch the git index (`never-commit-user-stages`)**: Never run `git commit`, `git add`, `git reset`, `git stash`, `git rm`, or `git mv` (use plain `rm` / `mv` for file deletions/moves). The user stages and commits changes themselves. When finishing a task, run the required gates, report whether the index holds anything (`git diff --cached --stat`, noting if a staged file was edited again as `MM`), and **provide a commit message** for the user.
- **Commit message style (`commit-message-style`)**: Draft a **single short conventional-commit line in English** naming roughly which components or areas changed (e.g. `feat(ui): ...` or `fix(connection): ...`). No multi-paragraph or bulleted body unless explicitly asked.
- **No in-place A/B reverts (`no-in-place-ab-reverts`)**: Because the user stages files mid-session, never temporarily revert tracked files (`Cargo.toml`, `Cargo.lock`, source files) in place in the working tree. For A/B comparisons or verifying that a new test fails without a fix, use a separate `git worktree` or copy with its own `CARGO_TARGET_DIR`.
- **Ask before rolling back dependencies (`ask-before-rolling-back-deps`)**: Never downgrade a dependency or regenerate `Cargo.lock` downward on your own to fix a build/MSRV issue. Diagnose with `--locked`, restore `Cargo.lock` if touched, present the options (move manifest up vs lock down), and ask the user.
- **Ask before triggering CI (`ask-before-triggering-ci`)**: Never run `gh workflow run`, re-run a workflow, dispatch `publish.yml`, or run `make nightly` without asking the user first and getting an explicit yes for that specific run. Read-only `gh run list/view` calls are fine.
- **Local vs CI verification (`ci-vs-local-verification`)**: Always run the local `make` gates (`make fmt`, `make lint`, `make test`, plus `make check-web` / `make lint-fast` when applicable) before reporting done. Leave cross-OS builds, Redis/Valkey/DB version matrices, and smoke lanes to CI; do not poll/babysit CI runs in a loop.
- **Local Redis test data (`local-redis-test-data`)**: Treat local `127.0.0.1:6379` db 0 (~100k demo keys) as **strictly read-only** unless told otherwise. For any test that writes, flushes, or changes config/ACL, spin up a temporary `redis-server` on port `46379` (and shut it down afterwards). Never kill port `16380` or its neighbours (used by the user's Apple `container` services).
- **Keep expired keys in the key tree (`keep-expired-keys-in-tree`)**: When opening a key discovers it has expired (`TTL -2`), keep its row in the key tree — do not auto-remove it.

---

## Required Gates & Commands

- **Build / typecheck**: `cargo check`
- **Format**: **run `make fmt` (`cargo fmt`) after every code change**, before `make lint`.
- **Lint (REQUIRED GATE)**: **run `make lint` once as the final step before completing any work** (and after every change). It runs `cargo fmt --check` + `typos` + `cargo clippy --all-targets --all -- --deny=warnings`. Never report work as done until `make lint` passes clean (`cargo clippy` alone skips `typos`).
- **Fast engine gate**: `make lint-fast` (clippy + app crate tests under `--features gpui-fast`).
- **Web compile gate**: `make check-web` compiles every crate for `wasm32-unknown-unknown` (nightly, from `zedis-web/`). Run it beside `make lint` whenever touching any file with a target gate or anything under `crates/`. Portable files must import `Instant` / `SystemTime` from `web_time` (`std::time::Instant::now()` panics in wasm at runtime).
- **Tests**: `make test` (`cargo test --workspace`). Run a subset via e.g. `cargo test -p zedis-core fuzzy` or `cargo test config::`.
  - **CRITICAL — Test config isolation**: **A test never reads the user's configuration, and never makes the OS ask for a password** (unsigned test binaries reading real server secrets trigger OS keychain prompts). Tests touching config must redirect it first (`override_config_dir`; in the app crate `test_support::isolate_config`, which `test_support::init_ui` calls). Any test or debug binary run by hand outside `make test` must set `ZEDIS_CONFIG_DIR="$(mktemp -d)"` (or `RUST_ENV=dev`).
- **Compatibility & Read-Only command gates** (part of `make test`):
  - `tests/command_floors.rs` checks every `cmd("…")` in `zedis-connection` against `assets/commands.json`; anything newer than Redis 6.2 must be listed in `GATED` with its `floors::` constant, `ServerCommand` probe, or best-effort note.
  - `crates/zedis-connection/tests/read_only_commands.rs` fails unless every command sent in `zedis-connection` is either in `is_read_only_command` (`KEYSPACE_READS` / server-read / module-read) or listed in `WRITES`.
- **Layering gate**: `make check-layering` (`tests/view_layering.rs`, part of `make test`) verifies zero Redis command construction/execution in `src/`.
- **Locale gate**: `make check-locales` (`tests/locale_keys.rs`, part of `make test`).
- **Live integration tests**: `make it-up` (or `REDIS_IMAGE=redis:7.2 make it-up`), `make it`, `make it-down`. See `CLAUDE.md` lines 19–21 for topology credentials (`protected_server` / `sentinel_server`), mTLS/SSH ports, Valkey/Dragonfly/Garnet rules, and multi-version verification.
- **Dependency audit**: `make deny` (`cargo deny check`; advisories ignored for real vulnerabilities go into both `deny.toml` and `.cargo/audit.toml`).
- **Benchmarks**: `make bench` (`crates/zedis-core/benches/hot_paths.rs`).
- **Toolchain**: Rust **1.98.1**, edition 2024 (`rust-toolchain.toml` + `.github/workflows/` `RUST_TOOLCHAIN` env must move together).

---

## Code Quality & Lint Rules

- **`unwrap_used = "deny"` crate-wide including tests** — use `.expect("…")` or proper matching in tests, never `.unwrap()`.
- **Avoid `#[allow(clippy::…)]` and `#[allow(dead_code)]`.** Fix the underlying smell (e.g. group `too_many_arguments` into a struct; delete or wire up dead code — never park dead code behind `#[allow(dead_code)]`).
- **Imports**: bring items into scope with `use` declarations at the top of the file and refer to them by short name. Do **not** write inline fully-qualified paths (`crate::views::…`, `crate::states::…`) except to disambiguate same-named types or inside macros.
- **Lib crate constructors**: clippy `should_implement_trait` fires on `pub fn from_str` in lib crates — name such constructors `from_name`.
- **Rust dependencies**: before planning/adding any crate, run `cargo info <crate>` (or `cargo search <crate>`) to check the current latest version on crates.io, and use `cargo add <crate>`. Keep the dependency surface lean.

---

## Build-Time Locale Parity Gate (Bites Immediately)

`build.rs` and `tests/locale_keys.rs` enforce that **all 8 locales** (`en, zh, de, es, fr, ja, pt, ru` in `locales/<lang>.toml`) have the exact same key set as `locales/en.toml`, and that no orphan keys exist.
- Adding or removing any UI string requires editing **all 8 files** or `cargo check` panics.
- Run `make check-locales` after editing locales.
- Use `t!("section.key")` via the `i18n_<section>(cx, key)` helpers in `src/states/i18n.rs`, each individually re-exported from `src/states.rs`.

---

## Workspace Layout & Layering Rules

- `zedis-gui` (root crate: `src/lib.rs` + thin `src/main.rs`) — desktop GUI & state (`src/views/`, `src/states/`, `src/helpers/`).
- `crates/zedis-core` — GUI-free pure logic (`Capability` matrix, `ServerFeatures`, fuzzy match, hex/csv/diff, JSONPath, `key_segments`, TTL helpers, `tally`). No `gpui`, no i18n.
- `crates/zedis-connection` — Redis layer: pooled clients (`manager/{client,pool,slots}.rs`), `RedisServer` config, SSH tunnels, TLS (`tls_connection.rs`), feature probes (`probe.rs`), version floors (`floors.rs`), and typed feature operations. No `gpui`, no i18n.
- `crates/zedis-db` — local storage (`redb` managers: tags, favorites, history, trash, scripts, proto descriptors, `backup.rs`, `page_data.rs`). Every value struct carries `#[serde(default)]`, loaders skip unreadable rows rather than deleting them, new per-key stores join `move_local_records` on key rename, and every `TableDefinition` must be opened in `ensure_schema`.
- `crates/zedis-ui` — target-agnostic reusable widgets (`ZedisCard`, `ZedisDialog`, `ZedisForm`, `ZedisTextTable`). Cannot access `crate::helpers::*`; fonts and translated strings are passed in by callers.
- `zedis-bridge` — HTTP server (`axum`) for the web build. Depends **only** on `zedis-connection` + `zedis-core` (never `zedis-gui`).
- `zedis-web` — browser wasm entry point (`entry`, `transport`, `assets`).

### Nothing in the GUI Crate Speaks Redis (ADR 10)
- **No file under `src/` builds a command (`cmd("`), runs one (`.query_async` / `.exec_async` / `pipe()`), takes a connection, or names `redis::`, `RedisAsyncConn`, `BridgeQuery`, or `BridgePipeline`.**
- Every Redis operation lives in `crates/zedis-connection`, takes `&ServerDb` (or `&ClusterNode` for node-specific cluster commands in `cluster_ops.rs`), parses replies via `zedis-connection/src/reply.rs`, returns a typed struct, and gets a live test in `crates/zedis-connection/tests/live/` in the same change.
- Server pushes / blocking reads use dedicated connection types (`ChannelSubscription`, `open_monitor_feeds`, `StreamTail`, `TerminalSession`).

### Desktop First: One Codebase, Two Targets (ADR 9)
- **A change made for the browser must not change what the desktop does.**
- **Dependency direction**: `zedis-core` ← `zedis-connection` / `zedis-db` / `zedis-ui` ← `zedis-gui` ← `zedis-web`, and `zedis-bridge` → `zedis-connection` + `zedis-core`.
- **Target gates**:
  - `zedis-ui` and `zedis-bridge`: 0 wasm gates.
  - Use attribute form (`#[cfg(target_family = "wasm")]` / `#[cfg(not(target_family = "wasm"))]`), never `cfg!(...)` in views.
  - Gate at the widest level: **module > item > statement**. If a function needs two behaviors, write two functions with the same signature under opposite gates (desktop first), never interleaved statements.
  - Ask `ServerView::in_this_build()` for panels and `helpers/this_build.rs` (`saves_files()`, `opens_files()`, `has_file_dialogs()`, `reads_clipboard_on_demand()`, `value_write_limit()`, `kills_runaway_scripts()`, `has_ai_assistant()`) for capabilities.
  - Never hard-code timer intervals next to a timer; define them in `src/helpers/pacing.rs`.
  - Every user-facing date/time goes through `src/helpers/datetime.rs` (never `Local::now().format(…)` in a view).
  - Never use root-absolute URLs (`/…`) in `zedis-web/www` or wasm (`--base-path` compatibility).

### Version Floors & Server Capabilities (ADR 1 & ADR 2)
- **Never compare raw version strings.** Every Redis/Valkey version gate is a `Floor { redis, valkey }` constant in `crates/zedis-connection/src/floors.rs` with both flavors researched, checked via `supports(floors::X)`.
- **Command availability** on proxies/managed clouds is probed via `ServerCommand` in `zedis-core/src/features.rs` and `zedis-connection/src/probe.rs` (read-only probes only!), mapped into `ServerView::required_commands()` and `Capability::required_commands()`.

---

## GPUI & UI Gotchas (Must Follow)

Before doing GPUI or UI design work, load the vendored skills in `.claude/skills/gpui-kit/SKILL.md` and `.claude/skills/gpui-kit-design-guides/SKILL.md` (note: this repo keeps `gpui::…` rather than `use gpui_kit::*;`, and `#[gpui::test]` rather than `#[gpui_kit::test]`).

1. **A dialog body that holds an `Input` must be a view entity** (`.child(move || some_view.clone())`), never inline elements rebuilt each frame (which corrupts inputs when paired with `flex_wrap`).
2. **`ZedisDialog` footer & chaining**: non-alert dialogs render no buttons unless `.ok_text(…)` or `.footer_child(…)` is set on `ZedisDialog`. An `on_ok` that opens *another* dialog must call `window.close_dialog(cx)`, open the next dialog, and return `false` (checked by `tests/dialog_chain.rs`). Destructive confirms use `.danger()` or `views/danger_confirm.rs` (`confirm_dangerous_command` / `confirm_delete_key`), and non-delete confirms must set their own `.ok_text(…)`.
3. **`Scrollable` + `max_h` silently clips instead of scrolling**: always give scrollable viewports a definite `.h(...)`, never `max_h`.
4. **A bare `div()` is `display: Block`**: any wrapper between a sized ancestor and a `uniform_list` / `h_full` / `flex_1` child must be a flex container (`v_flex().flex_1().min_h_0()`) or have an explicit `.h(...)`.
5. **`Button.loading(true)` requires `.icon(…)`**: the spinner replaces the icon; a label-only button shows no spinner.
6. **`Label` ignores parent `.text_color()`**: set `.text_color(...)` directly on each `Label`.
7. **Bold needs a concrete font family**: GPUI does not synthesize bold on `.SystemUIFont`; pair `FontWeight::BOLD` with `.font_family(get_mono_font_family())`.
8. **`InputState` placeholders/defaults must not contain `\n`** (causes byte-index panic).
9. **Grid cards & `Input`**: pin `Input` height with `.h(px(32.))` + `overflow_hidden` wrapper inside CSS-grid cards; do not use `NumberInput` in grid cards.
10. **Large text in `ZedisBytesEditor`**: soft-wrap is disabled past `SOFT_WRAP_MAX_BYTES` (512 kB) *before* setting text, and highlighting is capped at `HIGHLIGHT_MAX_BYTES` / `MAX_INLINE_VALUE_SIZE`.
11. **Focus hand-off**: closing an overlay/panel/palette must never call `window.blur`; hand focus back via `contains_focused` / `hand_focus_on` / `bind_to_tab` / `bind_to_page`. Focus a dialog's input *after* `.open(window, cx)`.
12. **UI integration tests**: start with `test_support::init_ui(cx)`, place in the view's own file (`#[cfg(test)] mod tests`), share control ID constants between view and test, and run `cx.update_window` → `cx.run_until_parked()` → `cx.update_window` (`window.render_frame(cx)` + assert) for deferred dialog/focus updates.
13. **Documentation parity**: keep `README.md` and `README_zh.md` (and `docs/*.md` + `docs/*_zh.md`) in sync when features or network paths change.
