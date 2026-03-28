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

use crate::{
    components::ZedisKvFetcher,
    components::{KvTableColumn, KvTableMode},
    helpers::{fast_contains_ignore_case, format_duration},
    states::{KeyType, RedisValue, ServerEvent, StreamInfoData, ZedisServerState, i18n_kv_table, i18n_stream_editor},
    views::{ZedisKvTable, kv_table::FOOTER_HEIGHT},
};
use gpui::{App, Entity, SharedString, Subscription, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, IconName,
    button::Button,
    h_flex,
    label::Label,
    scroll::ScrollableElement,
    table::{Column, DataTable, TableDelegate, TableState},
    v_flex,
};
use std::sync::Arc;
use std::time::Duration;
use zedis_ui::ZedisFormFieldType;

/// Manages Redis Stream values and their display state.
///
/// Handles both filtered and unfiltered views of stream data, maintaining
/// a mapping between visible items and their original indices when filtering.
struct ZedisStreamValues {
    /// Currently visible entry indices (filtered subset or all entries)
    visible_entry_indexes: Vec<usize>,
    /// Maps visible entry indices to original stream entry indices (Some when filtered, None otherwise)
    visible_item_indexes: Option<Vec<usize>>,
    /// The underlying Redis value data
    value: RedisValue,
    /// Field names for the stream
    fields: Vec<SharedString>,
    /// Reference to server state for performing operations
    server_state: Entity<ZedisServerState>,
}

impl ZedisStreamValues {
    /// Recalculates visible entries based on the current keyword filter.
    ///
    /// When a keyword is present:
    /// - Filters entries by checking if any field value contains the keyword (case-insensitive)
    /// - Maintains index mapping to original positions
    ///
    /// When no keyword:
    /// - Shows all entries directly
    /// - Clears index mapping
    fn recalc_visible_items(&mut self) {
        let Some(stream_value) = self.value.stream_value() else {
            return;
        };

        let keyword = stream_value.keyword.clone().unwrap_or_default().to_lowercase();

        // No filter: show all entries
        if keyword.is_empty() {
            self.visible_entry_indexes = (0..stream_value.values.len()).collect();
            self.visible_item_indexes = None;
            return;
        }

        // Filter entries by keyword (search in entry_id and all field values)
        let capacity = stream_value.values.len().max(100) / 10;
        let mut visible_item_indexes = Vec::with_capacity(capacity);
        let mut visible_entry_indexes = Vec::with_capacity(capacity);

        for (index, (entry_id, values)) in stream_value.values.iter().enumerate() {
            // Check entry_id
            if fast_contains_ignore_case(entry_id.as_str(), &keyword) {
                visible_item_indexes.push(index);
                visible_entry_indexes.push(index);
                continue;
            }

            // Check all field values
            let mut found = false;
            for (_, value) in values.iter() {
                if fast_contains_ignore_case(value.as_str(), &keyword) {
                    found = true;
                    break;
                }
            }

            if found {
                visible_item_indexes.push(index);
                visible_entry_indexes.push(index);
            }
        }

        self.visible_entry_indexes = visible_entry_indexes;
        self.visible_item_indexes = Some(visible_item_indexes);
    }
}

impl ZedisKvFetcher for ZedisStreamValues {
    fn key_type(&self) -> KeyType {
        KeyType::Stream
    }
    fn fields_required(&self) -> bool {
        false
    }
    fn include_field_names(&self) -> bool {
        true
    }
    fn support_add_fields(&self) -> bool {
        true
    }
    fn primary_index(&self) -> usize {
        1
    }
    fn readonly_on_edit(&self) -> bool {
        true
    }
    fn columns(&self) -> Option<Vec<KvTableColumn>> {
        Some(
            self.fields
                .iter()
                .enumerate()
                .map(|(index, field)| {
                    if index == 0 {
                        KvTableColumn::new_auto_created("Entry Id")
                    } else {
                        KvTableColumn::new(field.as_str(), None).field_type(ZedisFormFieldType::Editor)
                    }
                })
                .collect(),
        )
    }
    fn new(server_state: Entity<ZedisServerState>, value: RedisValue) -> Self {
        let fields = value.stream_fields();
        let mut this = Self {
            server_state,
            value,
            fields,
            visible_entry_indexes: Vec::default(),
            visible_item_indexes: None,
        };

        this.recalc_visible_items();
        this
    }

