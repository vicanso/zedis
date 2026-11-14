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

//! Dialog for `ZMPOP` (Redis 7.0+) on a Sorted Set key.

use crate::states::{ZedisServerState, dialog_button_props, i18n_common, i18n_zset_editor};
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

struct ZsetMpopDialog {
    pop_max: bool,
    count_input: Entity<InputState>,
    extra_keys_input: Entity<InputState>,
}

impl ZsetMpopDialog {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let count_input = cx.new(|cx| InputState::new(window, cx).default_value("1"));
        let extra_keys_input = cx.new(|cx| InputState::new(window, cx));
        Self {
            pop_max: false,
            count_input,
            extra_keys_input,
        }
    }

    fn submit(view: &Entity<Self>, server_state: &Entity<ZedisServerState>, window: &mut Window, cx: &mut App) -> bool {
        let (pop_max, count_str, raw_extra) = {
            let d = view.read(cx);
            (
                d.pop_max,
                d.count_input.read(cx).value().trim().to_string(),
                d.extra_keys_input.read(cx).value().to_string(),
            )
        };
        let Ok(count) = count_str.parse::<usize>() else {
            window.push_notification(Notification::warning(i18n_zset_editor(cx, "zmpop_count")), cx);
            return false;
        };
        if count == 0 {
            window.push_notification(Notification::warning(i18n_zset_editor(cx, "zmpop_count")), cx);
            return false;
        }
        let extra_keys: Vec<SharedString> = raw_extra
            .split([',', ' '])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| SharedString::from(s.to_string()))
            .collect();
        server_state.update(cx, |state, cx| {
            state.mpop_zset_members(extra_keys, pop_max, count, cx);
        });
        true
    }
}

impl Render for ZsetMpopDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let dir_idx = usize::from(self.pop_max);

        v_flex()
            .gap_3()
            .w_full()
            .child(
                RadioGroup::horizontal("zset-mpop-dir")
                    .selected_index(Some(dir_idx))
                    .child(Radio::new("zmpop-min").label(i18n_zset_editor(cx, "zmpop_min")))
                    .child(Radio::new("zmpop-max").label(i18n_zset_editor(cx, "zmpop_max")))
                    .on_click(cx.listener(|this, idx: &usize, _, cx| {
                        this.pop_max = *idx == 1;
                        cx.notify();
                    })),
            )
            .child(
                h_flex()
                    .gap_2()
                    .w_full()
                    .child(
                        v_flex()
                            .w(px(120.))
                            .gap_1()
                            .child(
                                Label::new(i18n_zset_editor(cx, "zmpop_count"))
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
                                Label::new(i18n_zset_editor(cx, "zmpop_extra_keys"))
                                    .text_xs()
                                    .text_color(muted),
                            )
                            .child(Input::new(&self.extra_keys_input)),
                    ),
            )
    }
}

pub(crate) fn open_zmpop_dialog(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| ZsetMpopDialog::new(window, cx));
    let body = view.clone();
    let initial_focus = view.read(cx).count_input.clone();
    let ok_label = i18n_common(cx, "confirm");
    let cancel_label = i18n_common(cx, "cancel");

    ZedisDialog::new(i18n_zset_editor(cx, "zmpop_title"))
        .w(px(440.))
        .ok_text(ok_label.clone())
        .cancel_text(cancel_label.clone())
        .button_props(dialog_button_props(cx).ok_text(ok_label).cancel_text(cancel_label))
        .child(move || body.clone())
        .on_ok(move |_, window, cx| ZsetMpopDialog::submit(&view, &server_state, window, cx))
        .open(window, cx);
    initial_focus.update(cx, |field, cx| field.focus(window, cx));
}
