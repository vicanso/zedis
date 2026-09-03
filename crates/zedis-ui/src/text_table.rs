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

//! A read-only grid of text cells — the shape most observability panels
//! (MONITOR, keyspace events, clients, slow log, INFO, pub/sub) share:
//! fixed columns, rows of strings, sort by column, a keyword filter, a
//! hover copy button on every cell, and an optional extra action on some
//! cells. One `TableDelegate` for all of them, so a panel only builds rows.
//!
//! Text first: cells are strings, and the escape hatches are hooks — a
//! colour / icon per cell, a whole custom element for a column (a kill
//! button, a navigation chip), a sort key that is another cell, a row
//! predicate for structured filters. A row may carry more cells than
//! there are columns: the extra ones are **payload** — raw numbers to
//! sort by, ids for an action — never drawn, exported only if a panel
//! zips them with its own keys. Column titles arrive localized — this
//! crate has no i18n — so a panel rebuilds the table on a locale change,
//! which the route switch already does.

use gpui::{AnyElement, App, ClipboardItem, Edges, Hsla, SharedString, Window, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, StyledExt, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    notification::Notification,
    table::{Column, ColumnSort, TableDelegate, TableState},
};
use std::collections::VecDeque;
use std::rc::Rc;
use zedis_core::string::fast_contains_ignore_case;

/// One column: a stable `key` (what exports and sort callers name), the
/// title to draw, and a width in pixels.
#[derive(Clone)]
pub struct TextColumn {
    key: &'static str,
    title: SharedString,
    width: f32,
    sortable: bool,
    numeric: bool,
    /// Cell index the sort reads (a payload cell holding the raw value);
    /// the column's own cell when `None`.
    sort_cell: Option<usize>,
    padded: bool,
}

impl TextColumn {
    pub fn new(key: &'static str, title: impl Into<SharedString>, width: f32) -> Self {
        Self {
            key,
            title: title.into(),
            width,
            sortable: false,
            numeric: false,
            sort_cell: None,
            padded: true,
        }
    }

    /// Clicking the header sorts by this column.
    pub fn sortable(mut self) -> Self {
        self.sortable = true;
        self
    }

    /// Sort by the cell parsed as a number (unparsable cells sort last),
    /// not by its text — for counts and durations.
    pub fn numeric(mut self) -> Self {
        self.numeric = true;
        self
    }

    /// Sort by another cell of the row — a payload cell with the raw
    /// value behind a formatted display ("1.2 MB" sorts by its bytes).
    /// Implies `sortable` and `numeric`.
    pub fn sort_by_cell(mut self, cell: usize) -> Self {
        self.sort_cell = Some(cell);
        self.sortable = true;
        self.numeric = true;
        self
    }

    /// No side paddings — for a column whose cell is a centered control.
    pub fn unpadded(mut self) -> Self {
        self.padded = false;
        self
    }

    pub fn key(&self) -> &'static str {
        self.key
    }
}

/// What a [`CellAction`] runs when clicked.
pub type CellClick = Rc<dyn Fn(&mut Window, &mut App)>;

/// An extra hover button on a cell, beside the copy button — "open this
/// key", "jump to that event" — decided per cell by the panel.
pub struct CellAction {
    pub icon: IconName,
    pub tooltip: SharedString,
    pub on_click: CellClick,
}

/// `(column index, the row's cells)` → an action for that cell, or none.
pub type CellActionProvider = Rc<dyn Fn(usize, &[SharedString]) -> Option<CellAction>>;

/// How a text cell is drawn beyond its text.
#[derive(Default)]
pub struct CellStyle {
    pub color: Option<Hsla>,
    /// Drawn before the text — or instead of it with `icon_only`.
    pub icon: Option<Icon>,
    pub icon_only: bool,
}

/// `(column index, the row's cells, the app)` → the cell's style. The app
/// is there for the theme's colours.
pub type CellStyleProvider = Rc<dyn Fn(usize, &[SharedString], &App) -> CellStyle>;

