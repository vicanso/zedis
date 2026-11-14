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

//! Row types, sort options, cell conversions, drill-down state, and table
//! builders for the prefix-group and single-key result tables. Payload cells
//! carry the raw numbers the sortable columns order by and the CSV exports
//! write out.

use super::*;
use crate::views::open_pattern_ops_dialog_with_pattern;
use gpui::WeakEntity;
use std::cell::Cell;
use std::rc::Rc;
use zedis_ui::{CellAction, CellActionProvider, TextColumn, ZedisTextTable};

const TYPE_KEY_WIDTH: f32 = 140.;
/// The least the prefix / key column is drawn at; under it the table
/// scrolls sideways.
const NAME_COLUMN_MIN_WIDTH: f32 = 200.;
/// The page's side paddings around each table: what the table is narrower
/// than the page by, for its first frame.
const PAGE_PADDING: f32 = 16.;
const MEMORY_KEY_WIDTH: f32 = 200.;
const COUNT_KEY_WIDTH: f32 = 150.;
const TTL_KEY_WIDTH: f32 = 120.;
const HEAT_KEY_WIDTH: f32 = 130.;
const PERM_KEY_WIDTH: f32 = 130.;

// ─── Column constants ────────────────────────────────────────────────────────

pub(super) const COL_PREFIX: &str = "prefix";
pub(super) const COL_KEY_COUNT: &str = "key_count";
pub(super) const COL_MEMORY: &str = "memory";
pub(super) const COL_TYPES: &str = "types";
pub(super) const COL_AVG_TTL: &str = "avg_ttl";
pub(super) const COL_PERM_COUNT: &str = "perm_count";
pub(super) const COL_KEY: &str = "key";
pub(super) const COL_KEY_TYPE: &str = "key_type";
pub(super) const COL_TTL: &str = "ttl";
pub(super) const COL_HEAT: &str = "heat";

/// Payload cells of a prefix row, after its six columns.
pub(super) const PREFIX_CELL_KEY_COUNT: usize = 6;
pub(super) const PREFIX_CELL_MEMORY_BYTES: usize = 7;
pub(super) const PREFIX_CELL_AVG_TTL_SECS: usize = 8;
pub(super) const PREFIX_CELL_PERM_COUNT: usize = 9;
/// The path behind the displayed `user:session:*`, and whether the map
/// holds a level below it — what the drill-down action reads.
pub(super) const PREFIX_CELL_PATH: usize = 10;
pub(super) const PREFIX_CELL_HAS_CHILDREN: usize = 11;

/// Payload cells of a single-key row, after its five columns.
pub(super) const KEY_CELL_MEMORY_BYTES: usize = 5;
pub(super) const KEY_CELL_TTL_SECS: usize = 6;
pub(super) const KEY_CELL_HEAT: usize = 7;

// ─── Row types ───────────────────────────────────────────────────────────────

/// A row in the prefix-group table.
#[derive(Clone, Debug)]
pub(super) struct PrefixRow {
    /// The prefix path without the wildcard ("user:session") — what a
    /// drill-down takes as its new root.
    pub(super) path: SharedString,
    /// Whether the map holds a level below this path, i.e. whether the row
    /// can be drilled into.
    pub(super) has_children: bool,
    /// e.g. "user:*"
    pub(super) prefix: SharedString,
    /// Estimated key count (sampled × 1/ratio)
    pub(super) key_count: u64,
    /// Display key count (with "~" prefix)
    pub(super) display_key_count: SharedString,
    /// Estimated memory in bytes
    pub(super) memory_bytes: u64,
    /// Human-readable estimated memory (with "~" prefix)
    pub(super) memory: SharedString,
    /// Comma-separated key types
    pub(super) types: SharedString,
    /// Display average TTL (with "~" prefix)
    pub(super) avg_ttl: SharedString,
    /// Average TTL in seconds (-1 means no expiry)
    pub(super) avg_ttl_secs: f64,
    /// Estimated count of keys with no expiry (TTL == -1) under this prefix.
    /// A high value on a cache prefix is a memory-leak red flag.
    pub(super) perm_count: u64,
    /// Display permanent-key count (with "~" prefix when sampled)
    pub(super) perm_display: SharedString,
}

