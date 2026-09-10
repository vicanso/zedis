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

//! The change log of a collection key, and the structured diff it implies.
//!
//! Two tables. "Net changes" is what is different now compared with before
//! the first edit — built by reducing the log to per-field before and after
//! (`change_log::net_snapshots`) and comparing those with `kv_diff`, so a
//! field edited and then edited back does not appear at all. "Every change"
//! is the log itself, oldest first.
//!
//! A List gets only the second table. Its elements are addressed by
//! position, and positions shift with every push and removal, so netting
//! "#3 before" against "#3 after" would compare two different elements and
//! present the result as a change to one of them.

use crate::helpers::format_unix_secs;
use crate::states::{KeyType, ZedisGlobalStore, ZedisServerState, dialog_button_props, i18n_common, i18n_editor};
use gpui::{App, Entity, SharedString, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme,
    label::Label,
    table::{DataTable, TableState},
    v_flex,
};
use rust_i18n::t;
use std::collections::VecDeque;
use zedis_core::change_log::{ChangeEntry, ChangeKind, net_snapshots, operation_count};
use zedis_core::diff::{KvDelta, kv_diff};
use zedis_ui::{TextColumn, ZedisDialog, ZedisTextTable};

const DIALOG_WIDTH: f32 = 880.0;
const TABLE_HEIGHT: f32 = 220.0;
/// Sized for the longest label across locales — "Modification" (fr header),
/// "Hinzugefügt" (de) — plus the 10px cell padding on each side and the
/// net table's sort arrow. 90 truncated even the English "Added".
const KIND_WIDTH: f32 = 140.0;
/// A full date and time in the widest configured layout
/// ("09/10/2026 02:30:00 PM"), plus padding.
const TIME_WIDTH: f32 = 200.0;
const TARGET_WIDTH: f32 = 170.0;
const VALUE_WIDTH: f32 = 160.0;

/// A cell for a value that may be absent. Set membership is stored as an
/// empty string, which reads as nothing at all — the dash says so plainly.
fn shown(value: Option<&str>) -> SharedString {
    match value {
        None | Some("") => "—".into(),
        Some(text) => text.to_string().into(),
    }
}

fn delta_label(delta: KvDelta, cx: &App) -> SharedString {
    match delta {
        KvDelta::Added => i18n_editor(cx, "change_kind_added"),
        KvDelta::Removed => i18n_editor(cx, "change_kind_removed"),
        KvDelta::Changed => i18n_editor(cx, "change_kind_changed"),
    }
}

/// An entry's own kind: an absent `old` is an addition, an absent `new` a
/// removal, and a whole-key operation is labelled as one.
fn entry_label(entry: &ChangeEntry, cx: &App) -> SharedString {
    match entry.kind {
        ChangeKind::Operation => i18n_editor(cx, "change_kind_operation"),
        ChangeKind::Element => match (&entry.old, &entry.new) {
            (None, _) => i18n_editor(cx, "change_kind_added"),
            (_, None) => i18n_editor(cx, "change_kind_removed"),
            _ => i18n_editor(cx, "change_kind_changed"),
        },
    }
}

fn text_table(columns: Vec<TextColumn>, rows: Vec<Vec<SharedString>>, cx: &App) -> ZedisTextTable {
    let mut table = ZedisTextTable::new(columns, i18n_common(cx, "copied_to_clipboard"))
        .copy_tooltip(i18n_common(cx, "copy_cell_tooltip"));
    table.set_rows(rows);
    table
}

pub struct ZedisChangeLogDialog {
    /// Absent for a List; see the module docs.
    net: Option<Entity<TableState<ZedisTextTable>>>,
    all: Entity<TableState<ZedisTextTable>>,
    operations: usize,
    is_list: bool,
}

