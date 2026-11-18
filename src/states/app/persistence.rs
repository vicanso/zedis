// Copyright 2026 Tree xie.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! State persistence, secret sealing, debounced config saves, quit flushing,
//! and shared dialog/layout helpers for [`ZedisAppState`].

use super::*;
#[cfg(target_family = "wasm")]
use crate::helpers::browser_store_path;
#[cfg(not(target_family = "wasm"))]
use crate::helpers::get_or_create_config_dir;
#[cfg(not(target_family = "wasm"))]
use crate::helpers::{decrypt, encrypt};
use crate::helpers::{format_human_duration, write_file_atomic_with_backup};
#[cfg(target_family = "wasm")]
use crate::states::save_preferences;
use gpui_kit::component::dialog::DialogButtonProps;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::info;

#[cfg(not(target_family = "wasm"))]
pub(super) fn app_state_path() -> Result<PathBuf> {
    // Same file name in both environments — a development run is isolated by its
    // own config *directory* (`<config_dir>/dev`), not by a `-dev` file suffix.
    let path = get_or_create_config_dir()?.join("zedis.toml");
    if path.exists() {
        return Ok(path);
    }
    std::fs::write(&path, "")?;
    Ok(path)
}

/// In a browser the preferences are the visitor's and the device's, so they
/// live in the browser: `zedis.toml` is a `localStorage` entry, which is what
/// a path from `browser_store_path` means to `fs` there (ADR 9). Nothing to
/// create — an absent entry reads as "first run".
#[cfg(target_family = "wasm")]
pub(super) fn app_state_path() -> Result<PathBuf> {
    Ok(browser_store_path("zedis.toml"))
}

/// The form a secret setting is kept in: encrypted under the machine's key.
/// Should encryption fail, the value is kept as it is.
#[cfg(not(target_family = "wasm"))]
pub(super) fn seal_secret(value: &str) -> String {
    encrypt(value).unwrap_or_else(|_| value.to_string())
}

/// No master key in a tab, and nothing is written to disk there either
/// (`fs` refuses), so the stored form *is* the plain form.
#[cfg(target_family = "wasm")]
pub(super) fn seal_secret(value: &str) -> String {
    value.to_string()
}

/// A kept secret read back. One that does not decrypt (hand-edited plain
/// text in `zedis.toml`) is taken as it is.
#[cfg(not(target_family = "wasm"))]
pub(super) fn unseal_secret(stored: &str) -> String {
    decrypt(stored).unwrap_or_else(|_| stored.to_string())
}

#[cfg(target_family = "wasm")]
pub(super) fn unseal_secret(stored: &str) -> String {
    stored.to_string()
}

/// Persists the app state crash-safely: atomic replace plus a rolling
/// `zedis.toml.bak` of the previous version (see `write_file_atomic_with_backup`).
#[cfg(not(target_family = "wasm"))]
pub fn save_app_state(state: &ZedisAppState) -> Result<()> {
    let path = app_state_path()?;
    let value = toml::to_string(state)?;
    write_file_atomic_with_backup(&path, value.as_bytes())?;
    Ok(())
}

/// In a page: this browser's `localStorage`, and the account's copy.
#[cfg(target_family = "wasm")]
pub fn save_app_state(state: &ZedisAppState) -> Result<()> {
    let path = app_state_path()?;
    // What a tab stores is readable by every script on the origin, and there
    // is no master key in a tab to encrypt it under — so the two secrets this
    // state can hold are not part of what is stored there. They stay in
    // memory for the session, as they did before anything was stored.
    let state = &ZedisAppState {
        ai_api_key: None,
        http_proxy: None,
        ..state.clone()
    };
    let value = toml::to_string(state)?;
    write_file_atomic_with_backup(&path, value.as_bytes())?;
    // The account's, on the bridge, as well as this browser's (ADR 17).
    save_preferences(&value);
    Ok(())
}

