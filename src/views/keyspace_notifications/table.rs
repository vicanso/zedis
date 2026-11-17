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

//! Table rows, filters, and `ZedisTextTable` builder for [`ZedisKeyspaceNotifications`].

use super::*;
#[derive(Clone, Debug)]
pub(super) struct NotificationRow {
    pub(super) timestamp: SharedString,
    pub(super) db: u32,
    pub(super) key: SharedString,
    pub(super) event: SharedString,
    pub(super) source: NotificationSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NotificationSource {
    Keyspace,
    Keyevent,
}

impl NotificationSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Keyspace => "keyspace",
            Self::Keyevent => "keyevent",
        }
    }
}

/// Which channel families to keep after parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(super) enum SourceFilter {
    #[default]
    Both,
    KeyeventOnly,
    KeyspaceOnly,
}

/// Built-in chips (order ≈ frequency). Dynamic events from the live
/// stream are appended after these.
pub(super) const DEFAULT_EVENT_CHIPS: &[&str] = &[
    "set",
    "del",
    "expire",
    "expired",
    "evicted",
    "rename_from",
    "rename_to",
    "hset",
    "hdel",
    "sadd",
    "srem",
    "lpush",
    "rpush",
    "zadd",
    "zrem",
    "xadd",
];

const COL_TIME: &str = "col_time";
const COL_DB: &str = "col_db";
const COL_KEY: &str = "col_key";
const COL_EVENT: &str = "col_event";
const COL_SOURCE: &str = "col_source";

const KEYSPACE_COLUMNS: [&str; 5] = [COL_TIME, COL_DB, COL_KEY, COL_EVENT, COL_SOURCE];
const TIME_COLUMN: usize = 0;
pub(super) const DB_COLUMN: usize = 1;
pub(super) const KEY_COLUMN: usize = 2;
pub(super) const EVENT_COLUMN: usize = 3;
const SOURCE_COLUMN: usize = 4;

impl NotificationRow {
    pub(super) fn cells(&self) -> Vec<SharedString> {
        vec![
            self.timestamp.clone(),
            self.db.to_string().into(),
            self.key.clone(),
            self.event.clone(),
            self.source.as_str().into(),
        ]
    }
}

/// The event grid: newest first, capped at [`RING_BUFFER_CAPACITY`]. The
/// time and source columns are muted, the event is coloured by what it
/// did to the key, and the key cell opens the key in the editor.
pub(super) fn build_table(
    server_state: Entity<ZedisServerState>,
    window: &mut Window,
    cx: &gpui::App,
) -> ZedisTextTable {
    let content_width = content_area_width(window, cx);

    let time_width = 160.;
    let db_width = 60.;
    let event_width = 120.;
    let source_width = 90.;
    // The key has what the others leave, and no less than this.
    let key_width = 200.;
    let widths = [time_width, db_width, key_width, event_width, source_width];

    let columns = KEYSPACE_COLUMNS
        .iter()
        .zip(widths)
        .enumerate()
        .map(|(ix, (&key, width))| {
            let column = TextColumn::new(key, i18n_keyspace_notifications(cx, key), width);
            if ix == KEY_COLUMN { column.fills() } else { column }
        })
        .collect();

    let style: CellStyleProvider = Rc::new(|col_ix, cells, cx| {
        let theme = cx.theme();
        let color = match col_ix {
            TIME_COLUMN | SOURCE_COLUMN => theme.muted_foreground,
            EVENT_COLUMN => match cells.get(EVENT_COLUMN).map(|e| e.as_ref()) {
                Some("del" | "expired" | "evicted") => theme.red,
                Some("set" | "hset" | "sadd" | "zadd" | "lpush" | "rpush" | "xadd") => theme.green,
                Some("expire") => theme.yellow,
                _ => theme.foreground,
            },
            _ => theme.foreground,
        };
        CellStyle {
            color: Some(color),
            ..Default::default()
        }
    });

    // The key cell is itself the link: click anywhere on it to open the key,
    // or click the clock button to open its single-key timeline.
    let open_key: CellRenderer = Rc::new(move |row_ix, col_ix, cells, _window, cx| {
        if col_ix != KEY_COLUMN {
            return None;
        }
        let key = cells.get(KEY_COLUMN)?.clone();
        let key_for_timeline = key.clone();
        let server_state = server_state.clone();
        let server_state_for_timeline = server_state.clone();
        let tooltip = i18n_common(cx, "open_key_tooltip");
        let timeline_tooltip = i18n_keyspace_notifications(cx, "open_timeline");
        let foreground = cx.theme().foreground;
        Some(
            h_flex()
                .size_full()
                .items_center()
                .justify_between()
                .gap_1()
                .paddings(Edges {
                    top: px(2.),
                    bottom: px(2.),
                    left: px(10.),
                    right: px(6.),
                })
                .child(
                    div()
                        .id(("ks-open-key", row_ix))
                        .flex_1()
                        .min_w_0()
                        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                        .on_click(move |_, _, cx: &mut App| {
                            open_key_in_editor(&server_state, key.clone(), cx);
                        })
                        .child(
                            Label::new(cells[KEY_COLUMN].clone())
                                .text_color(foreground)
                                .text_sm()
                                .text_ellipsis(),
                        ),
                )
                .child(
                    Button::new(("ks-open-timeline", row_ix))
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(CustomIconName::Clock3))
                        .tooltip(timeline_tooltip)
                        .on_click(move |_, window, cx| {
                            open_key_timeline_dialog_for_key(
                                server_state_for_timeline.clone(),
                                key_for_timeline.clone(),
                                window,
                                cx,
                            );
                        }),
                )
                .into_any_element(),
        )
    });

    ZedisTextTable::new(columns, i18n_common(cx, "copied_to_clipboard"))
        .copy_tooltip(i18n_common(cx, "copy_cell_tooltip"))
        .max_rows(RING_BUFFER_CAPACITY)
        .cell_style(style)
        .cell_render(open_key)
        .fitted_to(content_width)
}

