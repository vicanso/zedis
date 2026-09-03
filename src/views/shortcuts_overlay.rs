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

//! ⌘/ keyboard-shortcuts reference overlay: a read-only, grouped table
//! of the user-facing hotkeys. Mirrors the command-palette overlay
//! (absolute, full-size backdrop + centred panel; Esc / backdrop-click
//! to close). Data comes from [`shortcut_reference`]; keystrokes render
//! through [`humanize_keystroke`] for per-platform symbols.

use crate::helpers::{humanize_keystroke, shortcut_reference};
use crate::states::i18n_shortcuts;
use gpui::{Context, FocusHandle, Focusable, KeyDownEvent, ScrollHandle, Window, div, prelude::*, px};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::{ActiveTheme, StyledExt, h_flex, label::Label, v_flex};

pub struct ZedisShortcutsOverlay {
    open: bool,
    focus_handle: FocusHandle,
    /// Set when opened; the next `render` consumes it to focus the
    /// backdrop so it captures Esc. `toggle` runs from a focus-
    /// independent global action handler with no `Window`, so focusing
    /// is deferred to render (same pattern as the command palette).
    pending_focus: bool,
    /// Scroll handle for the (possibly overflowing) shortcuts list.
    scroll_handle: ScrollHandle,
}

impl ZedisShortcutsOverlay {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            open: false,
            focus_handle: cx.focus_handle(),
            pending_focus: false,
            scroll_handle: ScrollHandle::new(),
        }
    }

    /// Open the overlay (or close it if already open). Invoked from the
    /// global `ShortcutsAction` handler (no `Window`), so the focus grab
    /// is deferred to the next render via `pending_focus`.
    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        self.open = !self.open;
        if self.open {
            self.pending_focus = true;
            // The ScrollHandle keeps its offset across open/close; reset
            // so the list starts at the top each time.
            self.scroll_handle.set_offset(gpui::Point::default());
        }
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = false;
        // The open overlay holds focus; the closed render drops that
        // element, orphaning focus. Blur returns it to the window root
        // so the ⌘/ keybinding keeps a dispatch path to reopen.
        window.blur(cx);
        cx.notify();
    }
}

impl Focusable for ZedisShortcutsOverlay {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ZedisShortcutsOverlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            // Zero-footprint when closed.
            return div().into_any_element();
        }

        // Deferred from `toggle` (global action handler has no Window):
        // focus the backdrop so it captures Esc on the first render.
        if self.pending_focus {
            self.pending_focus = false;
            self.focus_handle.focus(window, cx);
        }

        let theme = cx.theme();
        let panel_bg = theme.background;
        let border = theme.border;
        let muted = theme.muted_foreground;
        let fg = theme.foreground;
        let chip_bg = theme.muted;
        let radius = theme.radius;
        let radius_lg = theme.radius_lg;

        // Grouped list: a muted section heading, then a row per binding
        // (description on the left, keystroke chip on the right).
        let mut list = v_flex()
            .id("zedis-shortcuts-list")
            .w_full()
            .gap_3()
            .p_3()
            .max_h(px(420.))
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle);
        for group in shortcut_reference() {
            let mut section = v_flex().w_full().gap_1().child(
                Label::new(i18n_shortcuts(cx, group.title_key))
                    .text_xs()
                    .text_color(muted),
            );
            for (keystroke, desc_key) in group.items {
                section = section.child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_between()
                        .gap_4()
                        .py_0p5()
                        .child(Label::new(i18n_shortcuts(cx, desc_key)).text_sm().text_color(fg))
                        .child(
                            div()
                                .px_1p5()
                                .py_0p5()
                                .rounded(radius)
                                .bg(chip_bg)
                                .border_1()
                                .border_color(border)
                                .child(Label::new(humanize_keystroke(keystroke)).text_xs().text_color(fg)),
                        ),
                );
            }
            list = list.child(section);
        }

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .justify_center()
            // Top-align so the panel stays content-sized rather than
            // stretching to its max height (mirrors the palette).
            .items_start()
            .bg(gpui::hsla(0., 0., 0., 0.4))
            .track_focus(&self.focus_handle)
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    // Click on the dim backdrop closes.
                    this.close(window, cx);
                }),
            )
            .capture_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key.as_str() == "escape" {
                    this.close(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                v_flex()
                    .mt(px(96.))
                    .w(px(480.))
                    .max_h(px(520.))
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
                            .px_3()
                            .py_2()
                            .border_b_1()
                            .border_color(border)
                            .child(Label::new(i18n_shortcuts(cx, "title")).font_semibold()),
                    )
                    .child(
                        // Relative wrapper so the absolutely-positioned
                        // scrollbar overlays the list and stays a sibling
                        // of the scroller (doesn't scroll with content).
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
