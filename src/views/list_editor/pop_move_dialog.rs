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

//! Dialog for `LMOVE` (Redis 6.2+) and `LMPOP` (Redis 7.0+) on a List key.

use crate::connection::{FromEnd, floors};
use crate::states::{ZedisServerState, dialog_button_props, i18n_common, i18n_list_editor};
use gpui::{App, AppContext, Context, Entity, SharedString, Window, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, WindowExt, h_flex,
    input::{Input, InputState},
    label::Label,
    notification::Notification,
    radio::{Radio, RadioGroup},
    v_flex,
};
use zedis_ui::ZedisDialog;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ListPopMoveMode {
    Move,
    Mpop,
}

struct ListPopMoveDialog {
    supports_lmpop: bool,
    mode: ListPopMoveMode,
    from_side: FromEnd,
    to_side: FromEnd,
    destination_input: Entity<InputState>,
    extra_keys_input: Entity<InputState>,
    count_input: Entity<InputState>,
}

impl ListPopMoveDialog {
    fn new(supports_lmpop: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let destination_input = cx.new(|cx| InputState::new(window, cx));
        let extra_keys_input = cx.new(|cx| InputState::new(window, cx));
        let count_input = cx.new(|cx| InputState::new(window, cx).default_value("1"));
        Self {
            supports_lmpop,
            mode: ListPopMoveMode::Move,
            from_side: FromEnd::Head,
            to_side: FromEnd::Tail,
            destination_input,
            extra_keys_input,
            count_input,
        }
    }

    fn submit(view: &Entity<Self>, server_state: &Entity<ZedisServerState>, window: &mut Window, cx: &mut App) -> bool {
        let (mode, from_side, to_side, dest, count_str, raw_extra) = {
            let d = view.read(cx);
            (
                d.mode,
                d.from_side,
                d.to_side,
                d.destination_input.read(cx).value().trim().to_string(),
                d.count_input.read(cx).value().trim().to_string(),
                d.extra_keys_input.read(cx).value().to_string(),
            )
        };
        match mode {
            ListPopMoveMode::Move => {
                if dest.is_empty() {
                    window.push_notification(Notification::warning(i18n_list_editor(cx, "destination_key")), cx);
                    return false;
                }
                server_state.update(cx, |state, cx| {
                    state.move_list_element(dest.into(), from_side, to_side, cx);
                });
                true
            }
            ListPopMoveMode::Mpop => {
                let Ok(count) = count_str.parse::<usize>() else {
                    window.push_notification(Notification::warning(i18n_list_editor(cx, "pop_count")), cx);
                    return false;
                };
                if count == 0 {
                    window.push_notification(Notification::warning(i18n_list_editor(cx, "pop_count")), cx);
                    return false;
                }
                let extra_keys: Vec<SharedString> = raw_extra
                    .split([',', ' '])
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| SharedString::from(s.to_string()))
                    .collect();
                server_state.update(cx, |state, cx| {
                    state.mpop_list_elements(extra_keys, from_side, count, cx);
                });
                true
            }
        }
    }
}

impl Render for ListPopMoveDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let is_move = self.mode == ListPopMoveMode::Move;
        let from_idx = match self.from_side {
            FromEnd::Head => 0,
            FromEnd::Tail => 1,
        };
        let to_idx = match self.to_side {
            FromEnd::Head => 0,
            FromEnd::Tail => 1,
        };

        v_flex()
            .gap_3()
            .w_full()
            .when(self.supports_lmpop, |el| {
                el.child(
                    RadioGroup::horizontal("list-pop-move-mode")
                        .selected_index(Some(if is_move { 0 } else { 1 }))
                        .child(Radio::new("lmove").label(i18n_list_editor(cx, "mode_lmove")))
                        .child(Radio::new("lmpop").label(i18n_list_editor(cx, "mode_lmpop")))
                        .on_click(cx.listener(|this, idx: &usize, _, cx| {
                            this.mode = if *idx == 0 {
                                ListPopMoveMode::Move
                            } else {
                                ListPopMoveMode::Mpop
                            };
                            cx.notify();
                        })),
                )
            })
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new(i18n_list_editor(cx, "from_side"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(
                        RadioGroup::horizontal("list-pop-from-side")
                            .selected_index(Some(from_idx))
                            .child(Radio::new("from-left").label(i18n_list_editor(cx, "side_left")))
                            .child(Radio::new("from-right").label(i18n_list_editor(cx, "side_right")))
                            .on_click(cx.listener(|this, idx: &usize, _, cx| {
                                this.from_side = if *idx == 0 { FromEnd::Head } else { FromEnd::Tail };
                                cx.notify();
                            })),
                    ),
            )
            .when(is_move, |el| {
                el.child(
                    v_flex()
                        .gap_1()
                        .child(
                            Label::new(i18n_list_editor(cx, "destination_key"))
                                .text_xs()
                                .text_color(muted),
                        )
                        .child(Input::new(&self.destination_input)),
                )
                .child(
                    v_flex()
                        .gap_1()
                        .child(Label::new(i18n_list_editor(cx, "to_side")).text_xs().text_color(muted))
                        .child(
                            RadioGroup::horizontal("list-pop-to-side")
                                .selected_index(Some(to_idx))
                                .child(Radio::new("to-left").label(i18n_list_editor(cx, "side_left")))
                                .child(Radio::new("to-right").label(i18n_list_editor(cx, "side_right")))
                                .on_click(cx.listener(|this, idx: &usize, _, cx| {
                                    this.to_side = if *idx == 0 { FromEnd::Head } else { FromEnd::Tail };
                                    cx.notify();
                                })),
                        ),
                )
            })
            .when(!is_move, |el| {
                el.child(
                    h_flex()
                        .gap_2()
                        .w_full()
                        .child(
                            v_flex()
                                .w(px(120.))
                                .gap_1()
                                .child(
                                    Label::new(i18n_list_editor(cx, "pop_count"))
                                        .text_xs()
                                        .text_color(muted),
                                )
                                .child(Input::new(&self.count_input)),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .gap_1()
                                .child(
                                    Label::new(i18n_list_editor(cx, "extra_keys"))
                                        .text_xs()
                                        .text_color(muted),
                                )
                                .child(Input::new(&self.extra_keys_input)),
                        ),
                )
            })
    }
}

pub(super) fn open_list_pop_move_dialog(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut App) {
    let supports_lmpop = server_state.read(cx).supports(floors::LMPOP);
    let view = cx.new(|cx| ListPopMoveDialog::new(supports_lmpop, window, cx));
    let body = view.clone();
    let initial_focus = view.read(cx).destination_input.clone();
    let ok_label = i18n_common(cx, "confirm");
    let cancel_label = i18n_common(cx, "cancel");

    ZedisDialog::new(i18n_list_editor(cx, "move_or_mpop"))
        .w(px(440.))
        .ok_text(ok_label.clone())
        .cancel_text(cancel_label.clone())
        .button_props(dialog_button_props(cx).ok_text(ok_label).cancel_text(cancel_label))
        .child(move || body.clone())
        .on_ok(move |_, window, cx| ListPopMoveDialog::submit(&view, &server_state, window, cx))
        .open(window, cx);
    initial_focus.update(cx, |field, cx| field.focus(window, cx));
}
