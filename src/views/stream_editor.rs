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

mod dialogs;
mod info_pending;
mod tail;

use crate::helpers::channel;
use crate::{
    components::ZedisKvFetcher,
    components::{KvTableColumn, KvTableMode},
    connection::{Capability, ServerDb, StreamTail},
    helpers::fast_contains_ignore_case,
    states::{
        ConnectionErrorKind, GlobalEvent, KeyType, NotificationAction, RedisStreamEntry, RedisValue, ServerEvent,
        ZedisGlobalStore, ZedisServerState, i18n_kv_table, i18n_status_bar, i18n_stream_editor, project_stream_entries,
    },
    views::{ZedisKvTable, kv_table::FOOTER_HEIGHT},
};
use gpui::{App, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, IconName, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
    input::{InputEvent, InputState},
    table::TableState,
    v_flex,
};
use info_pending::SimpleTableDelegate;
#[cfg(test)]
use info_pending::matches_pending_filter;
use zedis_ui::ZedisFormFieldType;

/// Width of the Entry Id column: a whole id ("1722990000000-12", a
/// millisecond stamp and a two-digit sequence at the table's 14px) beside
/// the cell's paddings and its hover copy button. It was 170 while the table
/// drew 12px text, and cut the last digits once it no longer did.
const ENTRY_ID_WIDTH: f32 = 196.;