impl ZedisChangeLogDialog {
    pub fn new(key_type: KeyType, log: VecDeque<ChangeEntry>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let is_list = key_type == KeyType::List;

        let net = if is_list {
            None
        } else {
            let (old, new) = net_snapshots(&log);
            let rows = kv_diff(&old, &new)
                .into_iter()
                .map(|entry| {
                    vec![
                        delta_label(entry.delta, cx),
                        entry.key.into(),
                        shown(entry.old.as_deref()),
                        shown(entry.new.as_deref()),
                    ]
                })
                .collect();
            let columns = vec![
                TextColumn::new("kind", i18n_editor(cx, "change_col_kind"), KIND_WIDTH).sortable(),
                TextColumn::new("target", i18n_editor(cx, "change_col_target"), TARGET_WIDTH).sortable(),
                TextColumn::new("before", i18n_editor(cx, "change_col_before"), VALUE_WIDTH),
                TextColumn::new("after", i18n_editor(cx, "change_col_after"), VALUE_WIDTH),
            ];
            let table = text_table(columns, rows, cx);
            Some(cx.new(|cx| TableState::new(table, window, cx)))
        };

        let rows = log
            .iter()
            .map(|entry| {
                vec![
                    format_unix_secs(entry.at).unwrap_or_default().into(),
                    entry_label(entry, cx),
                    entry.target.clone().into(),
                    shown(entry.old.as_deref()),
                    shown(entry.new.as_deref()),
                ]
            })
            .collect();
        let columns = vec![
            TextColumn::new("time", i18n_editor(cx, "change_col_time"), TIME_WIDTH),
            TextColumn::new("kind", i18n_editor(cx, "change_col_kind"), KIND_WIDTH),
            TextColumn::new("target", i18n_editor(cx, "change_col_target"), TARGET_WIDTH),
            TextColumn::new("before", i18n_editor(cx, "change_col_before"), VALUE_WIDTH),
            TextColumn::new("after", i18n_editor(cx, "change_col_after"), VALUE_WIDTH),
        ];
        let table = text_table(columns, rows, cx);
        let all = cx.new(|cx| TableState::new(table, window, cx));

        Self {
            net,
            all,
            operations: operation_count(&log),
            is_list,
        }
    }
}

impl Render for ZedisChangeLogDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let mut body = v_flex().w_full().gap_2().child(
            Label::new(i18n_editor(cx, "change_log_hint"))
                .text_xs()
                .text_color(muted),
        );

        if let Some(net) = &self.net {
            body = body
                .child(Label::new(i18n_editor(cx, "change_log_net")).text_sm())
                .child(
                    div()
                        .w_full()
                        .h(px(TABLE_HEIGHT))
                        .child(DataTable::new(net).bordered(false)),
                );
            // Operations are in the log below but could not be netted — say
            // so, rather than let the net table look complete.
            if self.operations > 0 {
                body = body.child(
                    Label::new(
                        t!(
                            "editor.change_log_operations_note",
                            count = self.operations,
                            locale = locale
                        )
                        .to_string(),
                    )
                    .text_xs()
                    .text_color(muted),
                );
            }
        } else if self.is_list {
            body = body.child(
                Label::new(i18n_editor(cx, "change_log_list_hint"))
                    .text_xs()
                    .text_color(muted),
            );
        }

        body.child(Label::new(i18n_editor(cx, "change_log_all")).text_sm())
            .child(
                div()
                    .w_full()
                    .h(px(TABLE_HEIGHT))
                    .child(DataTable::new(&self.all).bordered(false)),
            )
    }
}

/// Open the change log for the selected key. Does nothing when there is no
/// key or nothing was recorded — the menu entry is hidden in that case too.
pub fn open_change_log_dialog(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut App) {
    let state = server_state.read(cx);
    let Some(key) = state.key() else {
        return;
    };
    let Some(log) = state.change_log_for(&key).cloned() else {
        return;
    };
    let key_type = state.value().map(|v| v.key_type()).unwrap_or(KeyType::Unknown);

    let view = cx.new(|cx| ZedisChangeLogDialog::new(key_type, log, window, cx));
    let body = view.clone();
    let close = i18n_editor(cx, "change_log_close");
    ZedisDialog::new(format!("{} — {key}", i18n_editor(cx, "change_log")))
        .w(px(DIALOG_WIDTH))
        .ok_text(close.clone())
        .button_props(dialog_button_props(cx).ok_text(close))
        .child(move || body.clone())
        .on_ok(|_, _, _| true)
        .open(window, cx);
}