    /// Retrieves the value at the specified row and column index.
    ///
    /// Returns from the filtered visible entries when a keyword filter is active,
    /// otherwise returns directly from the original stream values.
    fn get(&self, row_ix: usize, col_ix: usize) -> Option<SharedString> {
        let stream_value = self.value.stream_value()?;
        if col_ix == 0 {
            return None;
        }

        // Map visible row index to real entry index
        let real_row_ix = *self.visible_entry_indexes.get(row_ix)?;

        if col_ix == 1 {
            return stream_value.get_entry_id(real_row_ix);
        }
        let field = self.fields.get(col_ix - 1)?;
        stream_value.get_field_value(real_row_ix, field)
    }

    /// Returns the total count of entries in the Redis stream (from XLEN).
    fn count(&self) -> usize {
        self.value.stream_value().map_or(0, |v| v.size)
    }

    /// Returns the number of currently visible rows.
    ///
    /// When filtered, returns the count of matching entries.
    /// Otherwise, returns the count of loaded entries.
    fn rows_count(&self) -> usize {
        self.visible_entry_indexes.len()
    }

    fn is_done(&self) -> bool {
        self.value.stream_value().is_some_and(|v| v.done)
    }

    fn load_more(&self, _window: &mut Window, cx: &mut App) {
        self.server_state.update(cx, |this, cx| {
            this.load_more_stream_value(cx);
        });
    }

    /// Removes the entry at the specified visible index.
    ///
    /// When a filter is active, maps the visible index to the real index
    /// in the underlying stream before performing the deletion (XDEL command).
    fn remove(&self, index: usize, cx: &mut App) {
        let Some(stream) = self.value.stream_value() else {
            return;
        };

        // Map visible index to real index
        let real_index = *self.visible_entry_indexes.get(index).unwrap_or(&index);

        let Some(entry_id) = stream.get_entry_id(real_index) else {
            return;
        };
        self.server_state.update(cx, |this, cx| {
            this.remove_stream_value(entry_id, cx);
        });
    }

    /// Applies a keyword filter to the stream entries.
    ///
    /// Searches only within already loaded entries for matching entry IDs or field values.
    fn filter(&self, keyword: SharedString, cx: &mut App) {
        self.server_state.update(cx, |state, cx| {
            state.filter_stream_value(keyword, cx);
        });
    }

    fn support_reverse(&self) -> bool {
        true
    }

    fn current_reverse(&self) -> bool {
        self.value.stream_value().is_some_and(|v| v.reverse)
    }

    fn toggle_reverse(&self, reverse: bool, cx: &mut App) {
        self.server_state.update(cx, |state, cx| {
            state.reload_stream_value(reverse, cx);
        });
    }

    fn handle_update_value(&self, _row_ix: usize, _values: Vec<SharedString>, _window: &mut Window, _cx: &mut App) {}

    fn handle_add_value(&self, values: Vec<SharedString>, _window: &mut Window, cx: &mut App) {
        let mut field_values = Vec::with_capacity(values.len() / 2);
        let mut iter = values.into_iter();

        while let (Some(key), Some(value)) = (iter.next(), iter.next()) {
            field_values.push((key, value));
        }

        let entry_id = field_values
            .first()
            .map(|(_, value)| value.clone())
            .filter(|value| !value.is_empty());

        let field_values: Vec<(SharedString, SharedString)> = field_values
            .into_iter()
            .skip(1)
            .filter(|(name, value)| !name.is_empty() && !value.is_empty())
            .collect();

        self.server_state.update(cx, |this, cx| {
            this.add_stream_value(entry_id, field_values, cx);
        });
    }
}

// ─── Simple flat-data table delegate ────────────────────────────────────────

/// A lightweight `TableDelegate` backed by static column names and row data.
/// Used to render Stream XINFO tables (groups, consumers, pending entries).
struct SimpleTableDelegate {
    column_names: Vec<SharedString>,
    columns: Vec<Column>,
    rows: Vec<Vec<SharedString>>,
}

