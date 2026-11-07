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

//! Footer toolbar for [`ZedisKvTable`]: add / bulk paste, keyword filter,
//! CSV export, sorted-set score filter, type-native key ops, batch selection
//! actions, and pagination status.

use super::*;

/// Width of the keyword search input field in pixels
const KEYWORD_INPUT_WIDTH: f32 = 200.0;

impl<T: ZedisKvFetcher> ZedisKvTable<T> {
    pub(super) fn render_footer(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text_color = cx.theme().muted_foreground;

        // Determine if operations are allowed based on mode
        let selected_count = self.table_state.read(cx).delegate().selected_count();
        let key_ops = self.key_ops();
        let score_range = self.server_state.read(cx).zset_score_range();
        let can_add = self.mode.contains(KvTableMode::ADD);
        let can_filter = self.mode.contains(KvTableMode::FILTER);

        // Search button with loading state
        let search_btn = Button::new("kv-table-search-btn")
            .ghost()
            .icon(IconName::Search)
            .tooltip(i18n_kv_table(cx, "search_tooltip"))
            .loading(self.loading)
            .disabled(self.loading || !can_filter)
            .on_click(cx.listener(|this, _, _, cx| {
                this.handle_filter(cx);
            }));

        // Completion indicator icon
        let status_icon = if self.done {
            Icon::new(CustomIconName::CircleCheckBig) // All data loaded
        } else {
            Icon::new(CustomIconName::CircleDotDashed) // More data available
        };

        h_flex()
            .flex_none()
            .w_full()
            .p_3()
            // Left side: Add button and search input. It wraps:
            // beside an open entry panel the table is a third of its
            // usual width, and a second line of controls is better
            // than a filter box cut to "Fi".
            .child(
                h_flex()
                    .flex_wrap()
                    .min_w_0()
                    .gap_2()
                    .when(can_add, |this| {
                        this.child(
                            Button::new("add-value-btn")
                                .icon(CustomIconName::FilePlusCorner)
                                .tooltip(i18n_kv_table(cx, "add_value_tooltip"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.handle_add_row(window, cx);
                                })),
                        )
                    })
                    .when(can_filter, |this| {
                        // Its own box, so that the line wraps around
                        // it rather than squeezing it.
                        this.child(
                            div()
                                .flex_none()
                                .w(px(KEYWORD_INPUT_WIDTH))
                                .child(Input::new(&self.keyword_state).suffix(search_btn).cleanable(true)),
                        )
                    })
                    // List and Stream have no server-side scan, so
                    // their filter only sees the loaded rows. Say so
                    // next to the box: a result that looks like a
                    // whole-key search but is not would be worse than
                    // no filter at all.
                    .when(can_filter && self.fetcher.filters_client_side(), |this| {
                        this.child(
                            Label::new(t!(
                                "kv_table.filter_loaded_only",
                                count = self.items_count,
                                locale = cx.global::<ZedisGlobalStore>().read(cx).locale()
                            ))
                            .text_xs()
                            .text_color(text_color),
                        )
                    })
                    .when_some(self.action_button_factory.as_ref(), |this, factory| {
                        this.children(factory(window, cx))
                    })
                    // Export loaded rows to CSV — one button on the
                    // shared table covers every collection type.
                    .when(
                        saves_files() && self.items_count > 0 && Capability::ExportCsv.allowed(self.readonly),
                        |this| {
                            this.child(
                                Button::new("kv-table-export-btn")
                                    .ghost()
                                    .icon(CustomIconName::Download)
                                    .tooltip(i18n_common(cx, "export_csv"))
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.export_csv(cx);
                                    })),
                            )
                        },
                    )
                    // A sorted set's own query: members are ordered
                    // by score, so a score window is what the type is
                    // for — the keyword box can only match names.
                    .when(can_filter && self.fetcher.key_type() == KeyType::Zset, |this| {
                        let active = score_range.is_some();
                        this.child(
                            Button::new("kv-table-score-filter")
                                .ghost()
                                .when(active, |button| button.primary())
                                .icon(CustomIconName::Equal)
                                .tooltip(i18n_zset_editor(cx, "score_filter"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    open_score_filter_dialog(this.server_state.clone(), window, cx);
                                })),
                        )
                    })
                    .when_some(score_range, |this, (min, max)| {
                        this.child(
                            h_flex()
                                .items_center()
                                .gap_1()
                                .child(
                                    Label::new(format!("{min} … {max}"))
                                        .text_xs()
                                        .font_family(get_mono_font_family())
                                        .text_color(text_color),
                                )
                                .child(
                                    Button::new("kv-table-score-filter-clear")
                                        .ghost()
                                        .icon(CustomIconName::X)
                                        .tooltip(i18n_zset_editor(cx, "score_filter_clear"))
                                        .on_click(cx.listener(|this, _, _window, cx| {
                                            this.server_state
                                                .update(cx, |state, cx| state.clear_zset_score_filter(cx));
                                        })),
                                ),
                        )
                    })
                    // Type-native operations (LTRIM / ZINCRBY / …)
                    // behind one menu, so the footer keeps its shape
                    // whether a type has three of them or none.
                    .when(!key_ops.is_empty(), |this| {
                        this.child(
                            Button::new("kv-table-key-ops")
                                .ghost()
                                .icon(CustomIconName::Zap)
                                .tooltip(i18n_key_ops(cx, "menu_tooltip"))
                                .dropdown_menu(move |menu, _window, _cx| {
                                    let mut menu = menu;
                                    for action in key_ops {
                                        let label_key = key_op_title_key(*action);
                                        menu = menu.menu_element(Box::new(*action), move |_, cx| {
                                            Label::new(i18n_key_ops(cx, label_key))
                                        });
                                    }
                                    menu
                                }),
                        )
                    })
                    // The selection chip only exists while something
                    // is ticked, so the toolbar keeps its usual shape
                    // until there is a batch to act on.
                    .when(selected_count > 0, |this| {
                        this.child(
                            h_flex()
                                .items_center()
                                .gap_1()
                                .child(
                                    Label::new(t!(
                                        "kv_table.selected_count",
                                        count = selected_count,
                                        locale = cx.global::<ZedisGlobalStore>().read(cx).locale()
                                    ))
                                    .text_xs()
                                    .text_color(text_color),
                                )
                                .child(
                                    Button::new("kv-table-remove-selected")
                                        .danger()
                                        .icon(CustomIconName::FileXCorner)
                                        .tooltip(i18n_kv_table(cx, "remove_selected_tooltip"))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.handle_remove_selected(window, cx);
                                        })),
                                )
                                .child(
                                    Button::new("kv-table-clear-selection")
                                        .ghost()
                                        .icon(CustomIconName::X)
                                        .tooltip(i18n_kv_table(cx, "clear_selection_tooltip"))
                                        .on_click(cx.listener(|this, _, _window, cx| {
                                            this.table_state
                                                .update(cx, |state, _| state.delegate_mut().clear_selection());
                                            cx.notify();
                                        })),
                                ),
                        )
                    })
                    // Status icon and count, at the right end of
                    // whichever line they land on: part of the
                    // wrapping group, or a narrow footer drew them
                    // over the end of the filter box.
                    .child(
                        h_flex()
                            .flex_none()
                            .ml_auto()
                            .items_center()
                            .gap_2()
                            .child(status_icon.text_color(text_color))
                            .child(
                                Label::new(format!("{} / {}", self.items_count, self.total_count))
                                    .text_sm()
                                    .text_color(text_color),
                            ),
                    )
                    .flex_1(),
            )
    }
}