/// A row in the single-key table.
#[derive(Clone, Debug)]
pub(super) struct SingleKeyRow {
    /// Full key name
    pub(super) key: SharedString,
    /// Actual memory in bytes
    pub(super) memory_bytes: u64,
    /// Human-readable memory
    pub(super) memory: SharedString,
    /// Key type
    pub(super) key_type: SharedString,
    /// TTL display string
    pub(super) ttl: SharedString,
    /// TTL in seconds for sorting (-1 = no expiry, -2 = not exists)
    pub(super) ttl_secs: i64,
    /// Heat metric (FREQ counter / IDLETIME seconds / unknown).
    pub(super) heat: HeatMetric,
    /// Pre-formatted heat cell (e.g. "12 hits", "3m idle", "—").
    pub(super) heat_display: SharedString,
}

/// Comparable signed key for the heat metric — higher = hotter so the
/// default descending sort puts hot keys at the top regardless of which
/// metric is in play.
pub(super) fn heat_sort_key(heat: HeatMetric) -> i64 {
    match heat {
        HeatMetric::Freq(v) => v as i64,
        HeatMetric::IdleTime(v) => -(v as i64),
        HeatMetric::None => i64::MIN,
    }
}

pub(super) fn format_heat(heat: HeatMetric) -> SharedString {
    match heat {
        HeatMetric::None => "—".into(),
        HeatMetric::Freq(v) => format!("{v} hits").into(),
        HeatMetric::IdleTime(secs) => {
            if secs == 0 {
                "0s idle".into()
            } else {
                format!("{} idle", format_duration(Duration::from_secs(secs))).into()
            }
        }
    }
}

/// What to sort the TopN single-key list by. Hot/Cold only meaningful when
/// the heat metric is available; the toggle hides those when it is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(super) enum SortMode {
    #[default]
    Size,
    /// FREQ desc (LFU) or IDLETIME asc (LRU) — most-active first.
    Hot,
    /// Inverse of Hot. Cold keys = eviction candidates.
    Cold,
}

/// One entry of the "rank by" dropdown.
#[derive(Clone, Debug)]
pub(super) struct SortOption {
    pub(super) label: SharedString,
    pub(super) mode: SortMode,
}

impl SelectItem for SortOption {
    type Value = SortMode;
    fn title(&self) -> SharedString {
        self.label.clone()
    }
    fn value(&self) -> &Self::Value {
        &self.mode
    }
}

/// Ranking choices for the single-key table. Hot/Cold need a heat metric
/// (`maxmemory-policy` must be LFU or LRU), so they are simply absent when the
/// server exposes neither: a `Select` cannot grey out one of its items, and an
/// option that silently does nothing is worse than one that isn't offered.
pub(super) fn sort_options(heat_available: bool, cx: &gpui::App) -> Vec<SortOption> {
    let mut options = vec![SortOption {
        label: i18n_memory_analysis(cx, "rank_size"),
        mode: SortMode::Size,
    }];
    if heat_available {
        options.push(SortOption {
            label: i18n_memory_analysis(cx, "rank_hot"),
            mode: SortMode::Hot,
        });
        options.push(SortOption {
            label: i18n_memory_analysis(cx, "rank_cold"),
            mode: SortMode::Cold,
        });
    }
    options
}

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
            self.path.clone(),
            if self.has_children {
                "1".into()
            } else {
                SharedString::default()
            },
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

impl ZedisMemoryAnalysis {
    /// What the displayed numbers must be scaled by: the run's sample ratio
    /// online, 1.0 for an RDB file (every key in it was read).
    pub(super) fn row_ratio(&self) -> f32 {
        if self.rdb_file.is_some() {
            1.0
        } else {
            self.run_ratio.unwrap_or(self.ratio)
        }
    }

    /// Show the children of `root` in the prefix table (empty = top level).
    pub(super) fn set_prefix_root(&mut self, root: SharedString, cx: &mut gpui::Context<Self>) {
        self.prefix_root = root;
        let separator = self.server_state.read(cx).key_separator().to_string();
        let rows = build_prefix_rows(&self.prefix_map, self.row_ratio(), &separator, &self.prefix_root);
        self.prefix_count = rows.len();
        self.prefix_table.update(cx, |state, _| {
            state
                .delegate_mut()
                .set_rows(rows.iter().map(PrefixRow::cells).collect());
        });
        self.prefix_rows = rows;
        cx.notify();
    }

