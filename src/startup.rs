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

//! Pre-window startup pieces: version constants, the database recovery
//! window, CLI argument parsing and the smoke-test gates.

use crate::connection::get_servers;
use crate::db::{DbOpenFailure, init_database, quarantine_database};
use crate::states::{Route, ServerView, ZedisAppState};
use crate::{init_caches, launch};
use gpui::{SharedString, Window, div, prelude::*};
// Only the custom-drawn title bar path uses this (Linux/FreeBSD keep
// server-side decorations — see the cfg at the open_window call).
use gpui_component::{
    ActiveTheme, StyledExt,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    v_flex,
};
use rust_i18n::t;
use tracing::{error, warn};

pub(crate) const PKG_NAME: &str = env!("CARGO_PKG_NAME");
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");
pub(crate) const GIT_SHA: &str = env!("VERGEN_GIT_SHA");

/// Shown when the local database can't be opened. Three causes, three
/// remedies: another instance holds the lock (quit it), the file was written by
/// a newer Zedis (update, or rebuild), or the file is damaged (rebuild).
/// "Back up & rebuild" moves the file aside as `zedis.redb.corrupt-<ts>` —
/// nothing is deleted; tags, favorites, history and scripts live in it —
/// creates a fresh one, and hands over to the normal startup (`launch`).
pub(crate) struct DatabaseErrorView {
    failure: DbOpenFailure,
    app_state: ZedisAppState,
    /// Why the last "Back up & rebuild" attempt failed, shown inline.
    rebuild_error: Option<String>,
}

impl DatabaseErrorView {
    pub(crate) fn new(failure: DbOpenFailure, app_state: ZedisAppState) -> Self {
        Self {
            failure,
            app_state,
            rebuild_error: None,
        }
    }

    /// No `ZedisGlobalStore` exists yet on this path, so translate against
    /// the locale straight from the loaded state.
    fn text(&self, key: &str) -> SharedString {
        t!(format!("database.{key}"), locale = self.app_state.locale())
            .to_string()
            .into()
    }

    fn rebuild(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let quarantined = match quarantine_database() {
            Ok(path) => path,
            Err(e) => {
                error!(error = %e, "could not move the local database aside");
                self.rebuild_error = Some(e.to_string());
                cx.notify();
                return;
            }
        };
        warn!(quarantined = %quarantined.display(), "local database moved aside; creating a fresh one");
        if let Err(e) = init_database() {
            error!(error = %e, "rebuilding the local database failed");
            self.rebuild_error = Some(e.to_string());
            cx.notify();
            return;
        }
        init_caches();
        let handle = window.window_handle();
        launch(cx, self.app_state.clone());
        cx.spawn(async move |_this, cx| {
            // Queued behind `launch`'s own spawn, so the main window is open
            // before this one goes: on Linux/Windows the default QuitMode
            // ends the app when the last window closes. The guard keeps the
            // recovery window around rather than quitting if that ever
            // doesn't hold.
            cx.update(|cx| {
                if cx.windows().len() > 1 {
                    let _ = handle.update(cx, |_, window, _| window.remove_window());
                }
            });
        })
        .detach();
    }
}

impl Render for DatabaseErrorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (body_key, can_rebuild) = match &self.failure {
            DbOpenFailure::Locked => ("locked_body", false),
            DbOpenFailure::SchemaTooNew { .. } => ("schema_too_new_body", true),
            DbOpenFailure::Damaged(_) => ("damaged_body", true),
            DbOpenFailure::Inaccessible(_) => ("inaccessible_body", false),
        };
        let detail: Option<String> = match &self.failure {
            DbOpenFailure::Locked => None,
            DbOpenFailure::SchemaTooNew { found, supported } => Some(format!("schema v{found} > v{supported}")),
            DbOpenFailure::Damaged(message) | DbOpenFailure::Inaccessible(message) => Some(message.clone()),
        };
        let rebuild_error = self
            .rebuild_error
            .as_ref()
            .map(|e| format!("{}: {e}", self.text("rebuild_failed")));
        let (title, body, quit, rebuild) = (
            self.text("title"),
            self.text(body_key),
            self.text("quit"),
            self.text("rebuild"),
        );
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .p_5()
            .gap_3()
            .child(Label::new(title).font_semibold())
            .child(Label::new(body).whitespace_normal())
            .when_some(detail, |this, detail| {
                this.child(Label::new(detail).text_xs().text_color(muted).whitespace_normal())
            })
            .when_some(rebuild_error, |this, message| {
                this.child(Label::new(message).text_color(danger).whitespace_normal())
            })
            .child(div().flex_1())
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("quit-db-error")
                            .label(quit)
                            .on_click(|_, _window, cx| cx.quit()),
                    )
                    .when(can_rebuild, |this| {
                        this.child(
                            Button::new("rebuild-db")
                                .label(rebuild)
                                .primary()
                                .on_click(cx.listener(|this, _, window, cx| this.rebuild(window, cx))),
                        )
                    }),
            )
    }
}

