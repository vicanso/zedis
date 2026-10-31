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

//! What the root does with the actions its window receives: themes,
//! language and zoom, the server tools, the tabs, the menus and Esc.

use super::*;

/// A theme mode picked: applied at once, and saved.
pub(super) fn on_theme(_this: &mut Zedis, e: &ThemeAction, _window: &mut Window, cx: &mut Context<Zedis>) {
    let action = *e;

    // Convert action to theme mode
    let mode = match action {
        ThemeAction::Light => Some(ThemeMode::Light),
        ThemeAction::Dark => Some(ThemeMode::Dark),
        ThemeAction::System => None, // Follow OS theme
    };

    // Determine actual render mode (resolve System to Light/Dark)
    let render_mode = match mode {
        Some(m) => m,
        None => theme_mode_for_appearance(cx.window_appearance()),
    };

    // A previously-applied named theme overwrote the Theme's
    // light/dark slot, so restore the registry defaults first —
    // otherwise switching mode would just re-apply that named theme.
    restore_default_themes(cx);
    // Apply theme immediately for instant visual feedback
    Theme::change(render_mode, None, cx);
    apply_default_ui_font_size(cx);

    // Save preference to disk asynchronously
    update_app_state_and_save(cx, "save_theme", move |state, _cx| {
        state.set_theme(mode);
    });
}

/// A named theme picked: applied, and saved if it exists.
pub(super) fn on_select_theme(_this: &mut Zedis, e: &SelectThemeAction, _window: &mut Window, cx: &mut Context<Zedis>) {
    let name = e.name.clone();
    if apply_named_theme(&name, cx) {
        update_app_state_and_save(cx, "save_theme_name", move |state, _cx| {
            state.set_theme_name(Some(name));
        });
    }
}

/// Locale action handler - changes language and saves to disk
pub(super) fn on_locale(_this: &mut Zedis, e: &LocaleAction, _window: &mut Window, cx: &mut Context<Zedis>) {
    let locale = match e {
        LocaleAction::Zh => "zh",
        LocaleAction::En => "en",
        LocaleAction::Ja => "ja",
        LocaleAction::Ru => "ru",
        LocaleAction::Pt => "pt",
        LocaleAction::De => "de",
        LocaleAction::Fr => "fr",
        LocaleAction::Es => "es",
    };

    save_ui_locale(cx, locale);
}

/// Settings, and the Protos and Scripts pages.
pub(super) fn on_settings(this: &mut Zedis, e: &SettingsAction, window: &mut Window, cx: &mut Context<Zedis>) {
    match e {
        SettingsAction::Editor => {
            this.keep_focus_in_page(window, cx);
            open_settings(window, cx)
        }
        SettingsAction::Protos => {
            cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                store.update(cx, |state, cx| {
                    state.go_to(Route::Protos, cx);
                });
            });
        }
        SettingsAction::Scripts => {
            cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                store.update(cx, |state, cx| {
                    state.go_to(Route::Scripts, cx);
                });
            });
        }
    }
}

/// Export Diagnostics: the bundle, for the active tab's connection.
pub(super) fn on_diagnostics(this: &mut Zedis, _: &DiagnosticsAction, window: &mut Window, cx: &mut Context<Zedis>) {
    this.export_diagnostics(window, cx);
}

