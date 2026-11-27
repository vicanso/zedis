# AGENTS.md

This file is the auto-loaded workspace rule file for coding agents (within the 24 KB inline rule budget). Full historical notes and deep subsystem runbooks remain in `CLAUDE.md` — consult the specific line ranges indexed below whenever touching those areas.

Zedis is a native, GPU-accelerated Redis GUI client built in Rust with [GPUI](https://www.gpui.rs/) and `gpui-component` through `gpui-kit 0.11.0`: GPUI is the `gpui-pre-* 0.3.14` snapshot family `gpui-kit` pins (renamed back to `gpui` / `gpui_platform` / `gpui_macros` in `[workspace.dependencies]`, so source keeps `gpui::…`), `gpui-fast 0.1.19` is the optional Fast engine (`extern crate gpui_kit as gpui;` in `src/lib.rs` and `crates/zedis-ui/src/lib.rs`), and the component library, base primitives, and icon assets are reached as `gpui_kit::component::…`, `gpui_kit::base::…`, and `gpui_kit::assets::Assets`. Linux enables `gpui-kit/x11` by default alongside `wayland` (omit default features for Wayland-only). Bump `gpui`, `gpui_platform`, `gpui_macros`, `gpui-kit`, and `gpui_web` together and confirm one `gpui-pre` in `Cargo.lock`.

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
- **GPUI Gotchas & UI Conventions**: `CLAUDE.md` lines 123–170
- **GPUI Skills (`.claude/skills/gpui-kit` & `.claude/skills/gpui-kit-design-guides`) & UI Tests**: `CLAUDE.md` lines 180–208
- **UI Design Standards (NSH-aligned)**: `docs/agents/ui-design-standards.md`
- **Domain Vocabulary, ADRs & Plans**: `CONTEXT.md`, `docs/adr/`, `docs/agents/domain.md`, `docs/agents/issue-tracker.md`, `docs/agents/optimization-plan.md`
- **Project Memory (`~/.gemini/projects/zedis/memory/`)**:
  - `MEMORY.md` — full index of historical decisions and verification recipes
  - `desktop-ui-click-test-recipe.md` — isolated `ZEDIS_CONFIG_DIR` + `osascript` clicks by PID + `screencapture` + AX tree
  - `verify-web-canvas-headless-chrome.md` — headless Chrome + DevTools protocol recipe for web GPUI canvas
  - `linux-ui-repro-in-container.md` — Linux Xvfb + `xdotool` reproduction inside Apple `container`
  - `signpath-onboarding-status.md` — Windows SignPath (`SIGNPATH_RELEASE_SIGNING` stays unset until prod cert lands)
  - `gpui-kit-080-upgrade-pending.md` (superseded by `0.11.0`), `web-parity-decisions.md`, `cloud-managed-auth-deferred.md`, `refactor-plan.md`

---

## User Workflow & Session Rules (Must Follow)

- **Never commit or touch the git index (`never-commit-user-stages`)**: Never run `git commit`, `git add`, `git reset`, `git stash`, `git rm`, or `git mv` (use plain `rm` / `mv`). The user stages and commits changes themselves. When finishing, run required gates, report `git diff --cached --stat` (noting `MM` if a staged file was edited), and **provide a commit message**.
- **Commit message style (`commit-message-style`)**: A **single short conventional-commit line in English** naming changed areas (e.g. `feat(ui): ...`). No multi-paragraph or bulleted body unless asked.
- **No in-place A/B reverts (`no-in-place-ab-reverts`)**: Never temporarily revert tracked files in place. For A/B checks or test-failure proofs, use a separate `git worktree` or copy with its own `CARGO_TARGET_DIR`.
- **Ask before rolling back deps (`ask-before-rolling-back-deps`)**: Never downgrade a dependency or regenerate `Cargo.lock` downward on your own. Diagnose with `--locked`, restore `Cargo.lock` if touched, present options (manifest up vs lock down), and ask.
- **Ask before triggering CI (`ask-before-triggering-ci`)**: Never run `gh workflow run`, re-run a workflow, dispatch `publish.yml`, or run `make nightly` without an explicit yes. Read-only `gh run list/view` calls are fine.
- **Local vs CI verification (`ci-vs-local-verification`)**: Always run local gates (`make fmt`, `make lint`, `make test`, plus `make check-web` / `make lint-fast` when applicable) before done. Leave cross-OS, DB matrices, and smoke lanes to CI; do not poll CI loops.
- **Local Redis test data (`local-redis-test-data`)**: Treat local `127.0.0.1:6379` db 0 (~100k demo keys) as **strictly read-only**. Run write/flush/ACL tests on a temp `redis-server` on port `46379` (shut down afterwards). Never kill port `16380` or neighbours (Apple `container` services).
- **Keep expired keys in tree (`keep-expired-keys-in-tree`)**: Opening a key that returns `TTL -2` keeps its row in the key tree.

---

## Required Gates & Commands

- **Build / typecheck**: `cargo check`
- **Format**: **run `make fmt` (`cargo fmt`) after every code change**, before `make lint`.
- **Lint (REQUIRED GATE)**: **run `make lint` once as the final step before completing any work** (and after every change). It runs `cargo fmt --check` + `typos` + `cargo clippy --all-targets --all -- --deny=warnings`. Never report work as done until `make lint` passes clean (`cargo clippy` alone skips `typos`).
- **Fast engine gate**: `make lint-fast` (clippy + app crate tests under `--features gpui-fast`).
- **Web compile gate**: `make check-web` compiles every crate for `wasm32-unknown-unknown` (nightly, from `zedis-web/`). Run it beside `make lint` whenever touching any file with a target gate or anything under `crates/`. Portable files must import `Instant` / `SystemTime` from `web_time` (`std::time::Instant::now()` panics in wasm at runtime).
- **Tests**: `make test` (`cargo test --workspace`; subset: `cargo test -p zedis-core fuzzy`, `cargo test config::`).
  - **CRITICAL — Test config isolation**: Tests never read real user config or trigger OS keychain prompts. Isolate first (`override_config_dir`; `test_support::isolate_config` / `test_support::init_ui`). Manual runs outside `make test` must set `ZEDIS_CONFIG_DIR="$(mktemp -d)"` (or `RUST_ENV=dev`).
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

## Code Quality, Modularity & Lint Rules

- **`unwrap_used = "deny"` crate-wide including tests** — use `.expect("…")` or proper matching in tests, never `.unwrap()`.
- **Avoid `#[allow(clippy::…)]` and `#[allow(dead_code)]`.** Fix the underlying smell (e.g. group `too_many_arguments` into a struct; delete or wire up dead code — never park dead code behind `#[allow(dead_code)]`).
- **Imports**: bring items into scope with `use` declarations at the top of the file and refer to them by short name. Do **not** write inline fully-qualified paths (`crate::views::…`, `crate::states::…`) except to disambiguate same-named types or inside macros.
- **Lib crate constructors**: clippy `should_implement_trait` fires on `pub fn from_str` in lib crates — name such constructors `from_name`.
- **Rust dependencies**: before planning/adding any crate, run `cargo info <crate>` (or `cargo search <crate>`) to check the current latest version on crates.io, and use `cargo add <crate>`. Keep the dependency surface lean.

### File & Function Size Limits (No Monolith Bloat)
- **Never pile new features into entry shells or oversized files**: keep crate/shell entry files (`src/lib.rs`, `src/root.rs`, `src/views.rs`, `src/states.rs`, `zedis-bridge/src/main.rs`, and crate `lib.rs` files) strictly for app wiring, lifecycle, and re-exports. Any new standalone panel, dialog view entity, sub-view, state slice, or bridge route group must live in its own submodule file using Rust 2018+ `foo.rs` + `foo/*.rs` layout (do **not** introduce new `mod.rs` files), e.g. `src/views/<feature>/*.rs`, `src/states/server/*.rs`, `zedis-bridge/src/api/*.rs`.
- **Line-count red lines**:
  - **Module files**: target **≤ 800 lines**, hard ceiling **1,500 lines** of production code (excluding `#[cfg(test)]` modules) for every new or split module.
  - **Functions**: aim for **≤ 120 lines** per function/method; decompose sprawling GPUI `render` trees, parsers, and state handlers into focused helpers or child view components.
- **Progressive splitting near the ceiling**: when touching files approaching the 1,500-line ceiling (e.g. `src/views/search_manager{,/render}.rs`, `src/views/config_editor.rs`, `src/views/geo_map.rs`, `src/states/server.rs`, `src/states/server/value.rs`, `src/views/function_editor.rs`, `src/views/servers.rs`), **never append new multi-item blocks, dialogs, or sub-views in place**. Extract the new or touched cohesive block into a `<module>/*.rs` submodule instead (keeping structural moves cleanly reviewable and preserving shared UI test control IDs).

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
- `crates/zedis-ui` — target-agnostic reusable widgets (`ZedisCard`, `ZedisDialog`, `ZedisForm`, `ZedisTextTable`, `ZedisDivider`, `panel_header`, `stat_chip`, …). Cannot access `crate::helpers::*`; callers pass fonts and translated strings.
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

## GPUI-Kit UI Design & Component Standards (Must Follow)

Before any UI work, load `.claude/skills/gpui-kit/SKILL.md` + `.claude/skills/gpui-kit-design-guides/SKILL.md` (Zedis keeps `gpui::…` / `#[gpui::test]`) and **`docs/agents/ui-design-standards.md`** (component selection matrix, `0.11.0` rules, tokens, finishing checklist, `2026-11-27` debt baseline — fix debt you touch, never add new debt). Desktop and web share one desktop idiom.

1. **Use `gpui-kit` first, never restyle per screen**: prefer `gpui_kit::component::*` (`Button`/`ButtonGroup`, `Input`+`Input::list`, `Editor`+`GutterStyle`/folding, `Select`, `DataTable`/`List`/`DescriptionList`, `Popover`/`PopupMenu`/`DropdownMenu`/`Tooltip`, `Badge`/`Tag`, `Kbd` [see wasm caveat], `Empty`, `Skeleton`/`Spinner`/`Progress`, `Alert`, `Diff`, `Separator`, `Switch`/`Checkbox`/`RadioGroup`, `Resizable`, `Sheet`, `Calendar`/`DatePicker`/`TimeField` on `jiff`) and `gpui_kit::base::SelectableText` before `crates/zedis-ui` (`ZedisCard`, `ZedisDialog`, `ZedisForm`, `ZedisTextTable`, `ZedisDivider`, `panel_header`, `stat_chip`) or `src/components/` (`KeyTypeBadge`, `ZedisKvTable`). Widgets take `variant`, one `Sizable` size, states (`selected`/`disabled`/`loading`), and slots; callers pass layout only. Missing look → variant/helper or `src/helpers/color.rs` fn. Extract patterns on second use. Never build controls from bare `div().on_click` (virtualized rows excepted, which owe hover/selected/keyboard/`ElementId`).
2. **Hierarchy is a budget**: one focal point per surface; hierarchy via size, weight, spacing and hairlines before color or boxes; no card in a card; neutral badges; semantic colors and `Alert` only for real meaning; shadows only on floating layers.
3. **Tokens & scale**: colors only from `cx.theme()` (no hex/`rgb`/`hsla` in views; palettes centralized and checked in light/dark). Radii from `theme().radius` (controls/popups) / `radius_lg` (containers), concentric, `rounded_full` only for circles/pills. ≤3–4 text sizes and ≤2 weights per screen, floor `text_xs`; UI font for prose, `get_mono_font_family()` for keys/values/commands/metrics/shortcuts; `muted_foreground` for secondary text. Spacing via rem helpers (`gap_1` < `gap_2` < `gap_3` < `gap_4` < `gap_6`); **no `px(...)`** except audited hairlines, `uniform_list` row heights, pane widths, and canvas/chart geometry.
4. **Alignment**: sibling bands share spine `px_3`; padding belongs to the component, gaps to the parent (no doubled insets); rows keep fixed lanes; `justify_between` only for opposite edges; one hairline per boundary; scrollbars at the scrolling edge.
5. **Controls & states**: one `Sizable` step per toolbar/header/filter row (usually `.small()`); square icon buttons with `.tooltip(...)` + domain `ElementId`; `Button::aria_expanded(open)` on disclosures; one popup surface (`Popover`/`PopupMenu`/`DropdownMenu`/`Select`, never hand-positioned `div`s). ≤1 `.primary()` per decision surface (`Enter` commit); toolbar actions `.outline()`/`.ghost()`; destructive via `.danger()`/`danger_confirm.rs`; `Link` only for external URLs. Hover is a quiet wash (`list_hover`/`table_hover`), weaker than selected, with no layout shift; selection is a surface fill (`list_active`/`.selected(true)`), never a `border_l` stripe; open triggers stay selected; disabled shows no hover; loading keeps size. Default cursor on controls/rows, hand only on links. Never gate the sole path to an action behind hover.
6. **Overlays & feedback**: smallest surface first (tooltip → popover → menu → notification → dialog → sheet); tooltips add missing meaning; no stacked overlays; no success toast when result is visible.
7. **Data views**: virtualize growing collections keyed by domain id; keep the key column stable, preserve selection across filtering; read-only `Editor` (`GutterStyle`, folding, `active_line_only_when_focused`, snap-back on `InputEvent::Change`) + `ZedisJsonTree` (`Text | Tree`) for structured values; `gpui_kit::base::SelectableText` for copyable plain prose (never `Label` dumps); `Diff` for diffs, `Progress` for bars, `DescriptionList` for summaries; design `Empty` / Loading / Error / Truncated states.
8. **Motion explains change**: prefer gpui-kit motion (`Presence` closed → `Absent`); hand-rolled `with_animation` uses `src/helpers/motion.rs` (120/180/240/360 ms), animates color/opacity only, keys `ElementId` by state, never on ticking cells, and honors `cx.reduce_motion()`. New panels list a motion inventory.
9. **Copy (all 8 locales)**: nouns for places, verbs for commands, no `Management`/`Module` wrappers; sentence case, no period on labels, `…` on dialog commands; confirms `Delete "key"?` + `Cancel` + result verb (no "Are you sure", no "successfully"); errors give cause + next step.
10. **Keyboard & finishing audit (blocking)**: all pointer actions work by keyboard (Tab, arrows in groups, Enter, Escape closes topmost and restores focus); focus rings unclipped; status never by color alone. Run the finishing checklist in **light & dark**, minimum width, and enlarged UI font, add `#[gpui::test]`, and report if visual inspection was unavailable.

---

## GPUI Engine Gotchas (Must Follow)

1. **`gpui-kit 0.11.0` / `gpui-pre 0.3.14` breaking APIs**: `Element: 'static + Sized` has blanket `impl<T: Element> IntoElement for T` — never write manual `impl IntoElement` on a custom `Element` (`E0119`; `#[derive(IntoElement)]` on `RenderOnce` stays valid). `Path` is non-generic; `FillOptions` drops `tolerance`; `WindowBounds::centered` takes `(Option<DisplayId>, size, cx)`; `Calendar`/`DatePicker`/`TimeField` use `jiff::civil` (never `chrono`). `Kbd::format` uses compile-time `#[cfg(target_os = "macos")]` (keep `humanize_keystroke` on `wasm32`).
2. **Dialog `Input`s must be a view entity** (`.child(move || view.clone())`), never inline elements rebuilt each frame (corrupts inputs with `flex_wrap`).
3. **`ZedisDialog` footer & chaining**: non-alert dialogs need `.ok_text(…)` or `.footer_child(…)`. An `on_ok` opening another dialog must call `window.close_dialog(cx)`, open the next, and return `false` (`tests/dialog_chain.rs`). Destructive confirms use `.danger()` or `views/danger_confirm.rs`.
4. **Scrollbars & containers**: `Scrollable` + `max_h` clips silently — give `.overflow_y_scrollbar()` a definite `.h(...)` (use `ScrollbarPlacement` for left/top bars; adaptive `max_h` popups use native `overflow_y_scroll` + `Scrollbar`). A bare `div()` is `display: Block` — wrap `uniform_list`/`h_full`/`flex_1` children in `v_flex().flex_1().min_h_0()` or set `.h(...)`.
5. **Controls & text**: `Button.loading(true)` needs `.icon(…)`; `Label` ignores parent `.text_color()`; `FontWeight::BOLD` needs `.font_family(get_mono_font_family())`; `InputState` placeholders/defaults must not contain `\n`; inside CSS-grid cards pin `Input` `.h(px(32.))` + `overflow_hidden` (no `NumberInput`); `Input::list(&ListState<D>)` handles simple single-line suggestions (`Editor` only for semantic LSP).
6. **Large text in `ZedisBytesEditor`**: disable soft-wrap past `SOFT_WRAP_MAX_BYTES` (512 kB) *before* setting text; highlight capped at `HIGHLIGHT_MAX_BYTES` / `MAX_INLINE_VALUE_SIZE`.
7. **Focus hand-off**: never call `window.blur` on close; restore via `contains_focused` / `hand_focus_on` / `bind_to_tab` / `bind_to_page`. Focus dialog inputs *after* `.open(window, cx)`.
8. **UI integration tests (`TESTING.md`)**: call `test_support::init_ui(cx)` (sets scratch config + reduces motion), place in `#[cfg(test)] mod tests`, place `.test_support()` before `.track_focus(&handle)` on custom controls (`Input`/`Textarea`/`Editor` project component IDs to native editor), share control ID constants, and use `update_window` → `run_until_parked()` → `update_window` (`render_frame` + assert).
9. **Documentation parity**: keep `README.md` / `README_zh.md` and `docs/*.md` / `docs/*_zh.md` in sync.