/// Width the content pane actually gets: the viewport minus the sidebar *as it
/// currently is*. Table views size their columns from this, and reading it as
/// the expanded [`SIDEBAR_WIDTH`] left them ~128px short whenever the sidebar was
/// collapsed to its icon rail.
///
/// Note this is sampled when a view builds its columns; collapsing the sidebar
/// while such a view is already open won't re-lay it out until the view is
/// rebuilt (it is dropped on route change).
pub fn content_area_width(window: &Window, cx: &App) -> Pixels {
    let sidebar = cx.global::<ZedisGlobalStore>().read(cx).sidebar_px();
    window.viewport_size().width - sidebar
}

/// Debounce window for high-frequency config saves (panel drags, sliders).
const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

/// Write the current app state to disk while the app is shutting down.
///
/// Every `update_app_state_and_save*` helper persists asynchronously — the
/// debounced ones only after [`SAVE_DEBOUNCE`] of quiet — and `cx.quit()` waits
/// for none of it. Resizing the key tree, switching a tab or toggling a
/// preference and immediately quitting therefore lost the change. gpui blocks
/// shutdown on the future returned here (up to its own shutdown timeout), so
/// the write runs inline instead of on the background executor.
///
/// One registration covers every way out — the Quit action, ⌘Q, the last window
/// closing, and the quit that hands over to an installer — so no exit path has
/// to remember to flush. Register it once, after the store is installed as a
/// global.
pub fn flush_app_state_on_quit(cx: &App) {
    cx.on_app_quit(|cx| {
        // `try_global`: the recovery window that runs when the local database
        // can't be opened never installs the store, and quitting from there
        // must not panic.
        let state = cx.try_global::<ZedisGlobalStore>().map(|store| store.read(cx).clone());
        async move {
            let Some(state) = state else {
                return;
            };
            match save_app_state(&state) {
                Ok(()) => info!("flushed app state before quitting"),
                Err(e) => error!(error = %e, "failed to flush app state before quitting"),
            }
        }
    })
    .detach();
}