/// A server tool from the status bar or the palette: a view of the active
/// tab's connection, or a dialog or window over it.
pub(super) fn on_server_tool(this: &mut Zedis, e: &ServerToolsAction, window: &mut Window, cx: &mut Context<Zedis>) {
    let target = match e {
        ServerToolsAction::Monitor => ServerView::Monitor,
        ServerToolsAction::Config => ServerView::Config,
        ServerToolsAction::Acl => ServerView::Acl,
        ServerToolsAction::Search => ServerView::Search,
        ServerToolsAction::Functions => ServerView::Functions,
        ServerToolsAction::LuaScripts => ServerView::LuaScripts,
        ServerToolsAction::Persistence => ServerView::Persistence,
        ServerToolsAction::KeyspaceNotifications => ServerView::KeyspaceNotifications,
        ServerToolsAction::Topology => ServerView::Topology,
        ServerToolsAction::ServerLoad => ServerView::ServerLoad,
        ServerToolsAction::Hotkeys => ServerView::Hotkeys,
        ServerToolsAction::TimeSeriesExplorer => ServerView::TimeSeriesExplorer,
        ServerToolsAction::ValueSearch => ServerView::ValueSearch,
        ServerToolsAction::ServerInfo => ServerView::ServerInfo,
        ServerToolsAction::Benchmark => ServerView::Benchmark,
        // FLUSHDB / FLUSHALL: a confirm dialog over whatever view
        // is active, never a route. The menu entry is already
        // disabled on a read-only connection and
        // `flush_database` re-checks the capability, so this arm
        // only has to raise the dialog.
        ServerToolsAction::FlushDb | ServerToolsAction::FlushAll => {
            let all = matches!(e, ServerToolsAction::FlushAll);
            let Some((server_id, _)) = cx.global::<ZedisGlobalStore>().read(cx).selected_server().cloned() else {
                return;
            };
            let Ok(server) = get_server(&server_id) else {
                return;
            };
            let kind = if all { DangerKind::FlushAll } else { DangerKind::FlushDb };
            let line = if all { "FLUSHALL" } else { "FLUSHDB" };
            let server_state = this.active_content().read(cx).server_state();
            let db = server_state.read(cx).db();
            confirm_dangerous_command(&server, db, &kind, Some(line), window, cx, move |_window, cx| {
                server_state.update(cx, |state, cx| {
                    state.flush_database(all, cx);
                });
            });
            return;
        }
        // A dialog, not a sub-route: keeps whatever view is
        // active underneath.
        ServerToolsAction::Trash => {
            let server_state = this.active_content().read(cx).server_state();
            open_trash_dialog(server_state, window, cx);
            return;
        }
        // The probed command matrix of the active tab's connection.
        ServerToolsAction::Capabilities => {
            let server_state = this.active_content().read(cx).server_state();
            open_features_dialog(server_state, window, cx);
            return;
        }
        // What the server says about itself, with what this
        // session's panels found, to paste or send.
        ServerToolsAction::TroubleshootingReport => {
            let server_state = this.active_content().read(cx).server_state();
            open_troubleshoot_report(server_state, window, cx);
            return;
        }
        // Import keys into the active server / db (not a
        // key-tree prefix).
        ServerToolsAction::ImportKeys => {
            let Some((server_id, db)) = cx.global::<ZedisGlobalStore>().read(cx).selected_server().cloned() else {
                return;
            };
            let server_name: gpui::SharedString = get_server(&server_id)
                .map(|s| s.name.into())
                .unwrap_or_else(|_| server_id.clone().into());
            this.keep_focus_in_page(window, cx);
            open_key_import(server_id.into(), server_name, db, window, cx);
            return;
        }
        // Pub/Sub lives inside the editor suite (channel mode),
        // not on a tool route — flip the mode, then land on the
        // editor view so the panel is visible.
        ServerToolsAction::PubsubMode => {
            let server_state = this.active_content().read(cx).server_state();
            server_state.update(cx, |state, cx| state.change_channel_mode(cx));
            cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                store.update(cx, |state, cx| state.go_to_view(ServerView::Editor, cx));
            });
            return;
        }
        // Compare a prefix of the active server / db with another
        // server / db, in its own window.
        ServerToolsAction::CompareKeys => {
            let Some((server_id, db)) = cx.global::<ZedisGlobalStore>().read(cx).selected_server().cloned() else {
                return;
            };
            let server_name: gpui::SharedString = get_server(&server_id)
                .map(|s| s.name.into())
                .unwrap_or_else(|_| server_id.clone().into());
            this.keep_focus_in_page(window, cx);
            open_compare(server_id.into(), server_name, db, window, cx);
            return;
        }
        // Export every key loaded in the active tab's tree (a
        // SCAN-limited subset, same coverage as the tree itself).
        ServerToolsAction::ExportKeys => {
            let Some((server_id, db)) = cx.global::<ZedisGlobalStore>().read(cx).selected_server().cloned() else {
                return;
            };
            let server_name: gpui::SharedString = get_server(&server_id)
                .map(|s| s.name.into())
                .unwrap_or_else(|_| server_id.clone().into());
            let mut keys: Vec<gpui::SharedString> = this
                .active_content()
                .read(cx)
                .server_state()
                .read(cx)
                .keys()
                .keys()
                .cloned()
                .collect();
            if keys.is_empty() {
                return;
            }
            keys.sort_unstable();
            this.keep_focus_in_page(window, cx);
            open_key_export(
                server_id.into(),
                server_name,
                db,
                keys,
                ExportSource::Loaded,
                window,
                cx,
            );
            return;
        }
    };
    cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
        store.update(cx, |state, cx| {
            state.toggle_view(target, cx);
        });
    });
}

