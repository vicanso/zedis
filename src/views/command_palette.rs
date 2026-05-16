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

//! ⌘K command palette: fuzzy-search configured servers and
//! navigation commands, keyboard-driven. Phase 1 scope — keys/settings
//! deep-search deliberately out of scope (per-server lazy SCAN state
//! needs its own design pass).

use crate::connection::get_servers;
use crate::helpers::fuzzy_score;
use crate::states::{Route, ZedisGlobalStore, i18n_command_palette};
use gpui::{Context, FocusHandle, Focusable, KeyDownEvent, Window, div, prelude::*, px};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, label::Label, v_flex};

/// What activating a palette row does.
#[derive(Clone)]
enum PaletteCommand {
    /// Connect to / switch to a configured server (by id).
    Server(String),
    /// Navigate to a route.
    Route(Route),
}

struct PaletteItem {
    label: gpui::SharedString,
    /// Secondary muted text (host:port for servers, empty for commands).
    hint: gpui::SharedString,
    /// String the fuzzy query is scored against.
    search: String,
    command: PaletteCommand,
}

pub struct ZedisCommandPalette {
    open: bool,
    query: gpui::Entity<gpui_component::input::InputState>,
    selected: usize,
    focus_handle: FocusHandle,
}

impl ZedisCommandPalette {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| gpui_component::input::InputState::new(window, cx));
        Self {
            open: false,
            query,
            selected: 0,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Open the palette (or close it if already open). Resets the
    /// query and selection and focuses the search field on open.
    pub fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = !self.open;
        if self.open {
            self.selected = 0;
            self.query.update(cx, |state, cx| {
                state.set_value(gpui::SharedString::default(), window, cx);
                state.focus(window, cx);
            });
        }
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        cx.notify();
    }

    /// Build the full (unfiltered) candidate list: configured servers
    /// first, then navigation commands.
    fn build_items(&self, cx: &Context<Self>) -> Vec<PaletteItem> {
        let mut items: Vec<PaletteItem> = Vec::new();

        for server in get_servers().unwrap_or_default() {
            let hint = format!("{}:{}", server.host, server.port);
            // Match against name + address so "10.0" finds a server too.
            let search = format!("{} {hint}", server.name);
            items.push(PaletteItem {
                label: server.name.clone().into(),
                hint: hint.into(),
                search,
                command: PaletteCommand::Server(server.id.clone()),
            });
        }

        // (i18n key, route) — order defines empty-query display order.
        let commands: [(&str, Route); 13] = [
            ("cmd_home", Route::Home),
            ("cmd_editor", Route::Editor),
            ("cmd_metrics", Route::Metrics),
            ("cmd_performance", Route::Slowlog),
            ("cmd_memory", Route::MemoryAnalysis),
            ("cmd_clients", Route::Clients),
            ("cmd_monitor", Route::Monitor),
            ("cmd_config", Route::Config),
            ("cmd_acl", Route::Acl),
            ("cmd_search", Route::Search),
            ("cmd_functions", Route::Functions),
            ("cmd_lua_scripts", Route::LuaScripts),
            ("cmd_settings", Route::Settings),
        ];
        for (key, route) in commands {
            let label = i18n_command_palette(cx, key);
            items.push(PaletteItem {
                label: label.clone(),
                hint: gpui::SharedString::default(),
                search: label.to_string(),
                command: PaletteCommand::Route(route),
            });
        }

        items
    }

    /// Filter+rank `items` by the current query. Returns indices into
    /// `items`, best match first. Empty query keeps the natural order.
    fn ranked(&self, items: &[PaletteItem], cx: &Context<Self>) -> Vec<usize> {
        let query = self.query.read(cx).value().to_string();
        let mut scored: Vec<(i32, usize)> = items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| fuzzy_score(&query, &it.search).map(|s| (s, i)))
            .collect();
        // Stable sort, higher score first. Equal scores (e.g. empty
        // query → all 0) keep insertion order (sort_by_key is stable).
        scored.sort_by_key(|b| std::cmp::Reverse(b.0));
        scored.into_iter().map(|(_, i)| i).collect()
    }

    fn execute(&mut self, command: &PaletteCommand, cx: &mut Context<Self>) {
        let command = command.clone();
        cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
            store.update(cx, |state, cx| match command {
                PaletteCommand::Server(id) => {
                    state.go_to(Route::Editor, cx);
                    state.set_selected_server((id, 0), cx);
                }
                PaletteCommand::Route(route) => {
                    state.go_to(route, cx);
                }
            });
        });
        self.close(cx);
    }
}

