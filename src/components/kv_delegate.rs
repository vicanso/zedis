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

use crate::states::RedisValue;
use crate::states::ZedisServerState;
use gpui::App;
use gpui::Entity;
use gpui::SharedString;
use gpui::Window;
use gpui::div;
use gpui::prelude::*;
use gpui_component::StyledExt;
use gpui_component::label::Label;
use gpui_component::table::{Column, TableDelegate, TableState};

pub const INDEX_COLUMN_NAME: &str = "#";

pub trait ZedisKvFetcher: 'static {
    fn get(&self, row_ix: usize, col_ix: usize) -> Option<SharedString>;
    fn count(&self) -> usize;
    fn rows_count(&self) -> usize;
    fn is_eof(&self) -> bool;
    fn is_done(&self) -> bool;
    fn is_initial_load(&self) -> bool;
    fn load_more(&self, _window: &mut Window, _cx: &mut App);
    fn filter(&self, keyword: SharedString, _cx: &mut App);
    fn handle_add_value(&self, _window: &mut Window, _cx: &mut App);
    fn new(server_state: Entity<ZedisServerState>, value: RedisValue) -> Self;
}
pub struct ZedisKvDelegate<T: ZedisKvFetcher> {
    loading: bool,
    fetcher: T,
    columns: Vec<Column>,
}

impl<T: ZedisKvFetcher> ZedisKvDelegate<T> {
    pub fn fetcher(&self) -> &T {
        &self.fetcher
    }
    pub fn set_fetcher(&mut self, fetcher: T) {
        self.fetcher = fetcher;
        self.loading = false;
    }
    pub fn new(columns: Vec<Column>, fetcher: T) -> Self {
        Self {
            columns,
            fetcher,
            loading: false,
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
        let value = self.fetcher.get(row_ix, col_ix).unwrap_or_else(|| "--".into());
        let column = self.column(col_ix, cx);
        let label = Label::new(value).text_align(column.align);
        div()
            .size_full()
            .when(column.paddings.is_some(), |this| {
                this.paddings(column.paddings.unwrap_or_default())
            })
            .child(label)
    }
    fn is_eof(&self, _: &App) -> bool {
        self.fetcher.is_eof()
    }

    fn load_more_threshold(&self) -> usize {
        50 // Load more when 50 rows from bottom
    }

    fn load_more(&mut self, window: &mut Window, cx: &mut Context<TableState<ZedisKvDelegate<T>>>) {
        if self.loading || self.fetcher.is_done() {
            return;
        }
        self.loading = true;
        self.fetcher.load_more(window, cx);
    }
}