/// `(row index, column index, the row's cells, window, app)` → a whole
/// element for the cell, or `None` to draw the text cell. The element
/// fills the cell; the column's paddings are not applied to it.
pub type CellRenderer = Rc<dyn Fn(usize, usize, &[SharedString], &mut Window, &mut App) -> Option<AnyElement>>;

/// A structured filter over a row, combined with the keyword.
pub type RowPredicate = Rc<dyn Fn(&[SharedString]) -> bool>;

pub struct ZedisTextTable {
    columns: Vec<TextColumn>,
    gpui_columns: Vec<Column>,
    /// Every row, in insertion order (a panel decides whether new rows go
    /// to the front or the back).
    rows: VecDeque<Vec<SharedString>>,
    /// The rows matching `keyword`, only maintained while one is set.
    filtered: Vec<Vec<SharedString>>,
    keyword: String,
    /// Column indices the keyword is matched against; empty = all.
    filter_columns: Vec<usize>,
    /// Ring-buffer cap; 0 = unbounded.
    max_rows: usize,
    copied_message: SharedString,
    /// Tooltip of the hover copy button; none when empty.
    copy_tooltip: SharedString,
    cell_action: Option<CellActionProvider>,
    cell_style: Option<CellStyleProvider>,
    cell_render: Option<CellRenderer>,
    row_filter: Option<RowPredicate>,
}

impl ZedisTextTable {
    /// `copied_message` is the toast shown after a cell is copied — passed
    /// in localized, as every string this crate shows.
    pub fn new(columns: Vec<TextColumn>, copied_message: impl Into<SharedString>) -> Self {
        let paddings = Some(Edges {
            top: px(2.),
            bottom: px(2.),
            left: px(10.),
            right: px(10.),
        });
        let gpui_columns = columns
            .iter()
            .map(|c| {
                let mut column = Column::new(c.key, c.title.clone()).width(c.width);
                if c.padded {
                    column.paddings = paddings;
                }
                if c.sortable { column.sortable() } else { column }
            })
            .collect();
        Self {
            columns,
            gpui_columns,
            rows: VecDeque::new(),
            filtered: Vec::new(),
            keyword: String::new(),
            filter_columns: Vec::new(),
            max_rows: 0,
            copied_message: copied_message.into(),
            copy_tooltip: SharedString::default(),
            cell_action: None,
            cell_style: None,
            cell_render: None,
            row_filter: None,
        }
    }

    pub fn cell_style(mut self, provider: CellStyleProvider) -> Self {
        self.cell_style = Some(provider);
        self
    }

    pub fn cell_render(mut self, renderer: CellRenderer) -> Self {
        self.cell_render = Some(renderer);
        self
    }

