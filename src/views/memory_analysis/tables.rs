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

//! Table delegates for the prefix-group and single-key result
//! tables, plus the shared copy/jump cell renderer.

use super::*;

/// Hover action for a cell: (tooltip, click handler). Used by the prefix /
/// key columns to jump into the key tree / editor.
type JumpAction = std::sync::Arc<dyn Fn(&mut gpui::App)>;

fn render_copy_cell(
    row_ix: usize,
    col_ix: usize,
    value: SharedString,
    column: &Column,
    id_prefix: &'static str,
    copied_message: SharedString,
    jump: Option<(SharedString, JumpAction)>,
) -> impl IntoElement {
    // This is the only necessary string allocation.
    // It serves as a globally unique Group identifier for the hover state.
    let group_name: SharedString = format!("{id_prefix}-td-{row_ix}-{col_ix}").into();

    h_flex()
        .size_full()
        .when_some(column.paddings, |this, paddings| this.paddings(paddings))
        .group(group_name.clone())
        .overflow_hidden()
        .child(
            Label::new(value.clone())
                .text_align(column.align)
                .text_ellipsis()
                .flex_1()
                // Essential for text_ellipsis to work inside a flex container
                .min_w_0(),
        )
        .when_some(jump, |this, (tooltip, action)| {
            this.child(
                div()
                    .id((group_name.clone(), 2_usize))
                    .invisible()
                    .group_hover(group_name.clone(), |style| style.visible())
                    .flex_none()
                    .on_click(|_, _, cx: &mut gpui::App| cx.stop_propagation())
                    .child(
                        Button::new((group_name.clone(), 3_usize))
                            .ghost()
                            .icon(IconName::Search)
                            .tooltip(tooltip)
                            .on_click(move |_, _, cx: &mut gpui::App| action(cx)),
                    ),
            )
        })
        .child(
            div()
                // Clever trick: Reuse the group_name (SharedString) combined with a usize index.
                // This perfectly matches GPUI's `impl From<(SharedString, usize)> for ElementId`.
                // It requires zero extra heap allocation and guarantees absolute uniqueness!
                .id((group_name.clone(), 0_usize))
                .invisible()
                .group_hover(group_name.clone(), |style| style.visible())
                .flex_none()
                // Stop event propagation to prevent triggering row selection events
                .on_click(|_, _, cx: &mut gpui::App| cx.stop_propagation())
                .child(
                    // Reuse the same group_name, but with index 1 to distinguish the Button's ID
                    Button::new((group_name.clone(), 1_usize))
                        .ghost()
                        .icon(IconName::Copy)
                        .on_click(move |_, window, cx: &mut gpui::App| {
                            cx.write_to_clipboard(ClipboardItem::new_string(value.to_string()));
                            window.push_notification(Notification::info(copied_message.clone()), cx);
                        }),
                ),
        )
}

const TYPE_KEY_WIDTH: f32 = 140.;
const MEMORY_KEY_WIDTH: f32 = 200.;
const COUNT_KEY_WIDTH: f32 = 150.;
const TTL_KEY_WIDTH: f32 = 120.;
const HEAT_KEY_WIDTH: f32 = 130.;
const PERM_KEY_WIDTH: f32 = 130.;

// ─── Prefix table delegate ───────────────────────────────────────────────────

pub(super) struct PrefixTableDelegate {
    pub(super) rows: Vec<PrefixRow>,
    /// Rows came from an offline RDB file — the jump-to-key-tree action
    /// would target the live connection, so it is suppressed.
    pub(super) offline: bool,
    columns: Vec<Column>,
    column_keys: Vec<&'static str>,
    /// For the prefix column's search-in-key-tree jump.
    server_state: Entity<ZedisServerState>,
}