impl SimpleTableDelegate {
    /// `cols` is a list of `(name, width)` pairs; pass `None` for flexible width.
    fn new(cols: Vec<(SharedString, Option<f32>)>, rows: Vec<Vec<SharedString>>) -> Self {
        let column_names = cols.iter().map(|(n, _)| n.clone()).collect();
        let columns = cols
            .iter()
            .map(|(n, w)| {
                let col = Column::new(n.clone(), n.clone());
                if let Some(w) = w { col.width(*w) } else { col }
            })
            .collect();
        Self {
            column_names,
            columns,
            rows,
        }
    }
}

impl TableDelegate for SimpleTableDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }
    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }
    fn column(&self, ix: usize, _: &App) -> Column {
        self.columns[ix].clone()
    }
    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut gpui::Context<TableState<Self>>,
    ) -> impl gpui::IntoElement {
        let name = self.column_names.get(col_ix).cloned().unwrap_or_default();
        h_flex()
            .size_full()
            .px_2()
            .child(Label::new(name).text_sm().text_color(cx.theme().primary))
    }
    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        _: &mut gpui::Context<TableState<Self>>,
    ) -> impl gpui::IntoElement {
        let value = self
            .rows
            .get(row_ix)
            .and_then(|r| r.get(col_ix))
            .cloned()
            .unwrap_or_default();
        h_flex()
            .size_full()
            .px_2()
            .child(Label::new(value).text_xs().text_ellipsis())
    }
    fn has_more(&self, _: &App) -> bool {
        false
    }
}

// ─── Stream editor ───────────────────────────────────────────────────────────

pub struct ZedisStreamEditor {
    table_state: Entity<ZedisKvTable<ZedisStreamValues>>,
    server_state: Entity<ZedisServerState>,
    /// Whether the auxiliary info view (XINFO) is currently shown.
    is_info_view: bool,
    /// Merged groups+consumers table (Group | Last-ID | Lag | Consumer | Pending | Idle )
    groups_consumers_table: Option<Entity<TableState<SimpleTableDelegate>>>,
    /// Pending-entries table (Group | Entry ID | Consumer | Idle | Deliveries)
    pending_table: Option<Entity<TableState<SimpleTableDelegate>>>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisStreamEditor {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let fields = if let Some(values) = server_state.read(cx).value() {
            values.stream_fields()
        } else {
            vec![]
        };

        let table_state = cx.new(|cx| {
            ZedisKvTable::<ZedisStreamValues>::new(
                fields
                    .iter()
                    .enumerate()
                    .map(|(index, field)| {
                        if index == 0 {
                            KvTableColumn::new_auto_created("Entry Id")
                        } else {
                            KvTableColumn::new(field.as_str(), None).field_type(ZedisFormFieldType::Editor)
                        }
                    })
                    .collect(),
                server_state.clone(),
                window,
                cx,
            )
            .mode(KvTableMode::ADD | KvTableMode::REMOVE | KvTableMode::FILTER)
        });

        // Register the info-view toggle button in the kv table footer.
        // The factory is called each render so icon/tooltip reflect current state.
        let weak = cx.weak_entity();
        table_state.update(cx, |table, _cx| {
            let weak = weak.clone();
            table.set_action_button_factory(Box::new(move |_window, cx| {
                let Some(editor) = weak.upgrade() else {
                    return vec![];
                };
                let is_info = editor.read(cx).is_info_view;
                let icon = if is_info {
                    IconName::LayoutDashboard
                } else {
                    IconName::Info
                };
                let tooltip: SharedString = i18n_kv_table(cx, if is_info { "data_tooltip" } else { "info_tooltip" });
                let weak_click = weak.clone();
                vec![
                    Button::new("info-view-btn")
                        .icon(icon)
                        .tooltip(tooltip)
                        .on_click(move |_, _, cx| {
                            if let Some(editor) = weak_click.upgrade() {
                                editor.update(cx, |this, cx| {
                                    this.is_info_view = !this.is_info_view;
                                    if this.is_info_view {
                                        this.server_state.update(cx, |s, cx| s.fetch_stream_info(cx));
                                    }
                                    cx.notify();
                                });
                            }
                        }),
                ]
            }));
        });

        // Rebuild info tables when fresh XINFO data arrives; reset them on key change
        let sub = cx.subscribe_in(
            &server_state,
            window,
            |this, server_state, event: &ServerEvent, window, cx| match event {
                ServerEvent::ValueUpdated => {
                    let stream_info = server_state
                        .read(cx)
                        .value()
                        .and_then(|v| v.stream_value())
                        .and_then(|sv| sv.info.clone());
                    if let Some(info) = stream_info {
                        this.rebuild_info_tables(&info, window, cx);
                    }
                    cx.notify();
                }
                ServerEvent::KeySelected(_) => {
                    this.is_info_view = false;
                    this.groups_consumers_table = None;
                    this.pending_table = None;
                    cx.notify();
                }
                _ => {}
            },
        );

        Self {
            table_state,
            server_state,
            is_info_view: false,
            groups_consumers_table: None,
            pending_table: None,
            _subscriptions: vec![sub],
        }
    }