    /// Tooltip for the copy button on every cell — passed in localized.
    pub fn copy_tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.copy_tooltip = text.into();
        self
    }

    /// Keep at most `n` rows: a push past the cap drops from the opposite
    /// end, so the table is a ring buffer for live feeds.
    pub fn max_rows(mut self, n: usize) -> Self {
        self.max_rows = n;
        self
    }

    /// Match the keyword against these columns only (by key).
    pub fn filter_columns(mut self, keys: &[&str]) -> Self {
        self.filter_columns = self
            .columns
            .iter()
            .enumerate()
            .filter(|(_, c)| keys.contains(&c.key))
            .map(|(ix, _)| ix)
            .collect();
        self
    }

    pub fn cell_action(mut self, provider: CellActionProvider) -> Self {
        self.cell_action = Some(provider);
        self
    }

    pub fn column_keys(&self) -> Vec<&'static str> {
        self.columns.iter().map(|c| c.key).collect()
    }

    /// Newest row first (live feeds); trims the oldest past the cap. The
    /// filter is *not* re-applied per push — call [`refilter`](Self::refilter)
    /// once after a batch.
    pub fn push_front(&mut self, row: Vec<SharedString>) {
        self.rows.push_front(row);
        if self.max_rows > 0 {
            while self.rows.len() > self.max_rows {
                self.rows.pop_back();
            }
        }
    }

    /// Append a row; trims the oldest (front) past the cap.
    pub fn push_back(&mut self, row: Vec<SharedString>) {
        self.rows.push_back(row);
        if self.max_rows > 0 {
            while self.rows.len() > self.max_rows {
                self.rows.pop_front();
            }
        }
    }

    /// Replace every row.
    pub fn set_rows(&mut self, rows: Vec<Vec<SharedString>>) {
        self.rows = rows.into();
        self.refilter();
    }

    pub fn clear(&mut self) {
        self.rows.clear();
        self.filtered.clear();
    }

    /// Set (or clear, with `""`) the keyword and recompute the visible rows.
    pub fn set_filter(&mut self, keyword: &str) {
        self.keyword = keyword.trim().to_lowercase();
        self.refilter();
    }

    /// Set (or clear) the structured filter; rows must pass it *and* the
    /// keyword. Recomputes the visible rows.
    pub fn set_row_filter(&mut self, predicate: Option<RowPredicate>) {
        self.row_filter = predicate;
        self.refilter();
    }

    /// Every row, filtered or not — for a panel that aggregates over the
    /// whole set.
    pub fn all_rows(&self) -> Vec<Vec<SharedString>> {
        self.rows.iter().cloned().collect()
    }

    /// Recompute the visible rows against the keyword and the row filter.
    pub fn refilter(&mut self) {
        if !self.is_filtered() {
            self.filtered.clear();
            return;
        }
        let keyword = self.keyword.clone();
        let matches_keyword = |row: &Vec<SharedString>| {
            if keyword.is_empty() {
                return true;
            }
            if self.filter_columns.is_empty() {
                row.iter().any(|cell| fast_contains_ignore_case(cell, &keyword))
            } else {
                self.filter_columns
                    .iter()
                    .filter_map(|&ix| row.get(ix))
                    .any(|cell| fast_contains_ignore_case(cell, &keyword))
            }
        };
        let predicate = self.row_filter.clone();
        self.filtered = self
            .rows
            .iter()
            .filter(|row| matches_keyword(row) && predicate.as_ref().is_none_or(|p| p(row)))
            .cloned()
            .collect();
    }

    pub fn is_filtered(&self) -> bool {
        !self.keyword.is_empty() || self.row_filter.is_some()
    }

    /// Rows the table shows: the filtered set while a keyword is active.
    pub fn visible_len(&self) -> usize {
        if self.is_filtered() {
            self.filtered.len()
        } else {
            self.rows.len()
        }
    }

    pub fn total_len(&self) -> usize {
        self.rows.len()
    }

    /// A copy of the visible rows, for exports.
    pub fn visible_rows(&self) -> Vec<Vec<SharedString>> {
        if self.is_filtered() {
            self.filtered.clone()
        } else {
            self.rows.iter().cloned().collect()
        }
    }

    fn visible_row(&self, ix: usize) -> Option<&Vec<SharedString>> {
        if self.is_filtered() {
            self.filtered.get(ix)
        } else {
            self.rows.get(ix)
        }
    }

    fn compare(numeric: bool, a: &SharedString, b: &SharedString) -> std::cmp::Ordering {
        if numeric {
            let parse = |s: &SharedString| s.trim().parse::<f64>().ok();
            match (parse(a), parse(b)) {
                (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.cmp(b),
            }
        } else {
            a.cmp(b)
        }
    }
}

impl TableDelegate for ZedisTextTable {
    fn columns_count(&self, _cx: &App) -> usize {
        self.gpui_columns.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.visible_len()
    }

    fn column(&self, index: usize, _cx: &App) -> Column {
        self.gpui_columns[index].clone()
    }

