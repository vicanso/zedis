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

use crate::error::Error;
use gpui::App;
use gpui::AppContext;
use gpui::AsyncApp;
use gpui::SharedString;
use gpui::Window;
use gpui::prelude::*;
use gpui_component::label::Label;
use gpui_component::table::{Column, ColumnSort, Table, TableDelegate, TableState};
use std::sync::Arc;
use tracing::error;

type Result<T, E = Error> = std::result::Result<T, E>;

pub const INDEX_COLUMN_NAME: &str = "#";

pub trait ZedisTableFetcher: Sized + 'static {
    fn get(&self, row_ix: usize, col_ix: usize) -> Option<SharedString>;
    fn rows_count(&self) -> usize;
    fn is_eof(&self) -> bool;
    fn load_more(&self, _window: &mut Window, _cx: &mut App);
}
pub struct ZedisTableDelegate<T: ZedisTableFetcher> {
    loading: bool,
    fetcher: T,
    columns: Vec<Column>,
}

impl<T: ZedisTableFetcher> ZedisTableDelegate<T> {
    pub fn set_fetcher(&mut self, fetcher: T) {
        self.fetcher = fetcher;
        self.loading = false;
    }
    pub fn new(columns: Vec<SharedString>, fetcher: T) -> Self {
        Self {
            columns: columns
                .iter()
                .map(|item| Column::new(item.clone(), item.clone()))
                .collect(),
            fetcher,
            loading: false,
        }
    }
}

impl<T: ZedisTableFetcher + 'static> TableDelegate for ZedisTableDelegate<T> {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.fetcher.rows_count()
    }

    fn column(&self, index: usize, _: &App) -> &Column {
        &self.columns[index]
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let value = self.fetcher.get(row_ix, col_ix).unwrap_or_else(|| "--".into());
        Label::new(value).into_any_element()
    }
    fn is_eof(&self, _: &App) -> bool {
        self.fetcher.is_eof()
    }

    fn load_more_threshold(&self) -> usize {
        50 // Load more when 50 rows from bottom
    }
    fn load_more(&mut self, window: &mut Window, cx: &mut Context<TableState<ZedisTableDelegate<T>>>) {
        if self.loading {
            return;
        }
        self.loading = true;

        self.fetcher.load_more(window, cx);
    }
}