    /// Rebuilds the two info DataTables from freshly-fetched `StreamInfoData`.
    fn rebuild_info_tables(&mut self, info: &Arc<StreamInfoData>, window: &mut Window, cx: &mut Context<Self>) {
        // Merged groups + consumers table.
        // One row per consumer; groups without consumers get a single placeholder row.
        // Columns: Group | Last Delivered ID | Lag | Consumer | Pending | Idle
        let gc_columns = vec![
            (i18n_stream_editor(cx, "col_group"), Some(140.)),
            (i18n_stream_editor(cx, "col_last_delivered_id"), Some(160.)),
            (i18n_stream_editor(cx, "col_lag"), Some(60.)),
            (i18n_stream_editor(cx, "col_consumer"), Some(140.)),
            (i18n_stream_editor(cx, "col_pending"), Some(80.)),
            (i18n_stream_editor(cx, "col_idle_ms"), None),
        ];
        let gc_rows: Vec<Vec<SharedString>> = info
            .groups
            .iter()
            .flat_map(|g| {
                if g.consumers.is_empty() {
                    vec![vec![
                        g.name.clone(),
                        g.last_delivered_id.clone(),
                        g.lag.to_string().into(),
                        "—".into(),
                        "—".into(),
                        "—".into(),
                    ]]
                } else {
                    g.consumers
                        .iter()
                        .map(|c| {
                            vec![
                                g.name.clone(),
                                g.last_delivered_id.clone(),
                                g.lag.to_string().into(),
                                c.name.clone(),
                                c.pending.to_string().into(),
                                format_duration(Duration::from_millis(c.idle_ms as u64)).into(),
                            ]
                        })
                        .collect()
                }
            })
            .collect();
        self.groups_consumers_table =
            Some(cx.new(|cx| TableState::new(SimpleTableDelegate::new(gc_columns, gc_rows), window, cx)));

        // All pending entries, flattened across groups
        // Columns: Group | Entry ID | Consumer | Idle(ms) | Deliveries
        let pending_columns = vec![
            (i18n_stream_editor(cx, "col_group"), Some(140.)),
            (i18n_stream_editor(cx, "col_entry_id"), Some(160.)),
            (i18n_stream_editor(cx, "col_consumer"), Some(140.)),
            (i18n_stream_editor(cx, "col_idle_ms"), Some(100.)),
            (i18n_stream_editor(cx, "col_deliveries"), None),
        ];
        let pending_rows: Vec<Vec<SharedString>> = info
            .groups
            .iter()
            .flat_map(|g| {
                g.pending_entries.iter().map(|p| {
                    vec![
                        g.name.clone(),
                        p.id.clone(),
                        p.consumer.clone(),
                        format_duration(Duration::from_millis(p.idle_ms as u64)).into(),
                        p.delivery_count.to_string().into(),
                    ]
                })
            })
            .collect();
        self.pending_table =
            Some(cx.new(|cx| TableState::new(SimpleTableDelegate::new(pending_columns, pending_rows), window, cx)));
    }

    /// Renders a single labelled metric: small muted label above a bold value.
    fn render_metric(&self, label: SharedString, value: SharedString, muted: gpui::Hsla) -> impl gpui::IntoElement {
        v_flex()
            .gap_0p5()
            .child(Label::new(label).text_xs().text_color(muted))
            .child(Label::new(value).text_sm())
    }

