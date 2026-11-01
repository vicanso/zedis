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

//! Pane split, width probes, and responsive column width fitting for [`ZedisKvTable`].

use super::*;

/// The least the entry panel is useful at: its fields, and Remove / Cancel /
/// Update on one line.
pub(super) const ENTRY_PANEL_MIN_WIDTH: f32 = 320.;
/// What the table keeps beside an open entry panel, and the least of it worth
/// drawing there: the tick box, the row number, and the naming column and the
/// value at about [`DATA_COLUMN_MIN_WIDTH`] each (`fit_columns`). In a pane
/// that cannot give it this, the panel is shown alone.
pub(super) const TABLE_MIN_WIDTH: f32 = 320.;
/// The narrowest a data column is drawn: about a dozen characters at the
/// table's 14px text, beside the cell's paddings.
pub(super) const DATA_COLUMN_MIN_WIDTH: f32 = 96.;
/// What the table holds back from its columns, so that they end short of its
/// edge and its scrollbar.
const TABLE_EDGE_RESERVE: f32 = 10.;
/// How much narrower than its share a sharing column is drawn.
const SHARED_COLUMN_GAP: f32 = 5.;
/// The most of the data columns' room a column that names its width takes
/// while others share that room: 200px of Field in a table of 400 left the
/// value 96.
const NAMED_COLUMN_MAX_SHARE: f32 = 0.4;

/// How an open entry panel and the table share the pane, in pixels.
#[derive(Debug, PartialEq)]
pub(super) struct PanelSplit {
    /// The panel's width when it opens.
    pub(super) panel: f32,
    /// How narrow and how wide the handle lets it go.
    pub(super) panel_min: f32,
    pub(super) panel_max: f32,
}

/// Divide `pane` — the width the table has, measured — between the table and
/// the entry panel, which opens at `saved` (the user's last drag) or at half.
/// `None` where the pane cannot hold the panel and a table worth drawing:
/// the panel is then shown alone, and the table is back when it closes.
///
/// The table keeps [`TABLE_MIN_WIDTH`] whatever the handle does. Both sides
/// used to be worked out from the *window*: half of it by default and three
/// quarters at most, inside a pane that is itself only about half the window
/// — so at the default window size the panel left the table its floor of
/// 100, the tick box and the row number, and the footer's filter read "Fi"
/// (#182).
pub(super) fn entry_panel_split(pane: f32, saved: Option<f32>) -> Option<PanelSplit> {
    if pane < ENTRY_PANEL_MIN_WIDTH + TABLE_MIN_WIDTH {
        return None;
    }
    let panel_min = ENTRY_PANEL_MIN_WIDTH;
    let panel_max = pane - TABLE_MIN_WIDTH;
    Some(PanelSplit {
        panel: saved.unwrap_or(pane / 2.).clamp(panel_min, panel_max),
        panel_min,
        panel_max,
    })
}

/// Covers its parent and writes the width that was laid out at into `slot`.
/// With `redraw`, a width it had not seen renders `view` again on the next
/// frame: what the view draws depends on a width that is only known after
/// layout (the `value_diff` pattern).
pub(super) fn width_probe<V: 'static>(
    slot: Rc<Cell<Option<Pixels>>>,
    view: WeakEntity<V>,
    redraw: bool,
) -> impl IntoElement {
    canvas(
        move |bounds: Bounds<Pixels>, window: &mut Window, _cx: &mut App| {
            let width = bounds.size.width;
            if slot.get() == Some(width) {
                return;
            }
            slot.set(Some(width));
            if redraw {
                window.on_next_frame(move |_, cx| {
                    let _ = view.update(cx, |_, cx| cx.notify());
                });
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// Give every column a width, for a table `table_width` wide — its own
/// width, which is less than the pane's while the entry panel is open.
///
/// A column that names a width keeps it and the ones that do not share the
/// rest — where the table has the room. Where it has not, a named column
/// takes at most [`NAMED_COLUMN_MAX_SHARE`] of what the data columns have,
/// so that the value — what the table is opened to read — is the wider of
/// the two rather than what is left over; and no data column is drawn
/// narrower than [`DATA_COLUMN_MIN_WIDTH`], whatever does not fit after
/// that scrolling sideways, last columns first.
///
/// The widths used to be worked out once, when the table was built, from an
/// estimate of the pane. Beside the entry panel that left a table of 320
/// showing the tick box, the row number, a 200px Field and 16px of the value
/// (#182, reopened), and a window resized after the key was opened kept the
/// widths of the old one — the field TTL column out of view to the right.
pub(super) fn fit_columns(mut columns: Vec<KvTableColumn>, table_width: f32) -> Vec<KvTableColumn> {
    let is_data = |column: &KvTableColumn| column.column_type == KvTableColumnType::Value;
    // The tick box and the row number are as narrow as they go: the room
    // is what they leave.
    let leading: f32 = columns
        .iter()
        .filter(|column| !is_data(column))
        .filter_map(|column| column.width)
        .sum();
    let room = table_width - TABLE_EDGE_RESERVE - leading;
    let sharing = columns.iter().filter(|column| column.width.is_none()).count();
    if sharing == 0 {
        return columns;
    }

    let most = (room * NAMED_COLUMN_MAX_SHARE).max(DATA_COLUMN_MIN_WIDTH);
    let mut remaining = room;
    for column in columns.iter_mut().filter(|column| is_data(column)) {
        let Some(width) = column.width else {
            continue;
        };
        // A width under 1 is a share of the room.
        let width = if width < 1. { width * room } else { width };
        let width = width.min(most);
        column.width = Some(width);
        remaining -= width;
    }
    let share = (remaining / sharing as f32 - SHARED_COLUMN_GAP).max(DATA_COLUMN_MIN_WIDTH);
    for column in columns.iter_mut() {
        column.width.get_or_insert(share);
    }
    columns
}

impl<T: ZedisKvFetcher> ZedisKvTable<T> {
    /// The delegate's columns: the caller's, with the row number (and the
    /// tick box) in front, fitted to a table `table_width` wide.
    pub(super) fn new_columns(
        columns: Vec<KvTableColumn>,
        selectable: bool,
        table_width: Pixels,
    ) -> Vec<KvTableColumn> {
        fit_columns(with_leading_columns(columns, selectable), table_width.as_f32())
    }

    /// The table's width before it has been laid out once: the app's own
    /// estimate of the pane (window − sidebar − key tree).
    pub(super) fn estimated_width(window: &Window, cx: &App) -> Pixels {
        let estimate = cx.global::<ZedisGlobalStore>().read(cx).content_width();
        estimate.unwrap_or(window.viewport_size().width)
    }
}