/// The table's columns for a stream's fields: the entry id, then one column
/// per field name. The id is fixed and wide enough to read whole — it is the
/// entry's key, what XACK / XDEL / XRANGE take, and an even share of the
/// width cut it to six digits; the fields share what is left.
fn stream_columns(fields: &[SharedString], cx: &App) -> Vec<KvTableColumn> {
    let entry_id = i18n_kv_table(cx, "entry_id");
    fields
        .iter()
        .enumerate()
        .map(|(index, field)| {
            if index == 0 {
                KvTableColumn::new_auto_created(entry_id.as_ref()).width(ENTRY_ID_WIDTH)
            } else {
                KvTableColumn::new(field.as_str(), None)
                    .field_type(ZedisFormFieldType::Editor)
                    .verbatim()
            }
        })
        .collect()
}

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
    fn columns(&self, cx: &App) -> Option<Vec<KvTableColumn>> {
        Some(stream_columns(&self.fields, cx))
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
    fn filters_client_side(&self) -> bool {
        true
    }

    fn supports_batch_remove(&self) -> bool {
        true
    }

    fn remove_many(&self, rows: &[usize], cx: &mut App) {
        let Some(stream) = self.value.stream_value() else {
            return;
        };
        let ids: Vec<SharedString> = rows
            .iter()
            .map(|row| *self.visible_entry_indexes.get(*row).unwrap_or(row))
            .filter_map(|real| stream.get_entry_id(real))
            .collect();
        self.server_state.update(cx, |this, cx| {
            this.remove_stream_values(ids, cx);
        });
    }

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

// ─── Stream editor ───────────────────────────────────────────────────────────

pub struct ZedisStreamEditor {
    table_state: Entity<ZedisKvTable<ZedisStreamValues>>,
    server_state: Entity<ZedisServerState>,
    /// Whether the auxiliary info view (XINFO) is currently shown.
    is_info_view: bool,
    /// Merged groups+consumers table (Group | Last-ID | Lag | Consumer | Pending | Idle )
    groups_consumers_table: Option<Entity<TableState<SimpleTableDelegate>>>,
    /// Keyword filter for the Pending Entries (XPENDING) table.
    pending_filter_input: Entity<InputState>,
    /// Optional group filter for the Pending Entries (XPENDING) table.
    pending_group_filter: Option<SharedString>,
    /// Whether live tail (XREAD / XREADGROUP BLOCK loop) is running.
    tailing: bool,
    /// Optional consumer-group tail configuration (`XREADGROUP`).
    tail_mode: tail::StreamTailMode,
    /// The cancellable tail loop. Dropping it (stop, key/server
    /// switch, or view teardown) cancels the loop and its dedicated
    /// connection.
    tail_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisStreamEditor {
    /// Select exactly `entry_id` and scroll it into view. A keyword filter
    /// would also match `…-20` when the id is `…-2`, and would miss an
    /// entry that has not been paged in yet.
    fn jump_to_entry(&mut self, entry_id: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.is_info_view = false;
        self.table_state.update(cx, |table, cx| table.clear_filter(window, cx));
        // Clearing the filter emits `ValueUpdated`, and that subscriber
        // drops the row selection. The select has to run after it.
        if self.loaded_entry_index(entry_id.as_str(), cx).is_some() {
            self.select_entry_when_settled(entry_id, cx);
            return;
        }
        let at = self.server_state.read(cx).at();
        let Some(key) = self.server_state.read(cx).key().map(|key| key.to_string()) else {
            return;
        };
        let id = entry_id.to_string();
        cx.spawn(async move |this, cx| {
            let fetched = crate::connection::stream_entry(&at, &key, &id).await;
            let _ = this.update(cx, |this, cx| {
                let Ok(Some(entry)) = fetched else {
                    return;
                };
                this.server_state
                    .update(cx, |state, cx| state.insert_stream_entry(entry, cx));
                this.select_entry_when_settled(id.into(), cx);
            });
        })
        .detach();
    }

    /// Select `entry_id` once the table has rebuilt from the pending
    /// `ValueUpdated`. That event clears the selection, so selecting in
    /// the same turn never sticks.
    fn select_entry_when_settled(&mut self, entry_id: SharedString, cx: &mut Context<Self>) {
        let table = self.table_state.clone();
        let server = self.server_state.clone();
        cx.defer(move |cx| {
            let index = server.read(cx).value().and_then(|value| {
                value.stream_value().and_then(|stream| {
                    stream
                        .values
                        .iter()
                        .position(|(id, _)| id.as_str() == entry_id.as_str())
                })
            });
            if let Some(index) = index {
                table.update(cx, |table, cx| table.select_row(index, cx));
            }
        });
    }

    fn loaded_entry_index(&self, entry_id: &str, cx: &App) -> Option<usize> {
        self.server_state.read(cx).value().and_then(|value| {
            value
                .stream_value()
                .and_then(|stream| stream.values.iter().position(|(id, _)| id.as_str() == entry_id))
        })
    }

    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let fields = if let Some(values) = server_state.read(cx).value() {
            values.stream_fields()
        } else {
            vec![]
        };

        let table_state = cx.new(|cx| {
            ZedisKvTable::<ZedisStreamValues>::new(stream_columns(&fields, cx), server_state.clone(), window, cx)
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
                let is_tailing = editor.read(cx).tailing;
                let tail_icon = if is_tailing {
                    IconName::Close
                } else {
                    IconName::Asterisk
                };
                let tail_tooltip: SharedString = i18n_stream_editor(
                    cx,
                    if is_tailing {
                        "tail_stop_tooltip"
                    } else {
                        "tail_start_tooltip"
                    },
                );
                let weak_click = weak.clone();
                let weak_tail = weak.clone();
                let writable = editor.read(cx).server_state.read(cx).can(Capability::MutateContainer);
                let has_group_mode = editor.read(cx).tail_mode.group.is_some();
                let mut actions = vec![
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
                    Button::new("stream-tail-btn")
                        .icon(tail_icon)
                        .tooltip(tail_tooltip)
                        .on_click(move |_, _, cx| {
                            if let Some(editor) = weak_tail.upgrade() {
                                editor.update(cx, |this, cx| this.toggle_tail(cx));
                            }
                        }),
                ];
                if writable && !is_tailing {
                    let weak_mode = weak.clone();
                    actions.push(
                        Button::new("stream-tail-mode-btn")
                            .when(has_group_mode, |b| b.primary())
                            .when(!has_group_mode, |b| b.ghost())
                            .icon(IconName::Settings2)
                            .tooltip(i18n_stream_editor(cx, "tail_mode_group"))
                            .on_click(move |_, window, cx| {
                                if let Some(editor) = weak_mode.upgrade() {
                                    editor.update(cx, |this, cx| this.open_tail_mode_dialog(window, cx));
                                }
                            }),
                    );
                }
                actions
            }));
        });

        let pending_filter_input = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_stream_editor(cx, "pending_filter_placeholder"))
        });
        let filter_sub = cx.subscribe(&pending_filter_input, |_this, _input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
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
                        if let Some(selected_group) = this.pending_group_filter.as_ref()
                            && !info.groups.iter().any(|g| &g.name == selected_group)
                        {
                            this.pending_group_filter = None;
                        }
                        this.rebuild_info_tables(&info, window, cx);
                    }
                    cx.notify();
                }
                ServerEvent::KeySelected(_) => {
                    this.is_info_view = false;
                    this.groups_consumers_table = None;
                    this.pending_group_filter = None;
                    this.pending_filter_input.update(cx, |input, cx| {
                        input.set_value(SharedString::default(), window, cx);
                    });
                    // Stop tailing — the loop targets the previous key;
                    // dropping the task cancels it and its connection.
                    this.tail_task = None;
                    this.tailing = false;
                    this.tail_mode = tail::StreamTailMode::default();
                    cx.notify();
                }
                ServerEvent::ServerInfoUpdated | ServerEvent::FeaturesProbed => {
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
            pending_filter_input,
            pending_group_filter: None,
            tailing: false,
            tail_mode: tail::StreamTailMode::default(),
            tail_task: None,
            _subscriptions: vec![sub, filter_sub],
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_pending_filter_checks_group_and_keyword() {
        assert!(matches_pending_filter("orders", "1700-0", "worker-1", None, ""));
        assert!(matches_pending_filter(
            "orders",
            "1700-0",
            "worker-1",
            Some("orders"),
            ""
        ));
        assert!(!matches_pending_filter(
            "orders",
            "1700-0",
            "worker-1",
            Some("billing"),
            ""
        ));
        assert!(matches_pending_filter("orders", "1700-0", "worker-1", None, "WORKER"));
        assert!(matches_pending_filter("orders", "1700-0", "worker-1", None, "1700"));
        assert!(!matches_pending_filter(
            "orders", "1700-0", "worker-1", None, "worker-2"
        ));
    }
}
