# Optimization & Feature Enhancement Plan

Drawn up 2026-10-31 from an audit of the codebase, documentation (`docs/FEATURES.md`, `docs/WEB.md`, ADRs), and open GitHub issues. Each item below is self-contained and can be implemented, verified, and reviewed as an independent step.

## Rules Every Step Follows

- **Desktop first (ADR 9)**: A change made for the browser must not alter desktop behavior; any step touching a target gate (`#[cfg(target_family = "wasm")]` / `#[cfg(not(target_family = "wasm"))]`) must pass both `make lint` and `make check-web`.
- **Nothing in `src/` speaks Redis (ADR 10)**: Any new Redis operation lives in `crates/zedis-connection`, takes `&ServerDb`, and gets a live test under `crates/zedis-connection/tests/live/`.
- **Locale parity**: Any UI string addition or removal updates all 8 locale files (`locales/{en,zh,de,es,fr,ja,pt,ru}.toml`) and passes `make check-locales`.
- **Gates**: `make fmt`, `make lint`, `make test` (plus `make check-web` / `make lint-fast` / `make deny` when applicable).
- **Never commit**: Offer a single-line conventional commit message at the end of each step.

---

## 1. Desktop UX & Key Tree Edge Cases

### 1.1 Fix Filtered Key Tree Refresh Blanking & Lazy Folder Reset (Issue #148)
- **Files**:
  - `src/states/server/scan.rs` (`KeyScan::extend`, `KeyScan::begin_refresh_scan`, `KeyScan::reset`)
  - `src/states/server/key.rs` (`scan_next`, `reload_prefix`)
- **Problem**:
  1. In `KeyScan::extend` (`src/states/server/scan.rs`), `keys_superseded_by_next_batch` is consumed on the very first `SCAN` batch that returns after a refresh (`⌘R` / `Ctrl+R`). When a filter is active (sparse glob/prefix keyword, `regex_keyword`, or `type_filter`) and the first `SCAN` batch yields `0` matching keys (`keys.is_empty()`) while `scan_completed == false` (more cursors remain), `extend` still clears `self.keys` and calls `renew_tree_id()`. This blanks the key tree mid-scan or leaves it empty until the user manually clicks *Scan more*.
  2. `KeyScan::begin_refresh_scan` calls `reset(true)`, which unconditionally clears `loaded_prefixes`. Any folder whose keys were lazily populated via `scan_prefix` on folder expand loses those keys on a top-level refresh if the first top-level scan page does not include them.
- **Proposed Fix**:
  - Only consume `keys_superseded_by_next_batch` in `KeyScan::extend` when `!keys.is_empty() || self.scan_completed`. While `keys_superseded_by_next_batch` remains `true` and `keys.is_empty() && !self.scan_completed`, keep the existing `self.keys` visible until a non-empty batch arrives or the scan finishes empty.
  - Preserve or re-trigger `loaded_prefixes` for expanded prefixes across `begin_refresh_scan` so expanded lazy folders do not collapse or lose children on refresh.
  - Add unit tests in `src/states/server/scan.rs` covering filtered multi-batch refreshes where batch 1 is empty and batch 2 is non-empty.

---

## 2. Web Build (`zedis-web` + `zedis-bridge`) Parity Enhancements

### 2.1 Single-Key Copy & Compare "Copy to target" in Browser
- **Files**:
  - `src/views/copy_key_dialog.rs`
  - `src/views/compare_window.rs`
  - `src/views/editor/toolbar.rs`
- **Problem**:
  - `open_compare` already works in the web build via `ZedisDialog`, and `zedis_connection::copy_key` (`crates/zedis-connection/src/logical_copy.rs`) operates entirely through `&ServerDb` (the bridge dials both source and target servers and enforces read-only locks, RBAC, and audit logging). However, single-key copy (`copy_key_dialog.rs`) and the *Copy to target* action in `compare_window.rs` are currently gated out of the browser build.
- **Proposed Fix**:
  - Enable single-key copy and Compare's *Copy to target* hand-off in wasm using `ZedisDialog`, matching the existing `open_compare` pattern.
  - Update `docs/WEB.md` and `docs/WEB_zh.md` feature parity tables.

