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

/// Redis Slow Log viewer.
///
/// Displays a table of slow-query log entries fetched from the server's
/// periodic `SLOWLOG GET` refresh cycle. Columns: Timestamp, Duration,
/// Command, Client. Rows are sortable by arrival order (newest first).
use crate::connection::{SlowLogEntry, list_commands};
use crate::states::{ServerEvent, ZedisServerState, i18n_common, i18n_slowlog_editor};
use crate::{assets::CustomIconName, constants::SIDEBAR_WIDTH};
use chrono::TimeZone;
use gpui::{ClipboardItem, Edges, Entity, SharedString, Subscription, Window, div, prelude::*, px};
use gpui_component::button::ButtonVariants;
use gpui_component::notification::Notification;
use gpui_component::{
    ActiveTheme, Icon, IconName, StyledExt, WindowExt,
    button::Button,
    h_flex,
    label::Label,
    table::{Column, DataTable, TableDelegate, TableState},
    v_flex,
};
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

/// Set of two-word Redis command names in uppercase (e.g. "CONFIG GET", "SLOWLOG GET").
/// Built once from the full command list so we can correctly split slowlog args.
static TWO_WORD_COMMANDS: OnceLock<HashSet<String>> = OnceLock::new();

fn two_word_commands() -> &'static HashSet<String> {
    TWO_WORD_COMMANDS.get_or_init(|| {
        list_commands("0.0.0")
            .into_iter()
            .filter(|cmd| cmd.contains(' '))
            .map(|cmd| cmd.to_string().to_uppercase())
            .collect()
    })
}

/// A single row in the slowlog table, pre-formatted for display.
#[derive(Clone, Debug)]
struct SlowLogRow {
    timestamp: SharedString,
    duration: SharedString,
    /// The Redis command name (args[0]), e.g. "GET", "HSET".
    command: SharedString,
    /// The arguments following the command (args[1..]), space-joined.
    args: SharedString,
    client: SharedString,
}

impl SlowLogRow {
    fn from_entry(entry: &SlowLogEntry) -> Self {
        let timestamp = chrono::Local
            .timestamp_opt(entry.timestamp, 0)
            .single()
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();

        let micros = entry.duration.as_micros();
        let duration = if micros < 1_000 {
            format!("{}μs", micros)
        } else {
            format!("{:.2}ms", micros as f64 / 1_000.0)
        };

        // Check whether the first two tokens form a known two-word command
        // (e.g. "CONFIG GET", "SLOWLOG GET") before splitting.
        let (command, args) = if entry.args.len() >= 2 {
            let candidate = format!("{} {}", entry.args[0], entry.args[1]).to_uppercase();
            if two_word_commands().contains(&candidate) {
                (candidate, entry.args.get(2..).unwrap_or(&[]).join(" "))
            } else {
                (
                    entry.args[0].to_uppercase(),
                    entry.args.get(1..).unwrap_or(&[]).join(" "),
                )
            }
        } else {
            (
                entry.args.first().map(|s| s.to_uppercase()).unwrap_or_default(),
                String::new(),
            )
        };

        let addr = entry.client_addr.as_deref().unwrap_or("");
        let name = entry.client_name.as_deref().unwrap_or("");
        let client = if !name.is_empty() {
            format!("{addr} ({name})")
        } else {
            addr.to_string()
        };

        Self {
            timestamp: timestamp.into(),
            duration: duration.into(),
            command: command.into(),
            args: args.into(),
            client: client.into(),
        }
    }
}

/// Table delegate for the slowlog row list.
struct SlowlogTableDelegate {
    rows: Arc<Vec<SlowLogRow>>,
    columns: Vec<Column>,
    /// i18n keys corresponding to each column, used to re-translate headers on every render.
    column_keys: Vec<&'static str>,
}

impl SlowlogTableDelegate {
    fn new(rows: Arc<Vec<SlowLogRow>>, window: &mut Window, _cx: &mut gpui::App) -> Self {
        let window_width = window.viewport_size().width;
        let content_width = window_width - SIDEBAR_WIDTH;
        let timestamp_width = 200.;
        let duration_width = 130.;
        let command_width = 120.;
        let client_width = 200.;
        let remaining_width =
            content_width.as_f32() - timestamp_width - duration_width - command_width - client_width - 10.;

        let make_paddings = || {
            Some(Edges {
                top: px(2.),
                bottom: px(2.),
                left: px(10.),
                right: px(10.),
            })
        };

        let column_keys: Vec<&'static str> = vec!["timestamp", "duration", "command", "args", "client"];
        let widths = [
            timestamp_width,
            duration_width,
            command_width,
            remaining_width,
            client_width,
        ];
        let columns = column_keys
            .iter()
            .zip(widths)
            .map(|(&key, width)| {
                Column::new(key, SharedString::default()).width(width).map(|mut col| {
                    col.paddings = make_paddings();
                    col
                })
            })
            .collect();

        Self {
            rows,
            columns,
            column_keys,
        }
    }
}

impl TableDelegate for SlowlogTableDelegate {
    fn columns_count(&self, _cx: &gpui::App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _cx: &gpui::App) -> usize {
        self.rows.len()
    }