/// A tab's own menu: close it, the others, those to its right.
pub(super) fn on_tab(this: &mut Zedis, e: &TabAction, _window: &mut Window, cx: &mut Context<Zedis>) {
    match e {
        TabAction::Close(ix) => this.close_tab(*ix, cx),
        TabAction::CloseOthers(ix) => this.close_others(*ix, cx),
        TabAction::CloseRight(ix) => this.close_right(*ix, cx),
    }
}

/// A connection's menu in the sidebar.
pub(super) fn on_sidebar_server(
    this: &mut Zedis,
    e: &SidebarServerAction,
    window: &mut Window,
    cx: &mut Context<Zedis>,
) {
    this.on_sidebar_server_action(e, window, cx);
}

/// ⌘1–⌘8 / Ctrl+1–8: jump to the Nth workspace tab (1-based key).
/// No-op when that index is not open yet (or already active).
pub(super) fn on_workspace_tab(this: &mut Zedis, e: &WorkspaceTabAction, window: &mut Window, cx: &mut Context<Zedis>) {
    let WorkspaceTabAction::Select(ix) = *e;
    if ix < this.tabs.len() {
        this.activate_tab(ix, Some(window), cx);
        this.project_active_tab(cx);
    }
}

/// ⌘+ / ⌘- / ⌘0: the same UI font size Settings drives, stepped
/// by one pixel inside that range; the store's value is applied
/// to the theme on the next frame.
pub(super) fn on_zoom(_this: &mut Zedis, e: &ZoomAction, _window: &mut Window, cx: &mut Context<Zedis>) {
    let current = cx
        .global::<ZedisGlobalStore>()
        .read(cx)
        .font_rem_px()
        .unwrap_or(DEFAULT_UI_FONT_SIZE);
    let next = match e {
        ZoomAction::In => current + 1.0,
        ZoomAction::Out => current - 1.0,
        ZoomAction::Reset => DEFAULT_UI_FONT_SIZE,
    }
    .clamp(UI_FONT_SIZE_MIN, UI_FONT_SIZE_MAX);
    update_app_state_and_save(cx, "zoom", move |state, _| {
        state.set_font_rem_px(Some(next));
    });
}

/// The macOS Window menu's commands.
pub(super) fn on_window(_this: &mut Zedis, e: &WindowAction, window: &mut Window, _cx: &mut Context<Zedis>) {
    match e {
        WindowAction::Minimize => window.minimize_window(),
        WindowAction::Zoom => window.zoom_window(),
        WindowAction::ToggleFullscreen => window.toggle_fullscreen(),
    }
}

/// ⌘W / Ctrl+W closes the active workspace tab while more than one is
/// open. With a single tab (or any non-Close menu action) it
/// propagates to the app-level `MemuAction` handler, which hides the
/// app on macOS / closes the window elsewhere — the red-button
/// behavior. Handled here (not only globally) so the view's tab list
/// is in reach; ⌘W stays bound to the one `MemuAction::Close`.
pub(super) fn on_menu(this: &mut Zedis, e: &MemuAction, window: &mut Window, cx: &mut Context<Zedis>) {
    match e {
        MemuAction::Close if this.tabs.len() > 1 => this.close_tab(this.active_tab, cx),
        // The bridge's account page, as a dialog over the page; on
        // the desktop nothing draws the item, and nothing opens.
        MemuAction::Account => {
            this.keep_focus_in_page(window, cx);
            open_account(window, cx);
        }
        _ => cx.propagate(),
    }
}

/// ⌘F / secondary-f: focus the active page's filter box.
/// Must live on the window root — after a tab switch focus often
/// sits on the tab pill (sibling of content), so handlers only on
/// `ZedisContent` / `ZedisServers` never see the action.
/// The binding carries `!Input`, so while focus is inside any
/// gpui-component `Input` this never fires and a code editor's
/// built-in ⌘F search panel takes the keystroke instead.
pub(super) fn on_editor(this: &mut Zedis, e: &EditorAction, window: &mut Window, cx: &mut Context<Zedis>) {
    match e {
        EditorAction::Search => {
            this.active_content().update(cx, |content, cx| {
                content.focus_search(window, cx);
            });
        }
        _ => cx.propagate(),
    }
}

/// Esc mirrors the tool pages' "back to editor" button. The
/// global keybinding only reaches here when no deeper handler
/// (focused input, open dialog, command palette) claimed the
/// keystroke; no-op on routes without a back affordance.
pub(super) fn on_nav(_this: &mut Zedis, _e: &NavAction, window: &mut Window, cx: &mut Context<Zedis>) {
    // Esc first dismisses an open dialog (the server editor,
    // import/export, a confirm alert, …). This binding lives in the
    // `Workspace` context and consumes Esc without propagating, so
    // when a dialog's own `escape` handler doesn't win (e.g. focus
    // isn't inside the dialog subtree) Esc would otherwise be
    // swallowed here — doing nothing on Home, or worse, navigating
    // the page *behind* the still-open dialog on a Server route.
    if window.has_active_dialog(cx) {
        window.close_dialog(cx);
        return;
    }
    cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
        store.update(cx, |state, cx| {
            if !matches!(
                state.route(),
                Route::Home
                    | Route::Settings
                    | Route::Server {
                        view: ServerView::Editor,
                        ..
                    }
            ) {
                state.go_to_view(ServerView::Editor, cx);
            }
        });
    });
}

