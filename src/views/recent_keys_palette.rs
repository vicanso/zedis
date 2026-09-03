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

//! ⌘P recent-keys palette: pick from the current connection's MRU list
//! (same data as the key-tree "Recently Opened" submenu). Mirrors the
//! command-palette overlay model — global toggle, Esc/backdrop close,
//! ↑↓/Enter navigation — but only lists recent keys.

use crate::db::{get_recent_keys_manager, recent_keys_scope};
use crate::helpers::fuzzy_score;
use crate::states::{Route, ServerView, ZedisGlobalStore, ZedisServerState, i18n_recent_keys_palette};
use gpui::{Context, FocusHandle, Focusable, KeyDownEvent, ScrollHandle, Window, div, prelude::*, px};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::{ActiveTheme, label::Label, v_flex};
use std::mem::take;

pub struct ZedisRecentKeysPalette {
    server_state: gpui::Entity<ZedisServerState>,
    /// Snapshot of MRU keys taken when the palette opens.
    recent: Vec<gpui::SharedString>,
    /// Whether a server connection is in context (not Home/Settings).
    in_server_context: bool,
    open: bool,
    query: gpui::Entity<gpui_kit::component::input::InputState>,
    selected: usize,
    focus_handle: FocusHandle,
    pending_focus: bool,
    /// Focus that was live when the palette opened; handed back on close.
    prev_focus: Option<FocusHandle>,
    /// Set on close; the next (closed) render restores `prev_focus` — or
    /// blurs when there is none. Deferred because `toggle` has no `Window`.
    pending_restore: bool,
    scroll_handle: ScrollHandle,
}

impl ZedisRecentKeysPalette {
    pub fn new(server_state: gpui::Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| {
            gpui_kit::component::input::InputState::new(window, cx)
                .placeholder(i18n_recent_keys_palette(cx, "search_placeholder"))
        });
        Self {
            server_state,
            recent: Vec::new(),
            in_server_context: false,
            open: false,
            query,
            selected: 0,
            focus_handle: cx.focus_handle(),
            pending_focus: false,
            prev_focus: None,
            pending_restore: false,
            scroll_handle: ScrollHandle::new(),
        }
    }

    /// Rebind to another tab's server state (the root swaps this on
    /// workspace-tab switch so the palette lists the active tab's keys).
    pub fn set_server_state(&mut self, server_state: gpui::Entity<ZedisServerState>) {
        self.server_state = server_state;
    }

    /// Open (or close if already open). Input reset/focus is deferred to
    /// `render` because the global action handler has no `Window`.
    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        self.open = !self.open;
        if self.open {
            self.selected = 0;
            self.pending_focus = true;
            self.scroll_handle.set_offset(gpui::Point::default());
            let (has_server, current_route) = {
                let state = cx.global::<ZedisGlobalStore>().read(cx);
                (state.selected_server().is_some(), state.route())
            };
            self.in_server_context = has_server && !matches!(current_route, Route::Home | Route::Settings);
            self.recent = if self.in_server_context {
                let s = self.server_state.read(cx);
                let scope = recent_keys_scope(s.server_id(), s.db());
                get_recent_keys_manager()
                    .records(&scope)
                    .unwrap_or_default()
                    .into_iter()
                    .map(Into::into)
                    .collect()
            } else {
                Vec::new()
            };
        } else {
            self.pending_restore = true;
        }
        cx.notify();
    }

    /// Close after jumping to a key: the route just changed, so the
    /// pre-open focus target may be gone — blur instead of restoring
    /// (an orphaned focus handle would kill keyboard dispatch entirely).
    fn close(&mut self, cx: &mut Context<Self>) {
        self.prev_focus = None;
        self.dismiss(cx);
    }

    /// Close and hand focus back to whatever had it before the palette
    /// opened (Esc / backdrop / ⌘P re-toggle). A bare `blur()` leaves the
    /// window with nothing focused, so every focus-routed keybinding dies
    /// until the next click. The restore runs in the next (closed) render
    /// pass, which — unlike `toggle` — has a `Window`.
    fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.pending_restore = true;
        cx.notify();
    }

    /// Ranked indices into `self.recent`. Empty query keeps MRU order.
    fn ranked(&self, query: &str) -> Vec<usize> {
        let mut scored: Vec<(i32, usize)> = self
            .recent
            .iter()
            .enumerate()
            .filter_map(|(i, key)| fuzzy_score(query, key).map(|s| (s, i)))
            .collect();
        scored.sort_by_key(|b| std::cmp::Reverse(b.0));
        scored.into_iter().map(|(_, i)| i).collect()
    }

    fn execute(&mut self, key: &gpui::SharedString, cx: &mut Context<Self>) {
        let key = key.clone();
        self.server_state.update(cx, |state, cx| state.select_key(key, cx));
        cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
            store.update(cx, |state, cx| state.go_to_view(ServerView::Editor, cx));
        });
        self.close(cx);
    }
}

