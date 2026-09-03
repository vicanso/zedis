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

//! Row → cell conversion and table builders for the prefix-group and
//! single-key result tables. Payload cells carry the raw numbers the
//! sortable columns order by and the CSV exports write out.

use super::*;
use std::cell::Cell;
use std::rc::Rc;
use zedis_ui::{CellAction, CellActionProvider, TextColumn, ZedisTextTable};

const TYPE_KEY_WIDTH: f32 = 140.;
const MEMORY_KEY_WIDTH: f32 = 200.;
const COUNT_KEY_WIDTH: f32 = 150.;
const TTL_KEY_WIDTH: f32 = 120.;
const HEAT_KEY_WIDTH: f32 = 130.;
const PERM_KEY_WIDTH: f32 = 130.;

/// Payload cells of a prefix row, after its six columns.
pub(super) const PREFIX_CELL_KEY_COUNT: usize = 6;
pub(super) const PREFIX_CELL_MEMORY_BYTES: usize = 7;
pub(super) const PREFIX_CELL_AVG_TTL_SECS: usize = 8;
pub(super) const PREFIX_CELL_PERM_COUNT: usize = 9;

/// Payload cells of a single-key row, after its five columns.
pub(super) const KEY_CELL_MEMORY_BYTES: usize = 5;
pub(super) const KEY_CELL_TTL_SECS: usize = 6;
pub(super) const KEY_CELL_HEAT: usize = 7;

impl PrefixRow {
    pub(super) fn cells(&self) -> Vec<SharedString> {
        vec![
            self.prefix.clone(),
            self.display_key_count.clone(),
            self.memory.clone(),
            self.avg_ttl.clone(),
            self.perm_display.clone(),
            self.types.clone(),
            self.key_count.to_string().into(),
            self.memory_bytes.to_string().into(),
            self.avg_ttl_secs.to_string().into(),
            self.perm_count.to_string().into(),
        ]
    }
}

impl SingleKeyRow {
    pub(super) fn cells(&self) -> Vec<SharedString> {
        vec![
            self.key.clone(),
            self.memory.clone(),
            self.ttl.clone(),
            self.key_type.clone(),
            self.heat_display.clone(),
            self.memory_bytes.to_string().into(),
            self.ttl_secs.to_string().into(),
            heat_sort_key(self.heat).to_string().into(),
        ]
    }
}

/// The prefix-group table. The prefix cell jumps to the key tree filtered
/// by that prefix — unless the rows came from an offline RDB file, which
/// `offline` (shared with the view) says at click time.
pub(super) fn prefix_table(
    server_state: Entity<ZedisServerState>,
    offline: Rc<Cell<bool>>,
    window: &mut Window,
    cx: &mut gpui::App,
) -> ZedisTextTable {
    let content_width = content_area_width(window, cx).as_f32();
    // Padding offsets keep the table from growing a horizontal scrollbar.
    let padding_offset = 16.0;
    let scrollbar_offset = 10.0;
    let prefix_w = content_width
        - COUNT_KEY_WIDTH
        - MEMORY_KEY_WIDTH
        - TYPE_KEY_WIDTH
        - TTL_KEY_WIDTH
        - PERM_KEY_WIDTH
        - padding_offset
        - scrollbar_offset;
    let title = |key: &'static str| i18n_memory_analysis(cx, key);
    let columns = vec![
        TextColumn::new(COL_PREFIX, title(COL_PREFIX), prefix_w).sortable(),
        TextColumn::new(COL_KEY_COUNT, title(COL_KEY_COUNT), COUNT_KEY_WIDTH).sort_by_cell(PREFIX_CELL_KEY_COUNT),
        TextColumn::new(COL_MEMORY, title(COL_MEMORY), MEMORY_KEY_WIDTH).sort_by_cell(PREFIX_CELL_MEMORY_BYTES),
        TextColumn::new(COL_AVG_TTL, title(COL_AVG_TTL), TTL_KEY_WIDTH).sort_by_cell(PREFIX_CELL_AVG_TTL_SECS),
        TextColumn::new(COL_PERM_COUNT, title(COL_PERM_COUNT), PERM_KEY_WIDTH).sort_by_cell(PREFIX_CELL_PERM_COUNT),
        TextColumn::new(COL_TYPES, title(COL_TYPES), TYPE_KEY_WIDTH).sortable(),
    ];
    let tooltip = i18n_common(cx, "search_prefix_tooltip");
    let jump: CellActionProvider = Rc::new(move |col_ix, cells| {
        if col_ix != 0 || offline.get() {
            return None;
        }
        let prefix = cells.first()?.clone();
        let server_state = server_state.clone();
        Some(CellAction {
            icon: IconName::Search,
            tooltip: tooltip.clone(),
            on_click: Rc::new(move |_window, cx| search_keys_in_tree(&server_state, prefix.clone(), cx)),
        })
    });
    ZedisTextTable::new(columns, i18n_common(cx, "copied_to_clipboard"))
        .copy_tooltip(i18n_common(cx, "copy_cell_tooltip"))
        .cell_action(jump)
}

/// The single-key Top-N table. The key cell opens the key in the editor —
/// again not for offline rows, which have no live key to open.
pub(super) fn single_key_table(
    server_state: Entity<ZedisServerState>,
    offline: Rc<Cell<bool>>,
    window: &mut Window,
    cx: &mut gpui::App,
) -> ZedisTextTable {
    let content_width = content_area_width(window, cx).as_f32();
    let padding_offset = 16.0;
    let scrollbar_offset = 10.0;
    let key_w = content_width
        - MEMORY_KEY_WIDTH
        - TTL_KEY_WIDTH
        - TYPE_KEY_WIDTH
        - HEAT_KEY_WIDTH
        - padding_offset
        - scrollbar_offset;
    let title = |key: &'static str| i18n_memory_analysis(cx, key);
    let columns = vec![
        TextColumn::new(COL_KEY, title(COL_KEY), key_w).sortable(),
        TextColumn::new(COL_MEMORY, title(COL_MEMORY), MEMORY_KEY_WIDTH).sort_by_cell(KEY_CELL_MEMORY_BYTES),
        TextColumn::new(COL_TTL, title(COL_TTL), TTL_KEY_WIDTH).sort_by_cell(KEY_CELL_TTL_SECS),
        TextColumn::new(COL_KEY_TYPE, title(COL_KEY_TYPE), TYPE_KEY_WIDTH).sortable(),
        TextColumn::new(COL_HEAT, title(COL_HEAT), HEAT_KEY_WIDTH).sort_by_cell(KEY_CELL_HEAT),
    ];
    let tooltip = i18n_common(cx, "open_key_tooltip");
    let jump: CellActionProvider = Rc::new(move |col_ix, cells| {
        if col_ix != 0 || offline.get() {
            return None;
        }
        let key = cells.first()?.clone();
        let server_state = server_state.clone();
        Some(CellAction {
            icon: IconName::Search,
            tooltip: tooltip.clone(),
            on_click: Rc::new(move |_window, cx| open_key_in_editor(&server_state, key.clone(), cx)),
        })
    });
    ZedisTextTable::new(columns, i18n_common(cx, "copied_to_clipboard"))
        .copy_tooltip(i18n_common(cx, "copy_cell_tooltip"))
        .cell_action(jump)
}