    fn perform_sort(&mut self, col_ix: usize, sort: ColumnSort, _: &mut Window, _: &mut Context<TableState<Self>>) {
        let Some(column) = self.columns.get(col_ix) else {
            return;
        };
        let numeric = column.numeric;
        let cell_ix = column.sort_cell.unwrap_or(col_ix);
        let empty = SharedString::default();
        let cmp = |a: &Vec<SharedString>, b: &Vec<SharedString>| {
            Self::compare(
                numeric,
                a.get(cell_ix).unwrap_or(&empty),
                b.get(cell_ix).unwrap_or(&empty),
            )
        };
        let ascending = matches!(sort, ColumnSort::Ascending);
        if self.is_filtered() {
            self.filtered
                .sort_by(|a, b| if ascending { cmp(a, b) } else { cmp(b, a) });
        } else {
            let mut rows: Vec<Vec<SharedString>> = self.rows.drain(..).collect();
            rows.sort_by(|a, b| if ascending { cmp(a, b) } else { cmp(b, a) });
            self.rows = rows.into();
        }
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = &self.gpui_columns[col_ix];
        // h_flex (items_center) matches render_td, so header text is
        // vertically centered like the cells.
        h_flex()
            .size_full()
            .when_some(column.paddings, |this, paddings| this.paddings(paddings))
            .child(
                Label::new(column.name.clone())
                    .text_align(column.align)
                    .text_color(cx.theme().muted_foreground)
                    .text_sm()
                    .flex_1(),
            )
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        if let Some(renderer) = &self.cell_render
            && let Some(row) = self.visible_row(row_ix)
            && let Some(element) = renderer(row_ix, col_ix, row, window, cx)
        {
            return element;
        }
        let column = &self.gpui_columns[col_ix];
        let (value, action, style): (SharedString, Option<CellAction>, CellStyle) = match self.visible_row(row_ix) {
            Some(row) => (
                row.get(col_ix).cloned().unwrap_or_else(|| "--".into()),
                self.cell_action.as_ref().and_then(|provider| provider(col_ix, row)),
                self.cell_style
                    .as_ref()
                    .map(|provider| provider(col_ix, row, cx))
                    .unwrap_or_default(),
            ),
            None => ("--".into(), None, CellStyle::default()),
        };
        let group_name: SharedString = format!("text-td-{row_ix}-{col_ix}").into();
        let copied_message = self.copied_message.clone();
        let cell_id = row_ix * 100 + col_ix;
        let mut copy_button = Button::new(("text-td-copy", cell_id))
            .ghost()
            .icon(IconName::Copy)
            .on_click(move |_, window, cx: &mut App| {
                cx.write_to_clipboard(ClipboardItem::new_string(value.to_string()));
                window.push_notification(Notification::info(copied_message.clone()), cx);
            });
        if !self.copy_tooltip.is_empty() {
            copy_button = copy_button.tooltip(self.copy_tooltip.clone());
        }
        let label_value = self
            .visible_row(row_ix)
            .and_then(|row| row.get(col_ix).cloned())
            .unwrap_or_else(|| "--".into());
        let mut label = Label::new(label_value).text_align(column.align).text_ellipsis();
        if let Some(color) = style.color {
            label = label.text_color(color);
        }
        h_flex()
            .size_full()
            .when_some(column.paddings, |this, paddings| this.paddings(paddings))
            .group(group_name.clone())
            .overflow_hidden()
            .when_some(style.icon, |this, icon| this.child(icon))
            .when(!style.icon_only, |this| this.child(label.flex_1().min_w_0()))
            .when(style.icon_only, |this| this.child(h_flex().flex_1()))
            .child(
                // Hover-only buttons: copy, plus the panel's own action. A
                // flex row on purpose — a bare `div()` is block layout and
                // stacks the two buttons vertically inside a one-line cell,
                // clipping both.
                h_flex()
                    .id(("text-td-actions", cell_id))
                    .gap_0p5()
                    .invisible()
                    .group_hover(group_name, |style| style.visible())
                    .flex_none()
                    .on_click(|_, _, cx: &mut App| cx.stop_propagation())
                    .when_some(action, |this, action| {
                        let on_click = action.on_click.clone();
                        this.child(
                            Button::new(("text-td-action", cell_id))
                                .ghost()
                                .icon(action.icon)
                                .tooltip(action.tooltip.clone())
                                .on_click(move |_, window, cx: &mut App| on_click(window, cx)),
                        )
                    })
                    .child(copy_button),
            )
            .into_any_element()
    }

    fn has_more(&self, _cx: &App) -> bool {
        false
    }

    fn load_more_threshold(&self) -> usize {
        0
    }

    fn load_more(&mut self, _window: &mut Window, _cx: &mut Context<TableState<Self>>) {}
}