### 2.2 Readable Key Import (JSON / CSV) in Browser
- **Files**:
  - `src/views/key_export.rs`
  - `src/views/file_picker.rs`
  - `crates/zedis-connection/src/readable_import.rs`
- **Problem**:
  - The browser build already supports exporting keys to JSON/CSV downloads (`src/views/key_export.rs`) and uploading files via `views::open_file` (`set_web_upload`), and `zedis_connection::readable_import` parses JSON/CSV in memory and writes via `&ServerDb`. Key import, however, is currently desktop-only.
- **Proposed Fix**:
  - Expose JSON/CSV readable key import in the web build via `views::open_file` with a browser upload size cap (`this_build::value_write_limit()`).
  - Update `docs/WEB.md` and `docs/WEB_zh.md`.

### 2.3 Runaway Script Kill (`SCRIPT KILL` / `FUNCTION KILL`) in Browser
- **Files**:
  - `src/helpers/this_build.rs` (`kills_runaway_scripts`)
  - `zedis-bridge/src/api.rs`
  - `zedis-web/src/transport.rs`
- **Problem**:
  - `kills_runaway_scripts()` in `src/helpers/this_build.rs` returns `false` on wasm because killing a runaway script requires dialing a fresh connection while the primary connection is blocked or returning `-BUSY`.
- **Proposed Fix**:
  - Route `kill_running_script` through `zedis-bridge` (which dials a fresh connection to the target server on the backend and enforces write permissions) so browser users can recover from `BUSY` scripts from the warning banner.

---

## 3. MCP Server (`zedis-bridge/src/mcp.rs`) Enhancements

### 3.1 Add `troubleshooting_report` Tool (10th MCP Tool)
- **Files**:
  - `zedis-bridge/src/mcp.rs`
  - `docs/WEB.md`, `docs/WEB_zh.md`
- **Problem**:
  - `zedis-bridge/src/mcp.rs` currently exposes 9 individual tools (`server_info`, `slowlog`, `memory_doctor`, `latency`, etc.). An AI assistant diagnosing a Redis/Valkey instance has to make 5–6 sequential tool calls.
  - Meanwhile, ADR 19 (`zedis_connection::troubleshooting_snapshot` + `zedis_core::report::Report`) already builds a read-only, value-free, secret-redacted diagnostic report combining `INFO`, `MEMORY DOCTOR`, `LATENCY DOCTOR`, slowlog, latency events, and sampled key-size distribution.
- **Proposed Fix**:
  - Expose `troubleshooting_report` in `zedis-bridge/src/mcp.rs` returning the markdown/structured report from `troubleshooting_snapshot` + `Report::from_snapshot`.
  - Add a route/MCP test in `zedis-bridge/src/mcp.rs` and document the 10th tool in `docs/WEB.md` and `docs/WEB_zh.md`.

### 3.2 Update Stale Account Error Hint in `mcp.rs`
- **Files**:
  - `zedis-bridge/src/mcp.rs` (`WRITE_ACCOUNT_REFUSAL`)
- **Problem**:
  - `WRITE_ACCOUNT_REFUSAL` in `zedis-bridge/src/mcp.rs` still says `"give this account read_only = true (or :ro) in the users file"`, which is outdated since ADR 17 replaced the static users file with self-service registration and the Account administration page.
- **Proposed Fix**:
  - Update `WRITE_ACCOUNT_REFUSAL` to refer to assigning the `Viewer` (read-only) role on the Account page or marking the server read-only.

### 3.3 Shared Presence Store Across Multi-Bridge SQL Deployments (Optional)
- **Files**:
  - `zedis-bridge/src/presence.rs`
  - `zedis-bridge/src/sql_store.rs`
- **Problem**:
  - In multi-bridge deployments backed by `--store postgres://...` or `mysql://...`, auth throttling (`ThrottleStore`) is shared across bridge replicas, whereas active client presence (`GET /v1/clients` in `presence.rs`) is held in per-process memory.