fn escape_keyspace_glob(prefix: &str) -> String {
    let mut escaped = String::with_capacity(prefix.len());
    for ch in prefix.chars() {
        if matches!(ch, '\\' | '*' | '?' | '[' | ']') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

pub(super) fn subscription_patterns(db: usize, prefix_filter: Option<&str>) -> Vec<String> {
    if let Some(prefix) = prefix_filter.filter(|p| !p.is_empty()) {
        vec![format!("__keyspace@{db}__:{}*", escape_keyspace_glob(prefix))]
    } else {
        vec![KEYSPACE_PATTERN.to_string(), KEYEVENT_PATTERN.to_string()]
    }
}

/// The structured filter as a row predicate over the cells — `None` when
/// nothing is narrowed, so the table shows every row.
pub(super) fn row_predicate(
    selected_events: &Option<AHashSet<String>>,
    key_filter: &str,
    prefix_filter: Option<&str>,
    selected_dbs: &Option<AHashSet<u32>>,
    source_filter: SourceFilter,
) -> Option<RowPredicate> {
    let prefix_filter = prefix_filter.filter(|p| !p.is_empty());
    if selected_events.is_none()
        && key_filter.is_empty()
        && prefix_filter.is_none()
        && selected_dbs.is_none()
        && source_filter == SourceFilter::Both
    {
        return None;
    }
    let events = selected_events.clone();
    let key_filter = key_filter.to_string();
    let prefix_filter = prefix_filter.map(str::to_string);
    let dbs = selected_dbs.clone();
    Some(Rc::new(move |cells: &[SharedString]| {
        let cell = |ix: usize| cells.get(ix).map(|c| c.as_ref()).unwrap_or("");
        if let Some(set) = &events
            && !set.contains(cell(EVENT_COLUMN))
        {
            return false;
        }
        if let Some(prefix) = &prefix_filter {
            if !cell(KEY_COLUMN).starts_with(prefix.as_str()) {
                return false;
            }
        } else if !key_filter.is_empty() && !cell(KEY_COLUMN).contains(key_filter.as_str()) {
            return false;
        }
        if let Some(set) = &dbs
            && !cell(DB_COLUMN).parse::<u32>().is_ok_and(|db| set.contains(&db))
        {
            return false;
        }
        match source_filter {
            SourceFilter::Both => true,
            SourceFilter::KeyeventOnly => cell(SOURCE_COLUMN) == NotificationSource::Keyevent.as_str(),
            SourceFilter::KeyspaceOnly => cell(SOURCE_COLUMN) == NotificationSource::Keyspace.as_str(),
        }
    }))
}
