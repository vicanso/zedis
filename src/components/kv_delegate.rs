// Copyright 2025 Tree xie.
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

use crate::assets::CustomIconName;
use crate::states::RedisValue;
use crate::states::ZedisGlobalStore;
use crate::states::ZedisServerState;
use crate::states::i18n_common;
use crate::views::{KvTableColumn, KvTableColumnType};
use gpui::App;
use gpui::Edges;
use gpui::Entity;
use gpui::SharedString;
use gpui::Window;
use gpui::div;
use gpui::prelude::*;
use gpui::px;
use gpui_component::Disableable;
use gpui_component::Icon;
use gpui_component::IconName;
use gpui_component::Sizable;
use gpui_component::StyledExt;
use gpui_component::WindowExt;
use gpui_component::button::Button;
use gpui_component::button::ButtonVariants;
use gpui_component::h_flex;
use gpui_component::label::Label;
use gpui_component::table::{Column, TableDelegate, TableState};
use rust_i18n::t;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

pub const INDEX_COLUMN_NAME: &str = "#";

pub trait ZedisKvFetcher: 'static {
    fn get(&self, row_ix: usize, col_ix: usize) -> Option<SharedString>;
    fn count(&self) -> usize;
    fn rows_count(&self) -> usize;
    fn is_eof(&self) -> bool {
        !self.is_done()
    }
    fn can_update(&self) -> bool {
        false
    }
    fn is_done(&self) -> bool;
    fn load_more(&self, _window: &mut Window, _cx: &mut App);
    fn remove(&self, index: usize, _cx: &mut App);
    fn filter(&self, keyword: SharedString, _cx: &mut App);
    fn handle_add_value(&self, _window: &mut Window, _cx: &mut App);
    fn new(server_state: Entity<ZedisServerState>, value: RedisValue) -> Self;
}
pub struct ZedisKvDelegate<T: ZedisKvFetcher> {
    table_columns: Vec<KvTableColumn>,
    processing: Rc<Cell<bool>>,
    fetcher: Arc<T>,
    columns: Vec<Column>,
    editing_row: Cell<Option<usize>>,
}

impl<T: ZedisKvFetcher> ZedisKvDelegate<T> {
    pub fn fetcher(&self) -> Arc<T> {
        self.fetcher.clone()
    }
    pub fn set_fetcher(&mut self, fetcher: T) {
        self.fetcher = Arc::new(fetcher);
        self.processing = Rc::new(Cell::new(false));
    }
    pub fn new(columns: Vec<KvTableColumn>, fetcher: T) -> Self {
        let table_columns = columns.clone();
        Self {
            table_columns,
            columns: columns
                .iter()
                .map(|item| {
                    let name = item.name.clone();
                    let mut column = Column::new(name.clone(), name.clone());
                    if let Some(width) = item.width {
                        column = column.width(width);
                    }
                    if let Some(align) = item.align {
                        column.align = align;
                    }
                    column.paddings = Some(Edges {
                        top: px(2.),
                        bottom: px(2.),
                        left: px(10.),
                        right: px(10.),
                    });
                    column
                })
                .collect::<Vec<Column>>(),
            fetcher: Arc::new(fetcher),
            processing: Rc::new(Cell::new(false)),
            editing_row: Cell::new(None),
        }
    }
}

impl<T: ZedisKvFetcher + 'static> TableDelegate for ZedisKvDelegate<T> {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.fetcher.rows_count()
    }

    fn column(&self, index: usize, _: &App) -> &Column {
        &self.columns[index]
    }
    /// Render the header cell at the given column index, default to the column name.
    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = self.column(col_ix, cx);
        let label = Label::new(column.name.clone()).text_align(column.align);
        div()
            .size_full()
            .when(column.paddings.is_some(), |this| {
                this.paddings(column.paddings.unwrap_or_default())
            })
            .child(label)
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = self.column(col_ix, cx);
        let mut base = h_flex().size_full().when(column.paddings.is_some(), |this| {
            this.paddings(column.paddings.unwrap_or_default())
        });
        let fetcher = self.fetcher();
        let support_update = fetcher.can_update();
        let processing = self.processing.clone();
        let editing_row = self.editing_row.clone();
        let is_editing = if let Some(editing_row) = editing_row.get() {
            editing_row == row_ix
        } else {
            false
        };
        // let is_editing = editing_row.get()
        if let Some(table_column) = self.table_columns.get(col_ix) {
            match table_column.ty {
                KvTableColumnType::Index => {
                    let label = Label::new((row_ix + 1).to_string()).text_align(column.align);
                    return base.child(label);
                }
                KvTableColumnType::Action => {
                    // Update button: Toggles between edit mode (pen icon) and save mode (check icon)
                    if support_update {
                        let update_btn = Button::new(("zedis-editor-table-action-update-btn", row_ix))
                            .small()
                            .ghost()
                            .mr_2()
                            .tooltip(i18n_common(cx, "update_tooltip"))
                            .when(!is_editing, |this| this.icon(Icon::new(CustomIconName::FilePenLine)))
                            .when(is_editing, |this| this.icon(Icon::new(IconName::Check)))
                            .disabled(processing.get())
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                this.delegate_mut().editing_row.set(Some(row_ix));
                                cx.stop_propagation();
                                cx.notify();
                            }));
                        base = base.child(update_btn);
                    }

                    let remove_btn = Button::new(("zedis-editor-table-action-remove-btn", row_ix))
                        .small()
                        .ghost()
                        .tooltip(i18n_common(cx, "remove_tooltip"))
                        .icon(Icon::new(CustomIconName::FileXCorner))
                        .disabled(processing.get())
                        .on_click(cx.listener(move |this, _event, window, cx| {
                            let processing_clone = this.delegate_mut().processing.clone();
                            cx.stop_propagation();
                            let value = fetcher.clone().get(row_ix, 0).unwrap_or_default();
                            let fetcher_clone = fetcher.clone();
                            window.open_dialog(cx, move |dialog, _, cx| {
                                let locale = cx.global::<ZedisGlobalStore>().locale(cx);
                                let message = t!(
                                    "common.remove_item_prompt",
                                    row = row_ix + 1,
                                    value = value,
                                    locale = locale
                                )
                                .to_string();
                                let fetcher_clone = fetcher_clone.clone();
                                let processing_clone = processing_clone.clone();
                                cx.stop_propagation();
                                dialog.confirm().child(message).on_ok(move |_, window, cx| {
                                    processing_clone.replace(true);
                                    fetcher_clone.remove(row_ix, cx);
                                    window.close_dialog(cx);
                                    true
                                })
                            });
                        }));

                    return base.child(remove_btn);
                }
                _ => {}
            }
        }
        let value = self.fetcher.get(row_ix, col_ix).unwrap_or_else(|| "--".into());
        let label = Label::new(value).text_align(column.align);
        base.child(label)
    }
    fn is_eof(&self, _: &App) -> bool {
        self.fetcher.is_eof()
    }

    fn load_more_threshold(&self) -> usize {
        50 // Load more when 50 rows from bottom
    }

    fn load_more(&mut self, window: &mut Window, cx: &mut Context<TableState<ZedisKvDelegate<T>>>) {
        if self.fetcher.is_done() {
            return;
        }
        let processing = self.processing.replace(true);
        if processing {
            return;
        }
        self.fetcher.load_more(window, cx);
    }
}