/// Generation counter behind [`update_app_state_and_save_debounced`]: each
/// debounced call bumps it, sleeps, and only the call still holding the
/// latest generation performs the write — coalescing a drag's event stream
/// into one disk write regardless of which view the events came from.
static SAVE_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Shared core for the `update_app_state_and_save*` helpers.
///
/// 1. Apply the mutation to the global app state (always immediate).
/// 2. If `debounce`, wait out [`SAVE_DEBOUNCE`] and yield to a newer call.
/// 3. Snapshot the state *after* the wait (so the write carries every
///    mutation applied during the quiet window) and persist it to disk.
/// 4. If `refresh`, refresh all windows to apply visual changes.
fn apply_and_save<F>(cx: &App, action_name: &'static str, refresh: bool, debounce: bool, mutation: F)
where
    F: FnOnce(&mut ZedisAppState, &App) + Send + 'static + Clone,
{
    let store = cx.global::<ZedisGlobalStore>().clone();

    cx.spawn(async move |cx| {
        store.update(cx, |state, cx| mutation(state, cx));

        if debounce {
            let generation = SAVE_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            if SAVE_GENERATION.load(Ordering::Acquire) != generation {
                // Superseded by a newer debounced save; that call will
                // persist a state snapshot that already includes this
                // mutation.
                return;
            }
        }

        let state = store.update(cx, |state, _| state.clone());
        cx.background_executor()
            .spawn(async move {
                if let Err(e) = save_app_state(&state) {
                    error!(error = %e, action = action_name, "Failed to save state");
                } else {
                    info!(action = action_name, "State saved successfully");
                }
            })
            .await;

        if refresh {
            cx.update(|cx| cx.refresh_windows());
        }
    })
    .detach();
}

/// Update app state, persist to disk, and refresh all windows.
///
/// Use for changes with app-wide visual effect (theme, locale, layout
/// preferences read by other windows).
#[inline]
pub fn update_app_state_and_save<F>(cx: &App, action_name: &'static str, mutation: F)
where
    F: FnOnce(&mut ZedisAppState, &App) + Send + 'static + Clone,
{
    apply_and_save(cx, action_name, true, false, mutation);
}

/// Update app state and persist to disk *without* refreshing windows.
///
/// Use for bookkeeping state with no visual output (open tabs, update-check
/// timestamps, AI credentials) — any visible side effect at those call sites
/// already goes through its own store update + notify.
#[inline]
pub fn update_app_state_and_save_quiet<F>(cx: &App, action_name: &'static str, mutation: F)
where
    F: FnOnce(&mut ZedisAppState, &App) + Send + 'static + Clone,
{
    apply_and_save(cx, action_name, false, false, mutation);
}

/// Update app state immediately, but debounce the disk write (and the final
/// window refresh) behind a [`SAVE_DEBOUNCE`] quiet window.
///
/// Use on high-frequency event streams — slider changes and the like — where
/// writing the whole TOML per event is pure churn but the settled value must
/// still repaint every window (e.g. the app font size). The in-memory state
/// updates per event, so live UI reads stay current; only persistence and the
/// app-wide repaint wait for the stream to settle. A quit inside the quiet
/// window can lose the last write (same trade-off as the window-bounds save
/// in `main.rs`).
#[inline]
pub fn update_app_state_and_save_debounced<F>(cx: &App, action_name: &'static str, mutation: F)
where
    F: FnOnce(&mut ZedisAppState, &App) + Send + 'static + Clone,
{
    apply_and_save(cx, action_name, true, true, mutation);
}

/// [`update_app_state_and_save_debounced`] without the settled-time window
/// refresh — for high-frequency streams whose visual effect is already
/// local to the emitting view (panel-resize drags: the view tracks the
/// width in its own field and repaints itself per event).
#[inline]
pub fn update_app_state_and_save_quiet_debounced<F>(cx: &App, action_name: &'static str, mutation: F)
where
    F: FnOnce(&mut ZedisAppState, &App) + Send + 'static + Clone,
{
    apply_and_save(cx, action_name, false, true, mutation);
}

/// Cancel / **Delete**: the buttons of a confirm that deletes something.
///
/// The name does not say so, and for a while every confirm took it as it
/// came — killing a client, a failover, enabling keyspace notifications and
/// `SENTINEL RESET` all asked their question over a button reading "Delete"
/// (#186). A confirm that deletes nothing names its own verb, or at least
/// `.ok_text(i18n_common(cx, "confirm"))`.
pub fn dialog_button_props(cx: &App) -> DialogButtonProps {
    DialogButtonProps::default()
        .cancel_text(i18n_common(cx, "cancel"))
        .ok_text(i18n_common(cx, "delete"))
}

/// A timeout as the Settings field shows it: whole seconds as a number, as
/// they always were, and anything finer as a duration (`500ms`, `1s 500ms`).
/// Shown as whole seconds, `500ms` read back as `0`, and the next time the
/// field lost focus that `0` was saved.
pub(super) fn timeout_text(timeout: Duration) -> String {
    if timeout.subsec_nanos() == 0 {
        timeout.as_secs().to_string()
    } else {
        format_human_duration(timeout)
    }
}

/// Escalate a destructive-action confirm-dialog body for production servers.
///
/// The app's safety convention is that destructive Redis ops escalate their
/// *wording* on a high-risk (PROD-tagged) connection. `dialog_button_props`
/// only sets button labels, so call-sites that build their own
/// `ZedisDialog::new_alert(...)` must run the body through this to actually
/// get the escalation. It mirrors the `high_risk_warning` suffix that
/// `confirm_dangerous_command` appends for `ConfirmStrictness::TypeName`, but
/// works for the many UI actions (key/server delete, XGROUP DESTROY, cluster
/// ops, ACL/FUNCTION delete, ...) that don't map to a CLI `DangerKind`.
/// Returns the body unchanged for non-high-risk servers.
pub fn escalate_dangerous_body(cx: &App, server_id: &str, body: impl Into<SharedString>) -> SharedString {
    let body = body.into();
    let high_risk = get_server(server_id).map(|s| s.is_high_risk_tag()).unwrap_or(false);
    if !high_risk {
        return body;
    }
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
    let warning = rust_i18n::t!("danger.high_risk_warning", locale = &locale).to_string();
    format!("{body}\n\n{warning}").into()
}
