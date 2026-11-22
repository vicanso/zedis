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

use super::*;

struct ConfigGroupSection<'a> {
    label: SharedString,
    desc: SharedString,
    configs: &'a [(SharedString, SharedString)],
    cols: u16,
    font_family: &'a SharedString,
    docs: &'a ConfigDocMap,
}

impl ZedisConfigEditor {
    /// Renders a single configuration group header (accent bar, title, description,
    /// count) over a responsive grid of parameter cards.
    fn render_group(&self, group: ConfigGroupSection<'_>, cx: &mut Context<Self>) -> gpui::AnyElement {
        let ConfigGroupSection {
            label,
            desc,
            configs,
            cols,
            font_family,
            docs,
        } = group;
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        // The theme's primary, same accent the sidebar and key tree use for their
        // selection bars — so a named theme restyles all three together.
        let accent_bar = cx.theme().primary;

        let mut cards: Vec<gpui::AnyElement> = Vec::with_capacity(configs.len());
        for (k, v) in configs {
            cards.push(self.render_card(k.clone(), v.clone(), font_family, docs, cx));
        }

        let group_id = SharedString::from(format!("config-group-{label}"));
        v_flex()
            .id(group_id)
            .w_full()
            .gap_1()
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .pb_2()
                    .border_b_1()
                    .border_color(border)
                    .child(div().w(px(3.)).h(px(15.)).rounded_sm().bg(accent_bar))
                    .child(
                        Label::new(label)
                            .font_family(font_family.clone())
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(fg),
                    )
                    .child(Label::new(desc).text_xs().text_color(muted))
                    .child(div().flex_1())
                    .child(
                        div()
                            .px_2()
                            .rounded_full()
                            .border_1()
                            .border_color(border)
                            .child(Label::new(configs.len().to_string()).text_xs().text_color(muted)),
                    ),
            )
            .child(div().grid().grid_cols(cols).items_start().gap_x_6().children(cards))
            .into_any_element()
    }
}