impl Focusable for ZedisRecentKeysPalette {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ZedisRecentKeysPalette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            if take(&mut self.pending_restore) {
                match self.prev_focus.take() {
                    Some(prev) => prev.focus(window, cx),
                    None => window.blur(cx),
                }
            }
            return div().into_any_element();
        }

        if self.pending_focus {
            self.pending_focus = false;
            // Remember the pre-open focus so a dismissal can hand it back.
            self.prev_focus = window.focused(cx);
            self.query.update(cx, |state, cx| {
                state.set_value(gpui::SharedString::default(), window, cx);
                state.focus(window, cx);
            });
        }

        let query_str = self.query.read(cx).value().trim().to_string();
        let order = self.ranked(&query_str);
        let count = order.len();
        let selected = if count == 0 { 0 } else { self.selected.min(count - 1) };

        let theme = cx.theme();
        let panel_bg = theme.background;
        let border = theme.border;
        let active = theme.list_active;
        let muted = theme.muted_foreground;
        let radius = theme.radius;
        let radius_lg = theme.radius_lg;

        let empty_label = if !self.in_server_context {
            i18n_recent_keys_palette(cx, "no_connection")
        } else if self.recent.is_empty() {
            i18n_recent_keys_palette(cx, "empty")
        } else {
            i18n_recent_keys_palette(cx, "no_matches")
        };

        let mut list = v_flex()
            .id("zedis-recent-keys-list")
            .w_full()
            .gap_0p5()
            .p_1()
            .max_h(px(360.))
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle);

        if count == 0 {
            list = list.child(Label::new(empty_label).text_sm().text_color(muted).p_2());
        } else {
            for (row, &item_idx) in order.iter().enumerate() {
                let key = self.recent[item_idx].clone();
                let is_sel = row == selected;
                let key_for_click = key.clone();
                let mut r = gpui_kit::component::h_flex()
                    .id(("zedis-recent-keys-row", row))
                    .w_full()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .rounded(radius)
                    .cursor_pointer()
                    .hover(|this| this.bg(active))
                    .child(
                        Label::new(key)
                            .text_sm()
                            .font_family(crate::helpers::get_mono_font_family()),
                    )
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _, _window, cx| {
                            this.execute(&key_for_click, cx);
                            cx.stop_propagation();
                        }),
                    );
                if is_sel {
                    r = r.bg(active);
                }
                list = list.child(r);
            }
        }

        let chosen: Option<gpui::SharedString> = order.get(selected).map(|&i| self.recent[i].clone());

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .justify_center()
            .items_start()
            .bg(gpui::hsla(0., 0., 0., 0.4))
            .track_focus(&self.focus_handle)
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    this.dismiss(cx);
                }),
            )
            .capture_key_down(cx.listener(move |this, event: &KeyDownEvent, _window, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => {
                        this.dismiss(cx);
                        cx.stop_propagation();
                    }
                    "down" => {
                        if count > 0 {
                            this.selected = (selected + 1).min(count - 1);
                            this.scroll_handle.scroll_to_item(this.selected);
                            cx.notify();
                        }
                        cx.stop_propagation();
                    }
                    "up" => {
                        this.selected = selected.saturating_sub(1);
                        this.scroll_handle.scroll_to_item(this.selected);
                        cx.notify();
                        cx.stop_propagation();
                    }
                    "enter" => {
                        if let Some(key) = &chosen {
                            this.execute(key, cx);
                        }
                        cx.stop_propagation();
                    }
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
                        cx.stop_propagation();
                    })
                    .child(
                        div()
                            .p_2()
                            .border_b_1()
                            .border_color(border)
                            .child(
                                Label::new(i18n_recent_keys_palette(cx, "title"))
                                    .text_xs()
                                    .text_color(muted)
                                    .mb_1(),
                            )
                            .child(gpui_kit::component::input::Input::new(&self.query)),
                    )
                    .child(
                        div().relative().child(list).child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .right_0()
                                .bottom_0()
                                .child(Scrollbar::vertical(&self.scroll_handle).mode(ScrollbarMode::Always)),
                        ),
                    ),
            )
            .into_any_element()
    }
}