impl Focusable for ZedisCommandPalette {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ZedisCommandPalette {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            // Zero-footprint when closed.
            return div().into_any_element();
        }

        let items = self.build_items(cx);
        let order = self.ranked(&items, cx);
        let count = order.len();
        // Clamp selection to the filtered list.
        let selected = if count == 0 { 0 } else { self.selected.min(count - 1) };

        let theme = cx.theme();
        let panel_bg = theme.background;
        let border = theme.border;
        let active = theme.list_active;
        let muted = theme.muted_foreground;
        let radius = theme.radius;
        let radius_lg = theme.radius_lg;

        let mut list = v_flex().w_full().gap_0p5();
        if count == 0 {
            list = list.child(
                Label::new(i18n_command_palette(cx, "empty"))
                    .text_sm()
                    .text_color(muted)
                    .p_2(),
            );
        } else {
            for (row, &item_idx) in order.iter().enumerate() {
                let it = &items[item_idx];
                let is_sel = row == selected;
                let mut r = gpui_component::h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .rounded(radius)
                    .child(Label::new(it.label.clone()).text_sm())
                    .when(!it.hint.is_empty(), |this| {
                        this.child(Label::new(it.hint.clone()).text_xs().text_color(muted))
                    });
                if is_sel {
                    r = r.bg(active);
                }
                list = list.child(r);
            }
        }

        // Snapshot the resolved command for the keyboard handler so it
        // doesn't need to re-rank inside the listener.
        let chosen: Option<PaletteCommand> = order.get(selected).map(|&i| items[i].command.clone());

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .justify_center()
            .bg(gpui::hsla(0., 0., 0., 0.4))
            .track_focus(&self.focus_handle)
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    // Click on the dim backdrop closes.
                    this.close(cx);
                }),
            )
            .capture_key_down(cx.listener(move |this, event: &KeyDownEvent, _window, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => {
                        this.close(cx);
                        cx.stop_propagation();
                    }
                    "down" => {
                        if count > 0 {
                            this.selected = (selected + 1).min(count - 1);
                            cx.notify();
                        }
                        cx.stop_propagation();
                    }
                    "up" => {
                        this.selected = selected.saturating_sub(1);
                        cx.notify();
                        cx.stop_propagation();
                    }
                    "enter" => {
                        if let Some(cmd) = &chosen {
                            this.execute(cmd, cx);
                        }
                        cx.stop_propagation();
                    }
                    // Everything else (typing) falls through to the input.
                    _ => {}
                }
            }))
            .child(
                v_flex()
                    .mt(px(96.))
                    .w(px(560.))
                    .max_h(px(440.))
                    .bg(panel_bg)
                    .border_1()
                    .border_color(border)
                    .rounded(radius_lg)
                    .shadow_lg()
                    .overflow_hidden()
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx: &mut gpui::App| {
                        // Clicks inside the panel must not bubble to the
                        // backdrop close handler.
                        cx.stop_propagation();
                    })
                    .child(
                        div()
                            .p_2()
                            .border_b_1()
                            .border_color(border)
                            .child(gpui_component::input::Input::new(&self.query)),
                    )
                    .child(div().p_1().overflow_y_scrollbar().child(list)),
            )
            .into_any_element()
    }
}