impl Render for ZedisConfigEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(notification) = self.pending_notification.take() {
            window.push_notification(notification, cx);
        }

        let font_family: SharedString = get_mono_font_family().into();
        let grouped = self.grouped_configs();
        let (buckets, others) = (&grouped.buckets, &grouped.others);

        // `stripe_bg` is still used by the cross-server diff view below.
        let stripe_bg = cx.theme().table_even;

        // Columns of rows, by the content-width proxy: a row wants its name
        // and a readable stretch of value, so fewer columns than cards did.
        let cols: u16 = cx
            .global::<ZedisGlobalStore>()
            .read(cx)
            .content_width()
            .map(|w| {
                let w = w.as_f32();
                if w > 1200. {
                    3
                } else if w > 600. {
                    2
                } else {
                    1
                }
            })
            .unwrap_or(1);

        v_flex()
            .size_full()
            .overflow_hidden()
            .track_focus(&self.focus_handle)
            // While editing, declare the `ConfigEdit` key context so `escape`
            // maps to Cancel — deeper than the workspace's back binding, so it
            // wins; with no edit active there's no context and `escape` returns
            // to the editor as usual. Requires the editor to hold focus (the
            // edit handler focuses the input or this root).
            .when(self.editing_key.is_some(), |this| this.key_context("ConfigEdit"))
            .on_action(cx.listener(|this, _: &ConfigEditAction, _window, cx| {
                this.editing_key = None;
                this.enum_select = None;
                cx.notify();
            }))
            .child(
                panel_header()
                    .gap_2()
                    .child(
                        Button::new("config-back")
                            .ghost()
                            .small()
                            .icon(IconName::ArrowLeft)
                            .tooltip(back_to_editor_tooltip(cx))
                            .on_click(|_, _w, cx| {
                                cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                    store.update(cx, |state, cx| state.go_to_view(ServerView::Editor, cx));
                                });
                            }),
                    )
                    .child(
                        Label::new(i18n_config_editor(cx, "title"))
                            .text_sm()
                            .font_family(font_family.clone()),
                    )
                    // Inline loading indicator in the header — more noticeable
                    // than a centered body spinner while `CONFIG GET *` is slow
                    // (covers both the initial load and a cross-server compare).
                    .when(self.loading || self.comparing, |this| {
                        this.child(
                            h_flex()
                                .items_center()
                                .gap_1p5()
                                .child(Spinner::new().with_size(px(14.)).color(cx.theme().muted_foreground))
                                .child(
                                    Label::new(i18n_common(cx, "loading"))
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground),
                                ),
                        )
                    })
                    // CONFIG SET missing / denied on this server: say so up
                    // front instead of silently hiding every pencil.
                    .when_some(
                        self.server_state.read(cx).blocked_by(Capability::ConfigWrite),
                        |this, (command, status)| this.child(unavailable_chip(cx, command, status)),
                    )
                    // Whether an edit here survives a restart. A server with
                    // no config file can only be told at runtime, and saying
                    // so beats a button that is guaranteed to error.
                    .when(self.config_file.is_empty() && !self.loading, |this| {
                        this.child(
                            Label::new(i18n_config_editor(cx, "runtime_only_hint"))
                                .text_xs()
                                .text_color(cx.theme().muted_foreground),
                        )
                    })
                    .child(div().flex_1())
                    .when(
                        !self.config_file.is_empty()
                            && self.server_state.read(cx).can(Capability::ConfigWrite)
                            && self
                                .server_state
                                .read(cx)
                                .command_block(ServerCommand::ConfigRewrite)
                                .is_none(),
                        |this| {
                            let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
                            let tooltip = t!(
                                "config_editor.rewrite_tooltip",
                                path = self.config_file.as_ref(),
                                locale = &locale
                            )
                            .to_string();
                            this.child(
                                Button::new("config-rewrite")
                                    .small()
                                    .outline()
                                    .icon(Icon::new(CustomIconName::Save))
                                    .label(i18n_config_editor(cx, "rewrite"))
                                    .tooltip(tooltip)
                                    .on_click(cx.listener(|this, _, window, cx| this.open_rewrite_dialog(window, cx))),
                            )
                        },
                    )
                    .child(
                        Button::new("config-reload")
                            .small()
                            .ghost()
                            .icon(Icon::new(CustomIconName::RotateCw))
                            .tooltip(i18n_common(cx, "reload"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.editing_key = None;
                                this.enum_select = None;
                                this.load_configs(cx);
                            })),
                    )
                    .child(
                        Button::new("config-compare")
                            .small()
                            .ghost()
                            .icon(Icon::new(CustomIconName::GitCompareArrows))
                            .tooltip(i18n_config_editor(cx, "compare_tooltip"))
                            .on_click(cx.listener(|this, _, window, cx| this.open_compare_dialog(window, cx))),
                    )
                    .when(self.diff.is_some(), |this| {
                        this.child(
                            Button::new("config-exit-diff")
                                .small()
                                .ghost()
                                .label(i18n_config_editor(cx, "exit_diff"))
                                .on_click(cx.listener(|this, _, _, cx| this.close_diff(cx))),
                        )
                    })
                    .child(div().w(px(200.0)).child(Input::new(&self.filter_state).small())),
            )
            .child(if let Some(diff) = &self.diff {
                let border = cx.theme().border;
                let muted = cx.theme().muted_foreground;
                let fg = cx.theme().foreground;
                let hover_bg = cx.theme().table_hover;
                let filter = self.filter.to_lowercase();
                let filtered_diff: Vec<&(SharedString, SharedString, SharedString)> = diff
                    .rows
                    .iter()
                    .filter(|(k, _, _)| filter.is_empty() || k.to_lowercase().contains(&filter))
                    .collect();
                if filtered_diff.is_empty() {
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Label::new(i18n_config_editor(cx, "no_diff")).text_color(muted))
                        .into_any_element()
                } else {
                    let header_row = h_flex()
                        .w_full()
                        .px_3()
                        .py_1()
                        .gap_2()
                        .border_b_1()
                        .border_color(border)
                        .child(
                            div()
                                .w(px(280.0))
                                .flex_none()
                                .child(Label::new(i18n_config_editor(cx, "param")).text_xs().text_color(muted)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .child(Label::new(diff.local_label.clone()).text_xs().text_color(muted)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .child(Label::new(diff.other_label.clone()).text_xs().text_color(muted)),
                        );
                    let diff_rows = filtered_diff.into_iter().enumerate().map(move |(i, (key, lv, ov))| {
                        let is_stripe = i % 2 != 0;
                        h_flex()
                            .id(("config-diff-row", i))
                            .w_full()
                            .px_3()
                            .py_1()
                            .gap_2()
                            .border_b_1()
                            .border_color(border)
                            .when(is_stripe, |this| this.bg(stripe_bg))
                            .hover(move |this| this.bg(hover_bg))
                            .child(
                                div()
                                    .w(px(280.0))
                                    .flex_none()
                                    .child(Label::new(key.clone()).text_sm().text_color(fg)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .overflow_hidden()
                                    .child(Label::new(lv.clone()).text_sm().text_ellipsis().text_color(muted)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .overflow_hidden()
                                    .child(Label::new(ov.clone()).text_sm().text_ellipsis().text_color(fg)),
                            )
                    });
                    div()
                        .id("config-diff-body")
                        .flex_1()
                        .w_full()
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(header_row)
                        .children(diff_rows)
                        .into_any_element()
                }
            } else if self.configs.is_empty() && !self.loading {
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(if let Some(err) = &self.error {
                        Label::new(err.clone()).text_color(cx.theme().red)
                    } else {
                        Label::new(i18n_config_editor(cx, "no_data")).text_color(cx.theme().muted_foreground)
                    })
                    .into_any_element()
            } else {
                // View-scoped map: loaded in `new`, not on every scroll/repaint.
                self.refresh_docs_if_locale_changed(cx);
                let mut sections = v_flex().w_full().gap_4().px_4().py_3();
                for (i, group) in CONFIG_GROUPS.iter().enumerate() {
                    if buckets[i].is_empty() {
                        continue;
                    }
                    sections = sections.child(self.render_group(
                        ConfigGroupSection {
                            label: SharedString::from(group.label),
                            desc: i18n_config_editor(cx, group.desc_key),
                            configs: &buckets[i],
                            cols,
                            font_family: &font_family,
                            docs: &self.config_docs,
                        },
                        cx,
                    ));
                }
                if !others.is_empty() {
                    sections = sections.child(self.render_group(
                        ConfigGroupSection {
                            label: i18n_config_editor(cx, "group_others"),
                            desc: i18n_config_editor(cx, "group_others_desc"),
                            configs: others,
                            cols,
                            font_family: &font_family,
                            docs: &self.config_docs,
                        },
                        cx,
                    ));
                }
                div()
                    .id("config-editor-body")
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(sections)
                    .into_any_element()
            })
    }
}