- **Proposed Fix**:
  - Optionally persist/query active client heartbeats in the SQL store when `--store` is configured so `GET /v1/clients` shows all connected collaborators across bridge replicas.

---

## 4. CI Security Hardening & Dependency Hygiene

### 4.1 Scope `GITHUB_TOKEN` in `.github/workflows/lint.yml` and `publish.yml`
- **Files**:
  - `.github/workflows/lint.yml`
  - `.github/workflows/publish.yml`
- **Problem**:
  - `.github/workflows/lint.yml` injects `GITHUB_TOKEN: ${{ secrets.GH_TOKEN }}` at the workflow root even though no step in `lint.yml` uses `GITHUB_TOKEN`.
  - `.github/workflows/publish.yml` injects `GITHUB_TOKEN: ${{ secrets.GH_TOKEN }}` at the workflow root, exposing the PAT to `cargo build` (including third-party `build.rs` and proc-macros) and cache steps, whereas only the `upload-release-asset` steps require it.
- **Proposed Fix**:
  - Remove the top-level `env.GITHUB_TOKEN` from `.github/workflows/lint.yml`.
  - Move `GITHUB_TOKEN: ${{ secrets.GH_TOKEN }}` in `.github/workflows/publish.yml` down to the specific release-upload steps that need it.

### 4.2 Update Yanked `crypto-bigint 0.7.6` in `Cargo.lock`
- **Files**:
  - `Cargo.lock`
- **Problem**:
  - `make deny` warns: `warning[yanked]: detected yanked crate (try cargo update -p crypto-bigint)` for `crypto-bigint 0.7.6`.
- **Proposed Fix**:
  - Run `cargo update -p crypto-bigint` (`0.7.6 -> 0.7.10`) and verify with `make deny`.

---

## 5. Documentation & Open Issues Cleanup

### 5.1 Replace `user-attachments` Screenshot & Video Links in `README.md` (Issue #257)
- **Files**:
  - `README.md`, `README_zh.md`
  - `docs/images/`
- **Problem**:
  - `README.md` and `README_zh.md` reference `https://github.com/user-attachments/assets/...` URLs for the demo video and 6 gallery screenshots. Outside GitHub's markdown renderer (e.g., on `crates.io`, `zedis.net`, or when clicking a thumbnail directly), those URLs return 404 or fail to play inline.
- **Proposed Fix**:
  - Host the screenshots and demo media under `docs/images/` (or `https://zedis.net/images/...`) and update the links in `README.md` and `README_zh.md`.

### 5.2 Triage / Close Resolved Issues (#190)
- **Issue**:
  - Issue #190 (hover alignment) was confirmed resolved by the reporter and can be closed via `gh issue close 190`.

---

## Progress Tracker

| Step | Area | Summary | Status |
|---|---|---|---|
| 1.1 | Desktop UX | Fix filtered key-tree refresh (`⌘R`) blanking & lazy folder reset (#148) | Done |
| 2.1 | Web Parity | Enable single-key copy & Compare *Copy to target* in browser | Done |
| 2.2 | Web Parity | Enable JSON/CSV readable key import in browser | Pending |
| 2.3 | Web Parity | Enable `SCRIPT KILL` / `FUNCTION KILL` in browser via bridge | Pending |
| 3.1 | MCP Server | Add `troubleshooting_report` (10th MCP tool) in `zedis-bridge` | Pending |
| 3.2 | MCP Server | Update stale `WRITE_ACCOUNT_REFUSAL` hint in `zedis-bridge/src/mcp.rs` | Pending |
| 3.3 | MCP Server | Share client presence across multi-bridge SQL deployments | Optional |
| 4.1 | CI Security | Scope `GITHUB_TOKEN` to only steps that need it in `lint.yml` / `publish.yml` | Pending |
| 4.2 | Dependencies | Update yanked `crypto-bigint` (`0.7.6 -> 0.7.10`) in `Cargo.lock` | Pending |
| 5.1 | Docs | Replace `user-attachments` links in `README.md` / `README_zh.md` (#257) | Pending |
| 5.2 | Issues | Close confirmed-fixed Issue #190 | Pending |