impl PrefixTableDelegate {
    pub(super) fn new(
        rows: Vec<PrefixRow>,
        server_state: Entity<ZedisServerState>,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> Self {
        let content_width = content_area_width(window, cx).as_f32();

        // Use padding offsets to prevent horizontal scrollbars
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

        let column_keys = vec![
            COL_PREFIX,
            COL_KEY_COUNT,
            COL_MEMORY,
            COL_AVG_TTL,
            COL_PERM_COUNT,
            COL_TYPES,
        ];
        let widths = [
            prefix_w,
            COUNT_KEY_WIDTH,
            MEMORY_KEY_WIDTH,
            TTL_KEY_WIDTH,
            PERM_KEY_WIDTH,
            TYPE_KEY_WIDTH,
        ];

        let columns = column_keys
            .clone()
            .into_iter()
            .zip(widths)
            .map(|(key, w)| {
                let mut c = Column::new(key, SharedString::default()).width(w).sortable();
                c.paddings = make_paddings();
                c
            })
            .collect();

        Self {
            rows,
            offline: false,
            columns,
            column_keys,
            server_state,
        }
    }
}

impl TableDelegate for PrefixTableDelegate {
    fn columns_count(&self, _cx: &gpui::App) -> usize {
        self.columns.len()
    }
    fn rows_count(&self, _cx: &gpui::App) -> usize {
        self.rows.len()
    }
    fn column(&self, ix: usize, _cx: &gpui::App) -> Column {
        self.columns[ix].clone()
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        _: &mut gpui::Context<TableState<Self>>,
    ) {
        let key = self.columns[col_ix].key.as_ref();
        self.rows.sort_by(|a, b| {
            let ord = match key {
                COL_PREFIX => a.prefix.cmp(&b.prefix),
                COL_KEY_COUNT => a.key_count.cmp(&b.key_count),
                COL_MEMORY => a.memory_bytes.cmp(&b.memory_bytes),
                COL_AVG_TTL => a
                    .avg_ttl_secs
                    .partial_cmp(&b.avg_ttl_secs)
                    .unwrap_or(std::cmp::Ordering::Equal),
                COL_PERM_COUNT => a.perm_count.cmp(&b.perm_count),
                COL_TYPES => a.types.cmp(&b.types),
                _ => std::cmp::Ordering::Equal,
            };
            if matches!(sort, ColumnSort::Ascending) {
                ord
            } else {
                ord.reverse()
            }
        });
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut gpui::Context<TableState<Self>>,
    ) -> impl IntoElement {
        let col = &self.columns[col_ix];
        // h_flex (items_center) matches render_td, so header text is
        // vertically centered like the cells.
        h_flex()
            .size_full()
            .when_some(col.paddings, |this, p| this.paddings(p))
            .child(
                Label::new(i18n_memory_analysis(cx, self.column_keys[col_ix]))
                    .text_align(col.align)
                    .text_color(cx.theme().muted_foreground)
                    .text_sm()
                    .flex_1(),
            )
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut gpui::Context<TableState<Self>>,
    ) -> impl IntoElement {
        let col = &self.columns[col_ix];
        let value: SharedString = self
            .rows
            .get(row_ix)
            .map(|r| match col_ix {
                0 => r.prefix.clone(),
                1 => r.display_key_count.clone(),
                2 => r.memory.clone(),
                3 => r.avg_ttl.clone(),
                4 => r.perm_display.clone(),
                5 => r.types.clone(),
                _ => "--".into(),
            })
            .unwrap_or_else(|| "--".into());

        // Prefix column: hover action jumps to the key tree filtered by
        // this prefix. Offline (RDB file) rows have no live keys to jump to.
        let jump = if col_ix == 0 && !self.offline {
            self.rows.get(row_ix).map(|r| {
                let prefix = r.prefix.clone();
                let server_state = self.server_state.clone();
                (
                    i18n_common(cx, "search_prefix_tooltip"),
                    std::sync::Arc::new(move |cx: &mut gpui::App| {
                        search_keys_in_tree(&server_state, prefix.clone(), cx);
                    }) as JumpAction,
                )
            })
        } else {
            None
        };

        // Uses our highly optimized render_copy_cell function
        render_copy_cell(
            row_ix,
            col_ix,
            value,
            col,
            "prefix",
            i18n_common(cx, "copied_to_clipboard"),
            jump,
        )
    }

