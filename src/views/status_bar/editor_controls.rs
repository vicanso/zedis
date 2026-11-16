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

//! The editor's controls the bar carries while a string is open: Soft Wrap,
//! the format and the Viewer, side by side or folded into one button.

use super::*;

/// The Viewer menu's width: its longest choice, the tick and the paddings.
const VIEWER_MENU_WIDTH: f32 = 96.;

/// The viewers a string can be shown in, in the Viewer picker's order.
pub(super) const VIEWER_MODES: [ViewMode; 3] = [ViewMode::Auto, ViewMode::Plain, ViewMode::Hex];

impl ZedisStatusBar {
    pub(super) fn render_editor_settings(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = &self.state.server_state;
        let wrap_fits = self
            .server_state
            .read(cx)
            .value()
            .and_then(|value| value.bytes_value())
            .is_none_or(|bytes| bytes.soft_wrap_fits());
        // Custom variant (transparent bg, muted foreground) so the label + check
        // read in the same recessive status-bar color as everything else — a
        // plain `.ghost()` paints them at the brighter `secondary_foreground`.
        let status_text = cx.theme().muted_foreground;
        Button::new("soft-wrap")
            .ghost()
            .text_color(status_text)
            .xsmall()
            // A value too large to wrap is shown unwrapped whatever the
            // preference: the button says so instead of showing a tick for
            // something that is not happening.
            .when(server_state.soft_wrap && wrap_fits, |this| this.icon(IconName::Check))
            .disabled(!wrap_fits)
            .tooltip(if wrap_fits {
                i18n_status_bar(cx, "soft_wrap_tooltip")
            } else {
                let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                t!(
                    "status_bar.soft_wrap_capped_tooltip",
                    size = humansize::format_size(SOFT_WRAP_MAX_BYTES, humansize::DECIMAL),
                    locale = locale
                )
                .to_string()
                .into()
            })
            .label(i18n_status_bar(cx, "soft_wrap"))
            .on_click(cx.listener(|this, _, _window, cx| this.toggle_soft_wrap(cx)))
    }

    /// Soft Wrap on or off, for this server's sessions from now on.
    fn toggle_soft_wrap(&mut self, cx: &mut Context<Self>) {
        let soft_wrap = !self.state.server_state.soft_wrap;
        self.state.server_state.soft_wrap = soft_wrap;
        self.server_state.update(cx, |state, cx| {
            state.set_soft_wrap(soft_wrap, cx);
        });
        cx.notify();

        let server_id = self.state.server_state.server_id.clone();
        if let Ok(mut option) = get_session_option(server_id.as_str()) {
            option.soft_wrap = Some(soft_wrap);
            save_session_option(server_id.as_str(), option, cx);
        }
    }

    pub(super) fn render_data_format(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(data_format) = self.state.data_format.clone() else {
            return h_flex().into_any_element();
        };
        Button::new("data-format")
            .ghost()
            .disabled(true)
            .text_color(cx.theme().primary)
            .tooltip(i18n_status_bar(cx, "data_format_tooltip"))
            .icon(Icon::new(CustomIconName::Binary))
            .label(data_format)
            .into_any_element()
    }

    pub(super) fn render_viewer_mode(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.state.data_format.is_none() {
            return h_flex();
        };
        let label = i18n_status_bar(cx, "viewer");
        h_flex().child(Label::new(label).mr_1()).child(
            Select::new(&self.viewer_mode_state)
                .appearance(false)
                // Its own width, not the trigger's: the trigger is as wide
                // as "Auto", and the menu cut "Plain" to "Pla…".
                .menu_width(px(VIEWER_MENU_WIDTH)),
        )
    }

    /// The editor's controls folded into one button, where the bar is short
    /// of room for Soft Wrap, the format and the Viewer side by side: the
    /// format's icon and the viewer the value is shown in, with the format
    /// named in its tooltip and at the top of its menu, the viewers and Soft
    /// Wrap below.
    pub(super) fn render_folded_editor_controls(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let format = self.state.data_format.clone().unwrap_or_default();
        let (mode, wrap_fits) = self
            .server_state
            .read(cx)
            .value()
            .and_then(|value| value.bytes_value())
            .map(|bytes| (bytes.view_mode.clone(), bytes.soft_wrap_fits()))
            .unwrap_or((ViewMode::Auto, true));
        let soft_wrap = self.state.server_state.soft_wrap && wrap_fits;
        let format_line: SharedString = format!("{}: {format}", i18n_status_bar(cx, "data_format_tooltip")).into();
        let status_bar = cx.entity().downgrade();
        Button::new("zedis-status-bar-view")
            .ghost()
            .xsmall()
            .compact()
            .text_color(cx.theme().muted_foreground)
            .icon(Icon::new(CustomIconName::Binary))
            .label(mode.as_str())
            // A menu, not a switch: the caret says so.
            .dropdown_caret(true)
            .tooltip(format_line.clone())
            .dropdown_menu(move |menu, _window, cx| {
                let mut menu = menu
                    .min_w(px(VIEWER_MENU_WIDTH * 2.))
                    .label(format_line.clone())
                    .separator()
                    .label(i18n_status_bar(cx, "viewer"));
                for (ix, choice) in VIEWER_MODES.iter().enumerate() {
                    let status_bar = status_bar.clone();
                    menu = menu.item(PopupMenuItem::new(choice.as_str()).checked(*choice == mode).on_click(
                        move |_, window, cx| {
                            let _ = status_bar.update(cx, |this, cx| this.choose_viewer(ix, window, cx));
                        },
                    ));
                }
                let status_bar = status_bar.clone();
                menu.separator().item(
                    PopupMenuItem::new(i18n_status_bar(cx, "soft_wrap"))
                        .checked(soft_wrap)
                        .disabled(!wrap_fits)
                        .on_click(move |_, _window, cx| {
                            let _ = status_bar.update(cx, |this, cx| this.toggle_soft_wrap(cx));
                        }),
                )
            })
    }

    /// Show the value in the viewer at `ix` of the Viewer picker — from the
    /// folded menu, keeping the picker in step for when the bar widens.
    fn choose_viewer(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(choice) = VIEWER_MODES.get(ix) else {
            return;
        };
        self.viewer_mode_state.update(cx, |state, cx| {
            state.set_selected_index(Some(IndexPath::new(ix)), window, cx);
        });
        let name: SharedString = choice.as_str().into();
        self.server_state.update(cx, |state, cx| {
            state.update_bytes_value_view_mode(name, cx);
        });
    }

    /// Soft Wrap, the format label and the Viewer picker belong to the
    /// string/bytes value editor — hidden on tool pages and when no string
    /// value is selected. (The last two used to stay behind on Metrics or
    /// Config once a string had been opened, a control for a page that was
    /// not on screen.)
    pub(super) fn show_editor_controls(&self, cx: &App) -> bool {
        if self.state.data_format.is_none() {
            return false;
        }
        matches!(
            cx.global::<ZedisGlobalStore>().read(cx).route(),
            crate::states::Route::Server {
                view: ServerView::Editor,
                ..
            }
        )
    }
}