#[cfg(test)]
mod tests {
    use super::{TextColumn, ZedisTextTable};
    use gpui::SharedString;

    fn row(cells: &[&str]) -> Vec<SharedString> {
        cells.iter().map(|c| SharedString::from(c.to_string())).collect()
    }

    fn table() -> ZedisTextTable {
        ZedisTextTable::new(
            vec![
                TextColumn::new("time", "Time", 100.),
                TextColumn::new("cmd", "Command", 100.).sortable(),
                TextColumn::new("n", "Calls", 60.).sortable().numeric(),
            ],
            "copied",
        )
        .filter_columns(&["cmd"])
    }

    #[test]
    fn ring_buffer_keeps_the_newest() {
        let mut t = table().max_rows(2);
        t.push_front(row(&["1", "GET", "1"]));
        t.push_front(row(&["2", "SET", "2"]));
        t.push_front(row(&["3", "DEL", "3"]));
        assert_eq!(t.total_len(), 2);
        assert_eq!(t.visible_rows()[0][0].as_ref(), "3");
        assert_eq!(t.visible_rows()[1][0].as_ref(), "2");
        let mut t = table().max_rows(2);
        t.push_back(row(&["1", "GET", "1"]));
        t.push_back(row(&["2", "SET", "2"]));
        t.push_back(row(&["3", "DEL", "3"]));
        assert_eq!(t.visible_rows()[0][0].as_ref(), "2");
    }

    #[test]
    fn keyword_filters_only_the_chosen_columns_case_insensitively() {
        let mut t = table();
        t.set_rows(vec![
            row(&["1", "GET", "1"]),
            row(&["2", "get-ish", "2"]),
            row(&["3", "SET", "3"]),
        ]);
        t.set_filter("get");
        assert!(t.is_filtered());
        assert_eq!(t.visible_len(), 2);
        assert_eq!(t.total_len(), 3);
        // The time column is not searched.
        t.set_filter("3");
        assert_eq!(t.visible_len(), 0);
        t.set_filter("");
        assert_eq!(t.visible_len(), 3);
        // A push after a filter shows up once refiltered.
        t.set_filter("del");
        t.push_front(row(&["4", "DEL", "4"]));
        assert_eq!(t.visible_len(), 0);
        t.refilter();
        assert_eq!(t.visible_len(), 1);
    }

    #[test]
    fn row_filter_combines_with_the_keyword_and_sort_reads_a_payload_cell() {
        use std::rc::Rc;
        let mut t = ZedisTextTable::new(
            vec![
                TextColumn::new("name", "Name", 100.),
                // Displays "1.2 MB" but sorts by the raw bytes in cell 2.
                TextColumn::new("size", "Size", 100.).sort_by_cell(2),
            ],
            "copied",
        );
        t.set_rows(vec![
            row(&["a", "9 B", "9"]),
            row(&["b", "1.2 KB", "1200"]),
            row(&["c", "10 B", "10"]),
        ]);
        t.set_row_filter(Some(Rc::new(|cells: &[SharedString]| cells[0].as_ref() != "b")));
        assert!(t.is_filtered());
        assert_eq!(t.visible_len(), 2);
        t.set_filter("c");
        assert_eq!(t.visible_len(), 1, "keyword and predicate both apply");
        t.set_filter("");
        t.set_row_filter(None);
        assert!(!t.is_filtered());
        // Payload cells are carried but not counted as columns.
        assert_eq!(t.column_keys().len(), 2);
        assert_eq!(t.visible_rows()[1].len(), 3);
    }

    #[test]
    fn numeric_columns_compare_as_numbers() {
        use super::ZedisTextTable as T;
        let a = SharedString::from("9");
        let b = SharedString::from("10");
        assert_eq!(
            T::compare(false, &a, &b),
            std::cmp::Ordering::Greater,
            "text: \"9\" > \"10\""
        );
        assert_eq!(T::compare(true, &a, &b), std::cmp::Ordering::Less, "number: 9 < 10");
        // Unparsable cells sort after numbers.
        let n = SharedString::from("n/a");
        assert_eq!(T::compare(true, &b, &n), std::cmp::Ordering::Less);
    }
}