    fn column(&self, index: usize, _cx: &gpui::App) -> Column {
        self.columns[index].clone()
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut gpui::Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = &self.columns[col_ix];
        let name = i18n_slowlog_editor(cx, self.column_keys[col_ix]);
        div()
            .size_full()
            .when_some(column.paddings, |this, paddings| this.paddings(paddings))
            .child(
                Label::new(name)
                    .text_align(column.align)
                    .text_color(cx.theme().primary)
                    .text_sm(),
            )
    }

    /// Renders a table cell with a hover copy button.
    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut gpui::Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = &self.columns[col_ix];
        let value: SharedString = if let Some(row) = self.rows.get(row_ix) {
            match col_ix {
                0 => row.timestamp.clone(),
                1 => row.duration.clone(),
                2 => row.command.clone(),
                3 => row.args.clone(),
                4 => row.client.clone(),
                _ => "--".into(),
            }
        } else {
            "--".into()
        };

        let group_name: SharedString = format!("slowlog-td-{}-{}", row_ix, col_ix).into();
        let copied_message = i18n_common(cx, "copied_to_clipboard");
        h_flex()
            .size_full()
            .when_some(column.paddings, |this, paddings| this.paddings(paddings))
            .group(group_name.clone())
            .overflow_hidden()
            .child(
                Label::new(value.clone())
                    .text_align(column.align)
                    .text_ellipsis()
                    .flex_1()
                    .min_w_0(),
            )
            .child(
                div()
                    .id(("copy-wrapper", row_ix * 100 + col_ix))
                    .invisible()
                    .group_hover(group_name, |style| style.visible())
                    .flex_none()
                    .on_click(|_, _, cx: &mut gpui::App| cx.stop_propagation())
                    .child(
                        Button::new(("copy-cell", row_ix * 100 + col_ix))
                            .ghost()
                            .icon(IconName::Copy)
                            .on_click(move |_, window, cx: &mut gpui::App| {
                                cx.write_to_clipboard(ClipboardItem::new_string(value.to_string()));
                                window.push_notification(Notification::info(copied_message.clone()), cx);
                            }),
                    ),
            )
    }

    fn has_more(&self, _cx: &gpui::App) -> bool {
        false
    }

    fn load_more_threshold(&self) -> usize {
        0
    }

    fn load_more(&mut self, _window: &mut Window, _cx: &mut gpui::Context<TableState<Self>>) {}
}

/// Main Slow Log viewer component.
///
/// Layout:
///   1. Toolbar  – entry count label
///   2. Table    – slowlog rows
pub struct ZedisSlowlogEditor {
    server_state: Entity<ZedisServerState>,
    table_state: Entity<TableState<SlowlogTableDelegate>>,
    rows: Arc<Vec<SlowLogRow>>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisSlowlogEditor {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
        let mut subscriptions = Vec::new();

        let rows = Self::build_rows(&server_state, cx);
        let delegate = SlowlogTableDelegate::new(rows.clone(), window, cx);
        let table_state = cx.new(|cx| TableState::new(delegate, window, cx));

        // Refresh table whenever the server sends updated redis info.
        subscriptions.push(cx.subscribe(&server_state, {
            let table_state = table_state.clone();
            move |this, _state, event, cx| {
                if matches!(
                    event,
                    ServerEvent::ServerRedisInfoUpdated | ServerEvent::ServerSelected(_)
                ) {
                    let new_rows = Self::build_rows(&this.server_state, cx);
                    let new_time_stamp = new_rows.first().map(|row| row.timestamp.clone());
                    let current_time_stamp = this.rows.first().map(|row| row.timestamp.clone());
                    if new_time_stamp == current_time_stamp {
                        return;
                    }
                    this.rows = new_rows.clone();
                    table_state.update(cx, |state, _| {
                        state.delegate_mut().rows = new_rows;
                    });
                    cx.notify();
                }
            }
        }));

        Self {
            server_state,
            table_state,
            rows,
            _subscriptions: subscriptions,
        }
    }

    fn build_rows(server_state: &Entity<ZedisServerState>, cx: &gpui::App) -> Arc<Vec<SlowLogRow>> {
        let entries = server_state.read(cx).slow_logs();
        Arc::new(entries.iter().map(SlowLogRow::from_entry).collect())
    }
}

impl gpui::Render for ZedisSlowlogEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let is_empty = self.rows.is_empty();

        v_flex()
            .size_full()
            .overflow_hidden()
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(Icon::new(CustomIconName::Snail))
                    .child(Label::new(i18n_common(cx, "slow_logs")).text_color(cx.theme().foreground))
                    .child(
                        Label::new(format!("({})", self.rows.len()))
                            .text_color(cx.theme().muted_foreground)
                            .text_sm(),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .when(is_empty, |this| {
                        this.child(div().size_full().flex().items_center().justify_center().child(
                            Label::new(i18n_slowlog_editor(cx, "no_slowlogs")).text_color(cx.theme().muted_foreground),
                        ))
                    })
                    .when(!is_empty, |this| {
                        this.child(
                            DataTable::new(&self.table_state)
                                .stripe(true)
                                .bordered(false)
                                .scrollbar_visible(true, true),
                        )
                    }),
            )
            .into_any_element()
    }
}
