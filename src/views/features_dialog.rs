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

//! The capability matrix dialog: every probed command with its verdict on
//! the connected server, opened from the status bar's "Limited" badge. The
//! list is a snapshot of the matrix at open time; "Re-probe" discards the
//! cache, runs the probe again and closes the dialog (the badge updates when
//! `FeaturesProbed` fires).

use crate::connection::{CommandStatus, ServerCommand};
use crate::helpers::get_mono_font_family;
use crate::states::{ZedisServerState, i18n_common, i18n_features};
use gpui::{App, Entity, SharedString, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, WindowExt, button::Button, h_flex, label::Label, scroll::ScrollableElement, v_flex,
};
use zedis_ui::ZedisDialog;

pub fn open_features_dialog(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut App) {
    let features = server_state.read(cx).features();
    let intro = i18n_features(cx, "dialog_intro");
    let flavor: SharedString = format!("{}: {}", i18n_features(cx, "flavor"), features.flavor.label()).into();
    let not_probed = (!features.probed).then(|| i18n_features(cx, "not_probed"));
    let rows: Vec<(SharedString, SharedString, CommandStatus)> = ServerCommand::ALL
        .iter()
        .map(|c| {
            let status = features.status(*c);
            (c.label().into(), i18n_features(cx, status.i18n_key()), status)
        })
        .collect();
    let theme = cx.theme();
    let (muted, green, red, yellow) = (theme.muted_foreground, theme.green, theme.red, theme.yellow);
    let mono = get_mono_font_family();
    let reprobe = i18n_features(cx, "reprobe");

    ZedisDialog::new(i18n_features(cx, "dialog_title"))
        .icon(IconName::Info)
        .w(px(560.))
        .child(move || {
            v_flex()
                .gap_2()
                .child(
                    Label::new(intro.clone())
                        .text_sm()
                        .text_color(muted)
                        .whitespace_normal(),
                )
                .child(
                    h_flex()
                        .gap_3()
                        .child(Label::new(flavor.clone()).text_xs().text_color(muted))
                        .when_some(not_probed.clone(), |this, text| {
                            this.child(Label::new(text).text_xs().text_color(yellow))
                        }),
                )
                .child(
                    // A definite height, not `max_h`: `Scrollable` + `max_h`
                    // clips instead of scrolling (see CLAUDE.md).
                    v_flex().h(px(320.)).overflow_y_scrollbar().children(rows.iter().map(
                        |(command, reason, status)| {
                            let slot = div().w(px(18.)).flex_none().flex().justify_center();
                            let slot = match status {
                                CommandStatus::Available => {
                                    slot.text_color(green).child(Icon::new(IconName::Check).text_xs())
                                }
                                CommandStatus::Missing => {
                                    slot.text_color(red).child(Icon::new(IconName::CircleX).text_xs())
                                }
                                CommandStatus::Denied => slot
                                    .text_color(yellow)
                                    .child(Icon::new(IconName::TriangleAlert).text_xs()),
                                CommandStatus::Unknown => slot.text_color(muted).child("○"),
                            };
                            h_flex()
                                .items_center()
                                .gap_2()
                                .py_0p5()
                                .child(slot)
                                .child(
                                    Label::new(command.clone())
                                        .text_sm()
                                        .font_family(mono.clone())
                                        .w(px(170.)),
                                )
                                .child(Label::new(reason.clone()).text_xs().text_color(muted))
                        },
                    )),
                )
        })
        .footer_child(move || {
            let server_state = server_state.clone();
            Button::new("features-dialog-reprobe")
                .outline()
                .label(reprobe.clone())
                .on_click(move |_, window, cx| {
                    server_state.update(cx, |state, cx| state.reprobe_features(cx));
                    window.close_dialog(cx);
                })
        })
        .ok_text(i18n_common(cx, "confirm"))
        .open(window, cx);
}
