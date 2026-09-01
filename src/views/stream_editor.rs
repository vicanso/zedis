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
    assets::CustomIconName,
    components::ZedisKvFetcher,
    components::{KvTableColumn, KvTableMode},
    connection::{get_server, open_single_connection},
    helpers::{fast_contains_ignore_case, format_duration},
    states::{
        ConnectionErrorKind, GlobalEvent, KeyType, NotificationAction, RedisStreamEntry, RedisValue, ServerEvent,
        StreamInfoData, StreamRefPolicy, StreamTrim, ZedisGlobalStore, ZedisServerState, dialog_button_props,
        escalate_dangerous_body, i18n_common, i18n_kv_table, i18n_status_bar, i18n_stream_editor, tail_read,
    },
    views::{ZedisKvTable, kv_table::FOOTER_HEIGHT},
};
use gpui::{App, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    label::Label,
    scroll::ScrollableElement,
    table::{Column, DataTable, TableDelegate, TableState},
    v_flex,
};
use rust_i18n::t;
use std::sync::Arc;
use std::time::Duration;
use zedis_ui::{ZedisDialog, ZedisFormFieldType};

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
        let entry_id = i18n_kv_table(cx, "entry_id");
        Some(
            self.fields
                .iter()
                .enumerate()
                .map(|(index, field)| {
                    if index == 0 {
                        KvTableColumn::new_auto_created(entry_id.as_ref())
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
            .child(Label::new(name).text_sm().text_color(cx.theme().muted_foreground))
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
    /// Whether live tail (XREAD BLOCK loop) is running.
    tailing: bool,
    /// The cancellable tail loop. Dropping it (stop, key/server
    /// switch, or view teardown) cancels the loop and its dedicated
    /// connection.
    tail_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// Block timeout per `XREAD` round (ms). Short enough that a stop /
/// key-switch is reflected promptly when the loop next wakes.
const TAIL_BLOCK_MS: u64 = 2000;
/// Max entries per `XREAD` round.
const TAIL_COUNT: usize = 200;
/// Ring-buffer cap — a hot stream trims oldest entries beyond this so
/// a long-running tail can't grow memory unbounded.
const TAIL_CAP: usize = 5000;

impl ZedisStreamEditor {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let fields = if let Some(values) = server_state.read(cx).value() {
            values.stream_fields()
        } else {
            vec![]
        };

        let entry_id = i18n_kv_table(cx, "entry_id");
        let table_state = cx.new(|cx| {
            ZedisKvTable::<ZedisStreamValues>::new(
                fields
                    .iter()
                    .enumerate()
                    .map(|(index, field)| {
                        if index == 0 {
                            KvTableColumn::new_auto_created(entry_id.as_ref())
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
                    Button::new("stream-tail-btn")
                        .icon(tail_icon)
                        .tooltip(tail_tooltip)
                        .on_click(move |_, _, cx| {
                            if let Some(editor) = weak_tail.upgrade() {
                                editor.update(cx, |this, cx| this.toggle_tail(cx));
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
                    // Stop tailing — the loop targets the previous key;
                    // dropping the task cancels it and its connection.
                    this.tail_task = None;
                    this.tailing = false;
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
            tailing: false,
            tail_task: None,
            _subscriptions: vec![sub],
        }
    }

    /// Start/stop the live-tail loop. Stopping just drops the task
    /// (which cancels the loop + its dedicated connection). Starting
    /// spawns: a background `XREAD BLOCK` loop feeding a channel, and
    /// a foreground drainer that ring-appends batches into the stream
    /// value. Tails from `$` so only entries arriving after Start are
    /// shown; existing loaded entries are kept.
    fn toggle_tail(&mut self, cx: &mut Context<Self>) {
        if self.tailing {
            self.tail_task = None;
            self.tailing = false;
            cx.notify();
            return;
        }

        let server_state = self.server_state.clone();
        let server_id = server_state.read(cx).server_id().to_string();
        let db = server_state.read(cx).db();
        let Some(key) = server_state.read(cx).key() else {
            return;
        };
        let key = key.to_string();
        let entity = cx.entity().downgrade();
        let (tx, rx) = smol::channel::unbounded::<Vec<RedisStreamEntry>>();

        let task = cx.spawn(async move |_handle, cx| {
            let Ok(server) = get_server(&server_id) else {
                let _ = entity.update(cx, |this: &mut ZedisStreamEditor, cx| {
                    this.tailing = false;
                    this.tail_task = None;
                    cx.notify();
                });
                return;
            };

            // Open the dedicated tail connection on the foreground task (we
            // still have `cx` here) so a failure can be surfaced. The old code
            // opened it inside `background_spawn`, where a failure just ended
            // the loop silently — the tail button sprang back with no hint why.
            let mut conn = match open_single_connection(&server, db, false).await {
                Ok(c) => c,
                Err(e) => {
                    let kind = e.connection_kind();
                    let _ = entity.update(cx, |this: &mut ZedisStreamEditor, cx| {
                        this.tailing = false;
                        this.tail_task = None;
                        this.notify_tail_failed(kind, cx);
                        cx.notify();
                    });
                    return;
                }
            };
            let key_bg = key.clone();
            let bg = cx.background_spawn(async move {
                // `$` = only entries that arrive after we subscribe.
                // A read error ends the while-let (and the loop).
                let mut last_id = "$".to_string();
                while let Ok((new_last, entries)) =
                    tail_read(&mut conn, &key_bg, &last_id, TAIL_BLOCK_MS, TAIL_COUNT).await
                {
                    if !entries.is_empty() {
                        last_id = new_last;
                        if tx.send(entries).await.is_err() {
                            break;
                        }
                    }
                }
            });

            while let Ok(batch) = rx.recv().await {
                let key_cl = key.clone();
                let r = entity.update(cx, |this: &mut ZedisStreamEditor, cx| {
                    this.server_state.update(cx, |state, cx| {
                        state.append_tail_entries(&key_cl, batch, TAIL_CAP, cx);
                    });
                });
                if r.is_err() {
                    break;
                }
            }

            // Channel closed or server entity gone → tear down.
            drop(bg);
            let _ = entity.update(cx, |this: &mut ZedisStreamEditor, cx| {
                this.tailing = false;
                this.tail_task = None;
                cx.notify();
            });
        });

        self.tail_task = Some(task);
        self.tailing = true;
        cx.notify();
    }

    /// Toast that the live tail couldn't start, naming the reason (connection
    /// timed out / auth failed / …) so the user isn't left wondering why the
    /// tail button sprang back.
    fn notify_tail_failed(&self, kind: ConnectionErrorKind, cx: &mut Context<Self>) {
        let reason = i18n_status_bar(cx, kind.reason_key());
        let msg: SharedString = format!("{}: {}", i18n_stream_editor(cx, "tail_stopped"), reason).into();
        cx.global::<ZedisGlobalStore>().clone().update(cx, |_, cx| {
            cx.emit(GlobalEvent::Notification(NotificationAction::new_error(msg)));
        });
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
    }

    /// Renders a single labelled metric: small muted label above a bold value.
    /// XGROUP CREATE dialog: group name + start ID (defaults to `$`).
    fn create_group_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name_state =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n_stream_editor(cx, "group_name_placeholder")));
        let id_state = cx.new(|cx| InputState::new(window, cx).default_value("$"));
        let name_label = i18n_stream_editor(cx, "group_name");
        let id_label = i18n_stream_editor(cx, "start_id");
        let id_hint = i18n_stream_editor(cx, "start_id_hint");
        let body_name = name_state.clone();
        let body_id = id_state.clone();
        let submit_name = name_state.clone();
        let submit_id = id_state.clone();
        let server_state = self.server_state.clone();

        ZedisDialog::new(i18n_stream_editor(cx, "create_group_title"))
            .w(px(480.))
            .ok_text(i18n_common(cx, "confirm"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_common(cx, "confirm"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || {
                gpui_component::v_flex()
                    .gap_2()
                    .w_full()
                    .child(Label::new(name_label.clone()).text_xs())
                    .child(Input::new(&body_name))
                    .child(Label::new(id_label.clone()).text_xs())
                    .child(Input::new(&body_id))
                    .child(Label::new(id_hint.clone()).text_xs())
            })
            .on_ok(move |_, _window, cx| {
                let name = submit_name.read(cx).value().to_string();
                if name.trim().is_empty() {
                    // Keep the dialog open until a group name is given.
                    return false;
                }
                let raw_id = submit_id.read(cx).value().to_string();
                let id = if raw_id.trim().is_empty() {
                    "$".to_string()
                } else {
                    raw_id
                };
                server_state.update(cx, |state, cx| {
                    state.create_stream_group(name.trim().to_string().into(), id.into(), cx);
                });
                true
            })
            .open(window, cx);
    }

    /// XGROUP SETID dialog for a specific group.
    fn setid_group_dialog(&mut self, group: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let id_state = cx.new(|cx| InputState::new(window, cx).default_value("$"));
        let id_hint = i18n_stream_editor(cx, "setid_hint");
        let body_id = id_state.clone();
        let submit_id = id_state.clone();
        let server_state = self.server_state.clone();

        ZedisDialog::new(i18n_stream_editor(cx, "setid_title"))
            .w(px(480.))
            .ok_text(i18n_common(cx, "confirm"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_common(cx, "confirm"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || {
                gpui_component::v_flex()
                    .gap_2()
                    .w_full()
                    .child(Input::new(&body_id))
                    .child(Label::new(id_hint.clone()).text_xs())
            })
            .on_ok(move |_, _window, cx| {
                let raw_id = submit_id.read(cx).value().to_string();
                if raw_id.trim().is_empty() {
                    return false;
                }
                let group = group.clone();
                server_state.update(cx, |state, cx| {
                    state.set_stream_group_id(group, raw_id.trim().to_string().into(), cx);
                });
                true
            })
            .open(window, cx);
    }

    /// XGROUP DESTROY confirmation. Destructive — drops the group and
    /// its entire PEL, so it routes through the standard alert dialog.
    fn confirm_destroy_group(&mut self, group: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let message = t!(
            "stream_editor.destroy_group_prompt",
            group = group.as_ref(),
            locale = locale
        )
        .to_string();
        let server_state = self.server_state.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();
        let group_for_ok = group.clone();

        ZedisDialog::new_alert(
            i18n_stream_editor(cx, "destroy_group_title"),
            escalate_dangerous_body(cx, &server_id, message),
        )
        .button_props(dialog_button_props(cx))
        .on_ok(move |_, window, cx| {
            let group = group_for_ok.clone();
            server_state.update(cx, |state, cx| {
                state.destroy_stream_group(group, cx);
            });
            window.close_dialog(cx);
            true
        })
        .open(window, cx);
    }

    /// XCLAIM dialog for one pending entry: target consumer name.
    fn claim_entry_dialog(
        &mut self,
        group: SharedString,
        entry_id: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let consumer_state =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n_stream_editor(cx, "claim_consumer_placeholder")));
        let hint = i18n_stream_editor(cx, "claim_hint");
        let body_consumer = consumer_state.clone();
        let submit_consumer = consumer_state.clone();
        let server_state = self.server_state.clone();

        ZedisDialog::new(i18n_stream_editor(cx, "claim_title"))
            .w(px(480.))
            .ok_text(i18n_common(cx, "confirm"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_common(cx, "confirm"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || {
                gpui_component::v_flex()
                    .gap_2()
                    .w_full()
                    .child(Input::new(&body_consumer))
                    .child(Label::new(hint.clone()).text_xs())
            })
            .on_ok(move |_, _window, cx| {
                let consumer = submit_consumer.read(cx).value().to_string();
                if consumer.trim().is_empty() {
                    return false;
                }
                let group = group.clone();
                let entry_id = entry_id.clone();
                server_state.update(cx, |state, cx| {
                    state.claim_stream_entry(group, consumer.trim().to_string().into(), entry_id, cx);
                });
                true
            })
            .open(window, cx);
    }

    /// XAUTOCLAIM dialog for one group: target consumer, minimum idle
    /// time and how many entries to claim at most.
    fn autoclaim_dialog(&mut self, group: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let consumer_state =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n_stream_editor(cx, "claim_consumer_placeholder")));
        let min_idle_state = cx.new(|cx| InputState::new(window, cx).default_value("60000"));
        let count_state = cx.new(|cx| InputState::new(window, cx).default_value("100"));
        let min_idle_label = i18n_stream_editor(cx, "autoclaim_min_idle");
        let count_label = i18n_stream_editor(cx, "autoclaim_count");
        let hint = i18n_stream_editor(cx, "autoclaim_hint");
        let body_consumer = consumer_state.clone();
        let body_min_idle = min_idle_state.clone();
        let body_count = count_state.clone();
        let submit_consumer = consumer_state.clone();
        let submit_min_idle = min_idle_state.clone();
        let submit_count = count_state.clone();
        let server_state = self.server_state.clone();

        ZedisDialog::new(i18n_stream_editor(cx, "autoclaim_title"))
            .w(px(480.))
            .ok_text(i18n_common(cx, "confirm"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_common(cx, "confirm"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || {
                gpui_component::v_flex()
                    .gap_2()
                    .w_full()
                    .child(Input::new(&body_consumer))
                    .child(Label::new(min_idle_label.clone()).text_xs())
                    .child(Input::new(&body_min_idle))
                    .child(Label::new(count_label.clone()).text_xs())
                    .child(Input::new(&body_count))
                    .child(Label::new(hint.clone()).text_xs())
            })
            .on_ok(move |_, _window, cx| {
                let consumer = submit_consumer.read(cx).value().to_string();
                if consumer.trim().is_empty() {
                    return false;
                }
                let Ok(min_idle_ms) = submit_min_idle.read(cx).value().trim().parse::<u64>() else {
                    return false;
                };
                let Ok(count) = submit_count.read(cx).value().trim().parse::<usize>() else {
                    return false;
                };
                if count == 0 {
                    return false;
                }
                let group = group.clone();
                server_state.update(cx, |state, cx| {
                    state.autoclaim_stream_entries(
                        group,
                        consumer.trim().to_string().into(),
                        min_idle_ms,
                        count.min(1000),
                        cx,
                    );
                });
                true
            })
            .open(window, cx);
    }

    /// XTRIM dialog: one input — a plain number trims by MAXLEN (keep
    /// the newest n), an `ms-seq` id trims by MINID (drop older) — plus,
    /// on Redis 8.2+, the reference-policy toggle (KEEPREF / DELREF /
    /// ACKED) — then the standard destructive confirm with the exact
    /// command spelled out (PROD-escalated).
    fn trim_stream_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let show_policy = self.server_state.read(cx).supports_stream_ref_policies();
        let form = cx.new(|cx| StreamTrimForm::new(show_policy, window, cx));
        let body = form.clone();
        let server_state = self.server_state.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();

        ZedisDialog::new(i18n_stream_editor(cx, "trim_title"))
            .w(px(480.))
            .ok_text(i18n_common(cx, "confirm"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_common(cx, "confirm"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || body.clone())
            .on_ok(move |_, window, cx| {
                let (raw, policy) = {
                    let form = form.read(cx);
                    (
                        form.threshold.read(cx).value().trim().to_string(),
                        form.policy_to_send(),
                    )
                };
                let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
                let (trim, mut message) = if raw.contains('-') {
                    let message = t!("stream_editor.trim_confirm_minid", id = raw.as_str(), locale = locale);
                    (StreamTrim::MinId(raw.clone().into()), message.to_string())
                } else if let Ok(n) = raw.parse::<u64>() {
                    let message = t!("stream_editor.trim_confirm_maxlen", n = n, locale = locale);
                    (StreamTrim::MaxLen(n), message.to_string())
                } else {
                    // Neither a count nor an id — keep the dialog open.
                    return false;
                };
                // A non-default reference policy changes what the trim does
                // to consumer groups — spell it in the confirm (the option
                // word is the command syntax, deliberately untranslated).
                if let Some(policy) = policy.filter(|p| *p != StreamRefPolicy::KeepRef) {
                    message = format!("{message} · {}", policy.word());
                }
                // Close the form ourselves before stacking the confirm on
                // top — returning true would auto-close the *topmost*
                // dialog, i.e. the alert we are about to open.
                window.close_dialog(cx);
                let server_state = server_state.clone();
                ZedisDialog::new_alert(
                    i18n_stream_editor(cx, "trim_title"),
                    escalate_dangerous_body(cx, &server_id, message),
                )
                .button_props(dialog_button_props(cx))
                .on_ok(move |_, window, cx| {
                    let trim = trim.clone();
                    server_state.update(cx, |state, cx| state.trim_stream(trim, policy, cx));
                    window.close_dialog(cx);
                    true
                })
                .open(window, cx);
                false
            })
            .open(window, cx);
    }

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

        // ── Idempotent producer (IDMP, Redis 8.6+) ───────────────────────────
        // Presence-gated on the XINFO reply — shown only when the server
        // reports the counters, so no version check is needed.
        if let Some(idmp) = summary.as_ref().and_then(|s| s.idmp.clone()) {
            result = result.child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(border)
                    .child(
                        Label::new(i18n_stream_editor(cx, "idmp_title"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(
                        h_flex()
                            .gap_6()
                            .flex_wrap()
                            .child(self.render_metric(
                                i18n_stream_editor(cx, "idmp_pids"),
                                idmp.pids_tracked.to_string().into(),
                                muted,
                            ))
                            .child(self.render_metric(
                                i18n_stream_editor(cx, "idmp_iids"),
                                idmp.iids_tracked.to_string().into(),
                                muted,
                            ))
                            .child(self.render_metric(
                                i18n_stream_editor(cx, "idmp_added"),
                                idmp.iids_added.to_string().into(),
                                muted,
                            ))
                            .child(self.render_metric(
                                i18n_stream_editor(cx, "idmp_duplicates"),
                                idmp.iids_duplicates.to_string().into(),
                                muted,
                            )),
                    ),
            );
        }

        // ── Manage groups: header + create, then per-group actions ───────────
        result = result.child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .child(
                    Label::new(i18n_stream_editor(cx, "manage_groups_title"))
                        .text_xs()
                        .text_color(muted),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("stream-trim")
                                .small()
                                .icon(Icon::new(CustomIconName::ListX))
                                .label(i18n_stream_editor(cx, "trim"))
                                .tooltip(i18n_stream_editor(cx, "trim_tooltip"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.trim_stream_dialog(window, cx);
                                })),
                        )
                        .child(
                            Button::new("stream-create-group")
                                .small()
                                .icon(IconName::Plus)
                                .label(i18n_stream_editor(cx, "create_group"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.create_group_dialog(window, cx);
                                })),
                        ),
                ),
        );

        for (idx, g) in info.groups.iter().enumerate() {
            let setid_group = g.name.clone();
            let autoclaim_group = g.name.clone();
            let destroy_group = g.name.clone();
            result = result.child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .border_1()
                    .border_color(border)
                    .rounded(cx.theme().radius)
                    .child(Label::new(g.name.clone()).text_sm())
                    .child(Label::new(g.last_delivered_id.clone()).text_xs().text_color(muted))
                    .child(div().flex_1())
                    .child(
                        Button::new(("stream-setid", idx))
                            .small()
                            .ghost()
                            .icon(IconName::Asterisk)
                            .tooltip(i18n_stream_editor(cx, "setid_tooltip"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.setid_group_dialog(setid_group.clone(), window, cx);
                            })),
                    )
                    .child(
                        Button::new(("stream-autoclaim", idx))
                            .small()
                            .ghost()
                            .icon(Icon::new(CustomIconName::ListCheck))
                            .tooltip(i18n_stream_editor(cx, "autoclaim_tooltip"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.autoclaim_dialog(autoclaim_group.clone(), window, cx);
                            })),
                    )
                    .child(
                        Button::new(("stream-destroy", idx))
                            .small()
                            .danger()
                            .icon(IconName::Close)
                            .tooltip(i18n_stream_editor(cx, "destroy_tooltip"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.confirm_destroy_group(destroy_group.clone(), window, cx);
                            })),
                    ),
            );
        }

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

        result = result
            .child(
                Label::new(i18n_stream_editor(cx, "pending_entries_title"))
                    .text_xs()
                    .text_color(muted),
            )
            .child(self.render_pending_entries(&info, muted, border, cx));

        result.into_any_element()
    }

    /// Pending entries across every group, as interactive rows: per-row
    /// XACK / XCLAIM, plus a per-group "load more" while XPENDING pages
    /// remain. Hand-rolled instead of a `DataTable` because the table
    /// component has no per-row action slot (same reason as the
    /// workspace tab strip).
    fn render_pending_entries(
        &self,
        info: &Arc<StreamInfoData>,
        muted: gpui::Hsla,
        border: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> impl gpui::IntoElement + use<> {
        let total_pending: usize = info.groups.iter().map(|g| g.pending_entries.len()).sum();

        // Header row mirroring the old table columns; the trailing gap
        // roughly reserves the per-row action-button width.
        let header = h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(border)
            .child(
                Label::new(i18n_stream_editor(cx, "col_group"))
                    .text_xs()
                    .text_color(muted)
                    .w(px(120.)),
            )
            .child(
                Label::new(i18n_stream_editor(cx, "col_entry_id"))
                    .text_xs()
                    .text_color(muted)
                    .w(px(150.)),
            )
            .child(
                Label::new(i18n_stream_editor(cx, "col_consumer"))
                    .text_xs()
                    .text_color(muted)
                    .w(px(120.)),
            )
            .child(
                Label::new(i18n_stream_editor(cx, "col_idle_ms"))
                    .text_xs()
                    .text_color(muted)
                    .w(px(80.)),
            )
            .child(
                Label::new(i18n_stream_editor(cx, "col_deliveries"))
                    .text_xs()
                    .text_color(muted),
            );

        let mut list = v_flex().w_full();
        if total_pending == 0 {
            list = list.child(
                div().p_2().child(
                    Label::new(i18n_stream_editor(cx, "no_pending_entries"))
                        .text_sm()
                        .text_color(muted),
                ),
            );
        }
        let can_ackdel = self.server_state.read(cx).supports_stream_ref_policies();
        let mut row_ix = 0usize;
        for g in info.groups.iter() {
            for p in g.pending_entries.iter() {
                let ack_group = g.name.clone();
                let ack_id = p.id.clone();
                let ackdel_group = g.name.clone();
                let ackdel_id = p.id.clone();
                let claim_group = g.name.clone();
                let claim_id = p.id.clone();
                list = list.child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_0p5()
                        .border_b_1()
                        .border_color(border)
                        .child(
                            Label::new(g.name.clone())
                                .text_xs()
                                .text_color(muted)
                                .w(px(120.))
                                .text_ellipsis(),
                        )
                        .child(Label::new(p.id.clone()).text_xs().w(px(150.)).text_ellipsis())
                        .child(Label::new(p.consumer.clone()).text_xs().w(px(120.)).text_ellipsis())
                        .child(
                            Label::new(SharedString::from(format_duration(Duration::from_millis(
                                p.idle_ms as u64,
                            ))))
                            .text_xs()
                            .text_color(muted)
                            .w(px(80.)),
                        )
                        .child(Label::new(p.delivery_count.to_string()).text_xs().text_color(muted))
                        .child(div().flex_1())
                        .child(
                            Button::new(("stream-ack", row_ix))
                                .xsmall()
                                .ghost()
                                .icon(IconName::Check)
                                .tooltip(i18n_stream_editor(cx, "ack_tooltip"))
                                .on_click(cx.listener(move |this, _, _window, cx| {
                                    this.server_state.update(cx, |state, cx| {
                                        state.ack_stream_entry(ack_group.clone(), ack_id.clone(), cx);
                                    });
                                })),
                        )
                        // Ack + delete in one atomic XACKDEL (Redis 8.2+ —
                        // hidden elsewhere, incl. Valkey).
                        .when(can_ackdel, |this| {
                            let ackdel_group = ackdel_group.clone();
                            let ackdel_id = ackdel_id.clone();
                            this.child(
                                Button::new(("stream-ackdel", row_ix))
                                    .xsmall()
                                    .ghost()
                                    .icon(Icon::new(CustomIconName::ListX))
                                    .tooltip(i18n_stream_editor(cx, "ackdel_tooltip"))
                                    .on_click(cx.listener(move |this, _, _window, cx| {
                                        this.server_state.update(cx, |state, cx| {
                                            state.ackdel_stream_entry(ackdel_group.clone(), ackdel_id.clone(), cx);
                                        });
                                    })),
                            )
                        })
                        .child(
                            Button::new(("stream-claim", row_ix))
                                .xsmall()
                                .ghost()
                                .icon(IconName::CircleUser)
                                .tooltip(i18n_stream_editor(cx, "claim_tooltip"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.claim_entry_dialog(claim_group.clone(), claim_id.clone(), window, cx);
                                })),
                        ),
                );
                row_ix += 1;
            }
            if !g.pending_done {
                let more_group = g.name.clone();
                let label: SharedString =
                    format!("{} · {}", i18n_stream_editor(cx, "pending_load_more"), g.name).into();
                list = list.child(
                    div().p_1().child(
                        Button::new(("stream-pending-more", row_ix))
                            .xsmall()
                            .ghost()
                            .icon(Icon::new(CustomIconName::ChevronsDown))
                            .label(label)
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.server_state.update(cx, |state, cx| {
                                    state.load_more_stream_pending(more_group.clone(), cx);
                                });
                            })),
                    ),
                );
                row_ix += 1;
            }
        }

        // Fixed height on purpose: `max_h` + a scroll wrapper silently
        // clips instead of scrolling (see the CLAUDE.md gotcha).
        v_flex()
            .w_full()
            .h(px(220.))
            .border_1()
            .border_color(border)
            .rounded_lg()
            .child(header)
            .child(div().flex_1().min_h_0().overflow_y_scrollbar().child(list))
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

/// The XTRIM dialog body as a **view entity** (a dialog body holding an
/// `InputState` must be one — see the CLAUDE.md dialog gotcha, and the
/// policy toggle needs `cx.notify` to repaint anyway): the threshold input
/// plus, on Redis 8.2+, the reference-policy toggle row.
struct StreamTrimForm {
    threshold: Entity<InputState>,
    /// Offer the KEEPREF / DELREF / ACKED toggle (Redis 8.2+ only).
    show_policy: bool,
    policy: StreamRefPolicy,
}

impl StreamTrimForm {
    fn new(show_policy: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let threshold =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n_stream_editor(cx, "trim_placeholder")));
        Self {
            threshold,
            show_policy,
            policy: StreamRefPolicy::default(),
        }
    }

    /// What the dialog submits: `None` when the server predates the
    /// option words (nothing extra is sent on the wire).
    fn policy_to_send(&self) -> Option<StreamRefPolicy> {
        self.show_policy.then_some(self.policy)
    }
}

impl Render for StreamTrimForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        // Option words are command syntax — shown verbatim, tooltips carry
        // the localized meaning.
        let policies: [(StreamRefPolicy, &'static str); 3] = [
            (StreamRefPolicy::KeepRef, "trim_policy_keepref_tooltip"),
            (StreamRefPolicy::DelRef, "trim_policy_delref_tooltip"),
            (StreamRefPolicy::Acked, "trim_policy_acked_tooltip"),
        ];
        v_flex()
            .gap_2()
            .w_full()
            .child(Input::new(&self.threshold))
            .child(Label::new(i18n_stream_editor(cx, "trim_hint")).text_xs())
            .when(self.show_policy, |this| {
                let mut row = h_flex().gap_1().items_center().child(
                    Label::new(i18n_stream_editor(cx, "trim_policy_label"))
                        .text_xs()
                        .text_color(muted),
                );
                for (policy, tooltip_key) in policies {
                    let active = self.policy == policy;
                    row = row.child(
                        Button::new(SharedString::from(format!("stream-trim-{}", policy.word())))
                            .xsmall()
                            .when(active, |b| b.primary())
                            .when(!active, |b| b.outline())
                            .label(policy.word())
                            .tooltip(i18n_stream_editor(cx, tooltip_key))
                            .on_click(cx.listener(move |this, _, _w, cx| {
                                this.policy = policy;
                                cx.notify();
                            })),
                    );
                }
                this.child(row)
            })
    }
}