    fn render_info_view(&self, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;

        let stream_value = self
            .server_state
            .read(cx)
            .value()
            .and_then(|v| v.stream_value())
            .cloned();

        let stream_info = stream_value.as_ref().and_then(|sv| sv.info.clone());

        let base = v_flex().size_full().overflow_y_scrollbar().p_4().gap_4();

        let Some(info) = stream_info else {
            return base
                .child(
                    Label::new(i18n_stream_editor(cx, "loading"))
                        .text_sm()
                        .text_color(muted),
                )
                .into_any_element();
        };

        // ── Stream summary card ───────────────────────────────────────────────
        let length = stream_value.as_ref().map_or(0, |sv| sv.size);
        let summary = info.summary.clone();

        let summary_card = v_flex()
            .w_full()
            .gap_2()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(border)
            .child(
                Label::new(i18n_stream_editor(cx, "stream_summary_title"))
                    .text_xs()
                    .text_color(muted),
            )
            .child(
                h_flex()
                    .gap_6()
                    .flex_wrap()
                    .child(self.render_metric(i18n_stream_editor(cx, "col_length"), length.to_string().into(), muted))
                    .when_some(summary.as_ref(), |this, s| {
                        this.child(self.render_metric(
                            i18n_stream_editor(cx, "col_groups_count"),
                            s.groups_count.to_string().into(),
                            muted,
                        ))
                    })
                    .when_some(summary.as_ref(), |this, s| {
                        this.child(self.render_metric(
                            i18n_stream_editor(cx, "col_first_entry_id"),
                            s.first_entry_id.clone(),
                            muted,
                        ))
                        .child(self.render_metric(
                            i18n_stream_editor(cx, "col_last_entry_id"),
                            s.last_entry_id.clone(),
                            muted,
                        ))
                        .child(self.render_metric(
                            i18n_stream_editor(cx, "col_radix_tree_nodes"),
                            s.radix_tree_nodes.to_string().into(),
                            muted,
                        ))
                        .child(self.render_metric(
                            i18n_stream_editor(cx, "col_radix_tree_keys"),
                            s.radix_tree_keys.to_string().into(),
                            muted,
                        ))
                    }),
            );

        let mut result = base.child(summary_card);

        if info.groups.is_empty() {
            return result
                .child(
                    Label::new(i18n_stream_editor(cx, "no_consumer_groups"))
                        .text_sm()
                        .text_color(muted),
                )
                .into_any_element();
        }

        if let Some(ref t) = self.groups_consumers_table {
            result = result
                .child(
                    Label::new(i18n_stream_editor(cx, "consumer_groups_title"))
                        .text_xs()
                        .text_color(muted),
                )
                .child(
                    div()
                        .w_full()
                        .h(px(160.))
                        .border_1()
                        .border_color(border)
                        .rounded_lg()
                        .overflow_hidden()
                        .child(DataTable::new(t).stripe(true).bordered(false)),
                );
        }

        if let Some(ref t) = self.pending_table {
            result = result
                .child(
                    Label::new(i18n_stream_editor(cx, "pending_entries_title"))
                        .text_xs()
                        .text_color(muted),
                )
                .child(
                    div()
                        .w_full()
                        .h(px(160.))
                        .border_1()
                        .border_color(border)
                        .rounded_lg()
                        .overflow_hidden()
                        .child(DataTable::new(t).stripe(true).bordered(false)),
                );
        }

        result.into_any_element()
    }
}

impl Render for ZedisStreamEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_info = self.is_info_view;

        v_flex()
            .size_full()
            .when(is_info, |this| {
                this.child(div().flex_1().min_h_0().child(self.render_info_view(cx)))
                    .child(
                        h_flex().flex_none().h(px(FOOTER_HEIGHT)).px_3().gap_2().child(
                            Button::new("info-view-btn")
                                .icon(IconName::LayoutDashboard)
                                .tooltip(i18n_kv_table(cx, "data_tooltip"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.is_info_view = false;
                                    cx.notify();
                                })),
                        ),
                    )
            })
            .when(!is_info, |this| {
                this.child(div().flex_1().min_h_0().child(self.table_state.clone()))
            })
            .into_any_element()
    }
}
