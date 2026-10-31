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

//! Inferred Keyspace Schema & Data Dictionary dialog.

use super::{format_memory, format_thousands};
use crate::assets::CustomIconName;
use crate::helpers::{get_mono_font_family, saves_files};
use crate::states::{ZedisServerState, i18n_common, i18n_memory_analysis};
use crate::views::export_to_file;
use gpui::{App, ClipboardItem, Entity, FontWeight, SharedString, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Sizable, WindowExt, button::Button, h_flex, label::Label, notification::Notification,
    scroll::ScrollableElement, v_flex,
};
use zedis_core::schema::{DataDictionary, PatternSchema};
use zedis_ui::ZedisDialog;

pub(crate) const SCHEMA_BTN_ID: &str = "memory-analysis-schema-btn";
pub(crate) const SCHEMA_COPY_MD_BTN_ID: &str = "schema-copy-md-btn";
pub(crate) const SCHEMA_EXPORT_MD_BTN_ID: &str = "schema-export-md-btn";
pub(crate) const SCHEMA_EXPORT_JSON_BTN_ID: &str = "schema-export-json-btn";

const DIALOG_WIDTH: f32 = 760.0;

pub struct ZedisDataDictionaryDialog {
    dict: DataDictionary,
    context_label: String,
    server_state: Entity<ZedisServerState>,
}

impl ZedisDataDictionaryDialog {
    pub fn new(dict: DataDictionary, context_label: String, server_state: Entity<ZedisServerState>) -> Self {
        Self {
            dict,
            context_label,
            server_state,
        }
    }

    fn render_pattern_card(&self, idx: usize, schema: &PatternSchema, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let border = theme.border;
        let muted = theme.muted_foreground;
        let mono = get_mono_font_family();
        let type_str = schema.type_summary();
        let enc_str = schema.encoding_summary();
        let type_badge = if enc_str == "-" {
            type_str
        } else {
            format!("{type_str} · {enc_str}")
        };
        let stats_summary = format!(
            "~{} keys · ~{}",
            format_thousands(schema.estimated_keys),
            format_memory(schema.estimated_memory_bytes)
        );
        let size_line = format!(
            "p50: {} · p95: {} · max: {}",
            format_memory(schema.p50_bytes),
            format_memory(schema.p95_bytes),
            format_memory(schema.max_bytes),
        );
        let ttl_line = format!("TTL: {}", schema.ttl_policy.summary());
        let examples = schema.examples.join(", ");
        let fields_label = i18n_memory_analysis(cx, "schema_col_fields");

        v_flex()
            .id(("schema-pattern-card", idx))
            .w_full()
            .p_2p5()
            .gap_1p5()
            .rounded(theme.radius)
            .border_1()
            .border_color(border)
            .bg(theme.muted.opacity(0.25))
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Label::new(SharedString::from(schema.pattern.clone()))
                                    .font_family(mono.clone())
                                    .font_weight(FontWeight::BOLD)
                                    .text_sm(),
                            )
                            .child(
                                div()
                                    .px_1p5()
                                    .py_0p5()
                                    .rounded(px(4.))
                                    .bg(theme.primary.opacity(0.12))
                                    .child(
                                        Label::new(SharedString::from(type_badge))
                                            .font_family(mono.clone())
                                            .text_xs()
                                            .text_color(theme.primary),
                                    ),
                            ),
                    )
                    .child(
                        Label::new(SharedString::from(stats_summary))
                            .font_family(mono.clone())
                            .text_xs()
                            .text_color(muted),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        Label::new(SharedString::from(size_line))
                            .font_family(mono.clone())
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(
                        Label::new(SharedString::from(ttl_line))
                            .font_family(mono.clone())
                            .text_xs()
                            .text_color(muted),
                    ),
            )
            .when(!schema.fields.is_empty(), |el| {
                let mono_field = mono.clone();
                let chips: Vec<_> = schema
                    .fields
                    .iter()
                    .map(|f| {
                        let text = format!("{}: {}", f.name, f.value_types.join("|"));
                        div()
                            .px_1p5()
                            .py_0p5()
                            .rounded(px(4.))
                            .border_1()
                            .border_color(border)
                            .child(
                                Label::new(SharedString::from(text))
                                    .font_family(mono_field.clone())
                                    .text_xs(),
                            )
                    })
                    .collect();
                el.child(
                    h_flex()
                        .w_full()
                        .flex_wrap()
                        .items_center()
                        .gap_1()
                        .child(Label::new(fields_label).text_xs().text_color(muted))
                        .children(chips),
                )
            })
            .when(!examples.is_empty(), |el| {
                el.child(
                    Label::new(SharedString::from(format!("e.g. {examples}")))
                        .font_family(mono)
                        .text_xs()
                        .text_color(muted),
                )
            })
    }
}