    fn has_more(&self, _cx: &gpui::App) -> bool {
        false
    }
    fn load_more_threshold(&self) -> usize {
        0
    }
    fn load_more(&mut self, _: &mut Window, _: &mut gpui::Context<TableState<Self>>) {}
}

// ─── Single-key table delegate ───────────────────────────────────────────────

pub(super) struct SingleKeyTableDelegate {
    pub(super) rows: Vec<SingleKeyRow>,
    /// Rows came from an offline RDB file — the open-in-editor action
    /// would target the live connection, so it is suppressed.
    pub(super) offline: bool,
    columns: Vec<Column>,
    column_keys: Vec<&'static str>,
    /// For the key column's open-in-editor jump.
    server_state: Entity<ZedisServerState>,
}

impl SingleKeyTableDelegate {
    pub(super) fn new(
        rows: Vec<SingleKeyRow>,
        server_state: Entity<ZedisServerState>,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> Self {
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

        let column_keys = vec![COL_KEY, COL_MEMORY, COL_TTL, COL_KEY_TYPE, COL_HEAT];
        let widths = [key_w, MEMORY_KEY_WIDTH, TTL_KEY_WIDTH, TYPE_KEY_WIDTH, HEAT_KEY_WIDTH];

        let columns = column_keys
            .clone()
            .into_iter()
            .zip(widths)
            .map(|(key, w)| {
                let mut c = Column::new(key, SharedString::default()).width(w).sortable();

                c.paddings = make_paddings();
                c
            })
            .collect();

        Self {
            rows,
            offline: false,
            columns,
            column_keys,
            server_state,
        }
    }
}

impl TableDelegate for SingleKeyTableDelegate {
    fn columns_count(&self, _cx: &gpui::App) -> usize {
        self.columns.len()
    }
    fn rows_count(&self, _cx: &gpui::App) -> usize {
        self.rows.len()
    }
    fn column(&self, ix: usize, _cx: &gpui::App) -> Column {
        self.columns[ix].clone()
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        _: &mut gpui::Context<TableState<Self>>,
    ) {
        let key = self.columns[col_ix].key.as_ref();
        self.rows.sort_by(|a, b| {
            let ord = match key {
                COL_KEY => a.key.cmp(&b.key),
                COL_MEMORY => a.memory_bytes.cmp(&b.memory_bytes),
                COL_TTL => a.ttl_secs.cmp(&b.ttl_secs),
                COL_KEY_TYPE => a.key_type.cmp(&b.key_type),
                COL_HEAT => heat_sort_key(a.heat).cmp(&heat_sort_key(b.heat)),
                _ => std::cmp::Ordering::Equal,
            };
            if matches!(sort, ColumnSort::Ascending) {
                ord
            } else {
                ord.reverse()
            }
        });
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut gpui::Context<TableState<Self>>,
    ) -> impl IntoElement {
        let col = &self.columns[col_ix];
        // h_flex (items_center) matches render_td, so header text is
        // vertically centered like the cells.
        h_flex()
            .size_full()
            .when_some(col.paddings, |this, p| this.paddings(p))
            .child(
                Label::new(i18n_memory_analysis(cx, self.column_keys[col_ix]))
                    .text_align(col.align)
                    .text_color(cx.theme().muted_foreground)
                    .text_sm()
                    .flex_1(),
            )
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut gpui::Context<TableState<Self>>,
    ) -> impl IntoElement {
        let col = &self.columns[col_ix];
        let value: SharedString = self
            .rows
            .get(row_ix)
            .map(|r| match col_ix {
                0 => r.key.clone(),
                1 => r.memory.clone(),
                2 => r.ttl.clone(),
                3 => r.key_type.clone(),
                4 => r.heat_display.clone(),
                _ => "--".into(),
            })
            .unwrap_or_else(|| "--".into());

        // Key column: hover action opens the key in the editor. Offline
        // (RDB file) rows have no live key to open.
        let jump = if col_ix == 0 && !self.offline {
            self.rows.get(row_ix).map(|r| {
                let key = r.key.clone();
                let server_state = self.server_state.clone();
                (
                    i18n_common(cx, "open_key_tooltip"),
                    std::sync::Arc::new(move |cx: &mut gpui::App| {
                        open_key_in_editor(&server_state, key.clone(), cx);
                    }) as JumpAction,
                )
            })
        } else {
            None
        };
        render_copy_cell(
            row_ix,
            col_ix,
            value,
            col,
            "singlekey",
            i18n_common(cx, "copied_to_clipboard"),
            jump,
        )
    }

    fn has_more(&self, _cx: &gpui::App) -> bool {
        false
    }
    fn load_more_threshold(&self) -> usize {
        0
    }
    fn load_more(&mut self, _: &mut Window, _: &mut gpui::Context<TableState<Self>>) {}
}

// ─── Accumulator ─────────────────────────────────────────────────────────────