/// What the app announces that the root answers for: a notification to show,
/// the active tab's connection, a request for a tab, and the server list.
pub(super) fn on_global_event(this: &mut Zedis, event: &GlobalEvent, cx: &mut Context<Zedis>) {
    match event {
        GlobalEvent::Notification(e) => {
            let message = e.message.clone();
            let mut notification = match e.category {
                NotificationCategory::Info => {
                    info!(message = %message, "info notification");
                    Notification::info(message)
                }
                NotificationCategory::Success => {
                    info!(message = %message, "success notification");
                    Notification::success(message)
                }
                NotificationCategory::Warning => {
                    info!(message = %message, "warning notification");
                    Notification::warning(message)
                }
                NotificationCategory::Error => {
                    error!(message = %message, "error notification");
                    Notification::error(message)
                }
            };
            // An empty title must not reach the toast: it renders as a
            // blank title row with the message pushed below it.
            if let Some(title) = e.title.as_ref().filter(|title| !title.is_empty()) {
                notification = notification.title(title);
            }
            this.pending_notification = Some(notification);
        }
        GlobalEvent::ServerSelected(server_id, db) => {
            // Track the active tab's connection identity. Deliberately
            // no "jump to an existing tab" here: the contents'
            // subscriptions run before this one, so by the time this
            // fired the active tab has already followed the selection.
            // Normal clicks navigate the current tab (browser-like);
            // only the explicit open-in-new-tab path below dedupes
            // onto an existing tab.
            let tab = &mut this.tabs[this.active_tab];
            if server_id.is_empty() {
                tab.server_id.clear();
                tab.db = 0;
            } else {
                tab.server_id = server_id.to_string();
                tab.db = *db;
            }
            this.persist_tabs(cx);
        }
        GlobalEvent::ServerOpenInNewTab(server_id, db, force) => {
            // `force` (⌘/Ctrl+Shift+click) skips the dedup lookup so a
            // duplicate tab on the same `(server, db)` can be opened.
            let existing = (!*force).then(|| {
                this.tabs
                    .iter()
                    .position(|tab| tab.server_id.as_str() == server_id.as_str() && tab.db == *db)
            });
            if let Some(ix) = existing.flatten() {
                this.activate_tab(ix, None, cx);
                this.project_active_tab(cx);
            } else if this.tabs.len() >= MAX_TABS {
                // The click still keeps its promise — the server opens
                // — just in the tab that is already active, and the
                // notice says which limit was hit and what to do about
                // it. A refusal alone read as the click doing nothing.
                let body = i18n_common(cx, "tab_limit_body").replace("%{max}", &MAX_TABS.to_string());
                this.pending_notification = Some(Notification::warning(body).title(i18n_common(cx, "tab_limit_title")));
                let (id, db) = (server_id.to_string(), *db);
                cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
                    if id.is_empty() {
                        state.clear_selected_server(cx);
                    } else {
                        state.connect_server(id, db, cx);
                    }
                });
            } else {
                // Creating a `ZedisContent` needs a `Window`; stash the
                // request and let `render` build the tab.
                this.pending_new_tab = Some((server_id.to_string(), *db));
            }
        }
        GlobalEvent::ServerListUpdated => this.close_tabs_of_removed_servers(cx),
        _ => {}
    }
    cx.notify();
}

/// The OS appearance changed.
pub(super) fn on_window_appearance(this: &mut Zedis, cx: &mut Context<Zedis>) {
    // Only follow the OS appearance on System mode with no named theme
    // active — a named theme should persist across OS appearance changes.
    let follow_system = {
        let store = cx.global::<ZedisGlobalStore>().read(cx);
        store.theme().is_none() && store.theme_name().is_none()
    };
    if follow_system {
        this.theme_update_task = Some(cx.spawn(async move |_this, cx| {
            cx.update(|cx| {
                restore_default_themes(cx);
                Theme::change(cx.window_appearance(), None, cx);
                apply_default_ui_font_size(cx);
                cx.refresh_windows();
            });
        }));
    }
}