impl Render for ZedisDataDictionaryDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let cards: Vec<_> = self
            .dict
            .patterns
            .iter()
            .enumerate()
            .map(|(idx, p)| self.render_pattern_card(idx, p, cx).into_any_element())
            .collect();

        v_flex()
            .w_full()
            .gap_3()
            .child(
                Label::new(i18n_memory_analysis(cx, "schema_dialog_subtitle"))
                    .text_xs()
                    .text_color(muted),
            )
            .child(
                v_flex()
                    .id("schema-patterns-scroll")
                    .w_full()
                    .h(px(400.))
                    .overflow_y_scrollbar()
                    .gap_2()
                    .children(cards),
            )
    }
}

struct SchemaDialogActions {
    body: Entity<ZedisDataDictionaryDialog>,
}

impl Render for SchemaDialogActions {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_2()
            .child(
                Button::new(SCHEMA_COPY_MD_BTN_ID)
                    .outline()
                    .small()
                    .icon(IconName::Copy)
                    .label(i18n_memory_analysis(cx, "schema_copy_md"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        let dialog = this.body.read(cx);
                        let md = dialog.dict.to_markdown(&dialog.context_label);
                        cx.write_to_clipboard(ClipboardItem::new_string(md));
                        window.push_notification(Notification::info(i18n_common(cx, "copied")), cx);
                    })),
            )
            .when(saves_files(), |row| {
                row.child(
                    Button::new(SCHEMA_EXPORT_MD_BTN_ID)
                        .outline()
                        .small()
                        .icon(Icon::new(CustomIconName::Download))
                        .label(i18n_memory_analysis(cx, "export_data_dictionary_md"))
                        .on_click(cx.listener(|this, _, _window, cx| {
                            let (md, server_state) = {
                                let dialog = this.body.read(cx);
                                (
                                    dialog.dict.to_markdown(&dialog.context_label),
                                    dialog.server_state.clone(),
                                )
                            };
                            let success = i18n_common(cx, "csv_exported");
                            let error = i18n_common(cx, "csv_export_failed");
                            export_to_file(cx, server_state, md.into_bytes(), "data-dictionary.md", success, error);
                        })),
                )
                .child(
                    Button::new(SCHEMA_EXPORT_JSON_BTN_ID)
                        .outline()
                        .small()
                        .icon(Icon::new(CustomIconName::Download))
                        .label(i18n_memory_analysis(cx, "export_data_dictionary_json"))
                        .on_click(cx.listener(|this, _, _window, cx| {
                            let (json, server_state) = {
                                let dialog = this.body.read(cx);
                                (
                                    dialog.dict.to_json_pretty(&dialog.context_label),
                                    dialog.server_state.clone(),
                                )
                            };
                            let success = i18n_common(cx, "csv_exported");
                            let error = i18n_common(cx, "csv_export_failed");
                            export_to_file(
                                cx,
                                server_state,
                                json.into_bytes(),
                                "data-dictionary.json",
                                success,
                                error,
                            );
                        })),
                )
            })
    }
}

pub fn open_data_dictionary_dialog(
    dict: DataDictionary,
    context_label: String,
    server_state: Entity<ZedisServerState>,
    window: &mut Window,
    cx: &mut App,
) {
    let body = cx.new(|_| ZedisDataDictionaryDialog::new(dict, context_label, server_state));
    let actions = cx.new(|_| SchemaDialogActions { body: body.clone() });
    ZedisDialog::new(i18n_memory_analysis(cx, "schema_dialog_title"))
        .icon(IconName::BookOpen)
        .w(px(DIALOG_WIDTH))
        .child(move || body.clone())
        .footer_child(move || actions.clone())
        .open(window, cx);
}