/// Value of a `--flag <value>` / `--flag=<value>` command-line argument.
pub(crate) fn cli_arg_value(flag: &str) -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == flag {
            return args.next();
        }
        // `strip_prefix('=')` keeps `--db` from matching `--database=x`.
        if let Some(value) = arg.strip_prefix(flag).and_then(|rest| rest.strip_prefix('=')) {
            return Some(value.to_string());
        }
    }
    None
}

/// A parsed `--route <name>` target: app-level routes stand alone, while a
/// server view still needs the `(id, db)` the startup composer resolves.
pub(crate) enum CliRoute {
    App(Route),
    View(ServerView),
}

/// Startup view override: `--route <name>` (`home`, `editor`, `metrics`, …).
/// Together with `--server` / `--db` this is the deep-link MVP behind the
/// screenshot-comparison workflow. Unrecognized names log a warning and are
/// ignored.
pub(crate) fn cli_route_override() -> Option<CliRoute> {
    let raw = cli_arg_value("--route")?;
    if let Some(route) = Route::app_from_name(&raw) {
        return Some(CliRoute::App(route));
    }
    match ServerView::from_name(raw.trim().to_ascii_lowercase().as_str()) {
        Some(view) => Some(CliRoute::View(view)),
        None => {
            warn!(route = %raw, "unrecognized --route value; ignoring");
            None
        }
    }
}

/// Startup connection override: `--server <id|name>`, resolved to a server id
/// — exact id first, then exact name, then case-insensitive name.
pub(crate) fn cli_server_override() -> Option<String> {
    let raw = cli_arg_value("--server")?;
    let Ok(servers) = get_servers() else {
        warn!("server config unavailable; ignoring --server");
        return None;
    };
    let found = servers
        .iter()
        .find(|s| s.id == raw)
        .or_else(|| servers.iter().find(|s| s.name == raw))
        .or_else(|| servers.iter().find(|s| s.name.eq_ignore_ascii_case(&raw)));
    if found.is_none() {
        warn!(server = %raw, "no server matches --server by id or name; ignoring");
    }
    found.map(|s| s.id.clone())
}

/// Startup database override: `--db <n>`.
pub(crate) fn cli_db_override() -> Option<usize> {
    let raw = cli_arg_value("--db")?;
    let db = raw.parse::<usize>().ok();
    if db.is_none() {
        warn!(db = %raw, "invalid --db value; ignoring");
    }
    db
}

/// True when launched with `ZEDIS_SMOKE_TEST=1` — the CI smoke mode: exit 0
/// as soon as the first frame has painted, else the watchdog kills the
/// process with a nonzero code. See the hooks in `main`.
pub(crate) fn is_smoke_test() -> bool {
    std::env::var("ZEDIS_SMOKE_TEST").is_ok_and(|v| v == "1")
}

/// `ZEDIS_SMOKE_GATE=window` relaxes the smoke success signal from "first
/// frame painted" to "main window created and the process survived its
/// first seconds". Headless Linux CI (Xvfb + llvmpipe) never delivers the
/// frame-present signal, so the frame gate can't be a hard gate there —
/// this one still catches the regressions that matter on that platform:
/// missing system libraries, Vulkan / window-creation failures, startup
/// panics (DB, config, theme, fonts).
pub(crate) fn smoke_gate_is_window() -> bool {
    std::env::var("ZEDIS_SMOKE_GATE").is_ok_and(|v| v == "window")
}