    /// The breadcrumb's steps: every ancestor of the current root, each with
    /// the path that returns to it. Empty at the top level.
    pub(super) fn prefix_breadcrumb(&self, cx: &gpui::App) -> Vec<(SharedString, SharedString)> {
        if self.prefix_root.is_empty() {
            return Vec::new();
        }
        let separator = self.server_state.read(cx).key_separator().to_string();
        if separator.is_empty() {
            return vec![(self.prefix_root.clone(), self.prefix_root.clone())];
        }
        let mut steps = Vec::new();
        let mut path = String::new();
        for segment in self.prefix_root.split(separator.as_str()) {
            if !path.is_empty() {
                path.push_str(&separator);
            }
            path.push_str(segment);
            steps.push((
                SharedString::from(segment.to_string()),
                SharedString::from(path.clone()),
            ));
        }
        steps
    }

    /// Whether the sample carries encodings at all: a server whose probe
    /// found `OBJECT ENCODING` missing or denied leaves every one empty, and
    /// then there is no second cut to offer.
    pub(super) fn has_encodings(&self) -> bool {
        self.type_map.keys().any(|(_, encoding)| !encoding.is_empty())
    }

    /// Rebuild the breakdown rows — after a scan round, or when the card's
    /// type / encoding toggle flips.
    pub(super) fn rebuild_type_rows(&mut self, cx: &gpui::App) {
        let unknown = i18n_memory_analysis(cx, "types_unknown");
        let by_encoding = self.type_by_encoding && self.has_encodings();
        self.type_rows = build_type_rows(&self.type_map, self.row_ratio(), by_encoding, &unknown);
    }

    pub(super) fn set_type_by_encoding(&mut self, by_encoding: bool, cx: &mut gpui::Context<Self>) {
        if self.type_by_encoding == by_encoding {
            return;
        }
        self.type_by_encoding = by_encoding;
        self.rebuild_type_rows(cx);
        cx.notify();
    }

    pub(super) fn set_sort_mode(&mut self, mode: SortMode, cx: &mut gpui::Context<Self>) {
        if self.sort_mode == mode {
            return;
        }
        self.sort_mode = mode;
        let rows = self.single_groups.rows_for(mode);
        self.single_count = rows.len();
        self.single_table.update(cx, |s, _| {
            s.delegate_mut()
                .set_rows(rows.iter().map(SingleKeyRow::cells).collect())
        });
        self.single_rows = rows;
        cx.notify();
    }
}

/// The prefix-group table. The prefix cell carries two hover actions:
/// drill into the next prefix level (only once a finished run left its map
/// on the view, which `drillable` says at click time), and jump to the key
/// tree filtered by that prefix — the latter not for rows that came from an
/// offline RDB file, which `offline` says the same way.
pub(super) fn prefix_table(
    server_state: Entity<ZedisServerState>,
    offline: Rc<Cell<bool>>,
    drillable: Rc<Cell<bool>>,
    view: WeakEntity<ZedisMemoryAnalysis>,
    window: &mut Window,
    cx: &mut gpui::App,
) -> ZedisTextTable {
    let title = |key: &'static str| i18n_memory_analysis(cx, key);
    let columns = vec![
        // The prefix has what the figures leave, and follows the table's
        // width (`fitted_table`).
        TextColumn::new(COL_PREFIX, title(COL_PREFIX), NAME_COLUMN_MIN_WIDTH)
            .sortable()
            .fills(),
        TextColumn::new(COL_KEY_COUNT, title(COL_KEY_COUNT), COUNT_KEY_WIDTH).sort_by_cell(PREFIX_CELL_KEY_COUNT),
        TextColumn::new(COL_MEMORY, title(COL_MEMORY), MEMORY_KEY_WIDTH).sort_by_cell(PREFIX_CELL_MEMORY_BYTES),
        TextColumn::new(COL_AVG_TTL, title(COL_AVG_TTL), TTL_KEY_WIDTH).sort_by_cell(PREFIX_CELL_AVG_TTL_SECS),
        TextColumn::new(COL_PERM_COUNT, title(COL_PERM_COUNT), PERM_KEY_WIDTH).sort_by_cell(PREFIX_CELL_PERM_COUNT),
        TextColumn::new(COL_TYPES, title(COL_TYPES), TYPE_KEY_WIDTH).sortable(),
    ];
    let search_tooltip = i18n_common(cx, "search_prefix_tooltip");
    let drill_tooltip = i18n_memory_analysis(cx, "prefix_drill_tooltip");
    let pattern_tooltip = i18n_memory_analysis(cx, "remediate_pattern_ops");
    let jump: CellActionProvider = Rc::new(move |col_ix, cells| {
        let mut actions = Vec::new();
        if col_ix != 0 {
            return actions;
        }
        if drillable.get()
            && cells.get(PREFIX_CELL_HAS_CHILDREN).is_some_and(|c| c.as_ref() == "1")
            && let Some(path) = cells.get(PREFIX_CELL_PATH).cloned()
        {
            let view = view.clone();
            actions.push(CellAction {
                icon: IconName::ChevronRight,
                tooltip: drill_tooltip.clone(),
                on_click: Rc::new(move |_window, cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| this.set_prefix_root(path.clone(), cx));
                    }
                }),
            });
        }
        if !offline.get()
            && let Some(prefix) = cells.first().cloned()
        {
            let server_state_search = server_state.clone();
            let prefix_search = prefix.clone();
            actions.push(CellAction {
                icon: IconName::Search,
                tooltip: search_tooltip.clone(),
                on_click: Rc::new(move |_window, cx| {
                    search_keys_in_tree(&server_state_search, prefix_search.clone(), cx)
                }),
            });
            let server_state_ops = server_state.clone();
            let pattern = normalize_prefix_glob(prefix.as_ref());
            actions.push(CellAction {
                icon: IconName::Settings,
                tooltip: pattern_tooltip.clone(),
                on_click: Rc::new(move |window, cx| {
                    open_pattern_ops_dialog_with_pattern(server_state_ops.clone(), pattern.clone(), window, cx);
                }),
            });
        }
        actions
    });
    ZedisTextTable::new(columns, i18n_common(cx, "copied_to_clipboard"))
        .copy_tooltip(i18n_common(cx, "copy_cell_tooltip"))
        .cell_action(jump)
        .fitted_to(content_area_width(window, cx) - px(PAGE_PADDING))
}

/// The single-key Top-N table. The key cell opens the key in the editor —
/// again not for offline rows, which have no live key to open.
pub(super) fn single_key_table(
    server_state: Entity<ZedisServerState>,
    offline: Rc<Cell<bool>>,
    window: &mut Window,
    cx: &mut gpui::App,
) -> ZedisTextTable {
    let title = |key: &'static str| i18n_memory_analysis(cx, key);
    let columns = vec![
        TextColumn::new(COL_KEY, title(COL_KEY), NAME_COLUMN_MIN_WIDTH)
            .sortable()
            .fills(),
        TextColumn::new(COL_MEMORY, title(COL_MEMORY), MEMORY_KEY_WIDTH).sort_by_cell(KEY_CELL_MEMORY_BYTES),
        TextColumn::new(COL_TTL, title(COL_TTL), TTL_KEY_WIDTH).sort_by_cell(KEY_CELL_TTL_SECS),
        TextColumn::new(COL_KEY_TYPE, title(COL_KEY_TYPE), TYPE_KEY_WIDTH).sortable(),
        TextColumn::new(COL_HEAT, title(COL_HEAT), HEAT_KEY_WIDTH).sort_by_cell(KEY_CELL_HEAT),
    ];
    let tooltip = i18n_common(cx, "open_key_tooltip");
    let jump: CellActionProvider = Rc::new(move |col_ix, cells| {
        let action = || -> Option<CellAction> {
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
        };
        action().into_iter().collect()
    });
    ZedisTextTable::new(columns, i18n_common(cx, "copied_to_clipboard"))
        .copy_tooltip(i18n_common(cx, "copy_cell_tooltip"))
        .cell_action(jump)
        .fitted_to(content_area_width(window, cx) - px(PAGE_PADDING))
}
