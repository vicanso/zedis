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

//! Render half of the RediSearch browser: header/index picker,
//! schema panel, query & options bars, create/add-field panels and
//! the result tables. Split out of `search_manager.rs` to keep the
//! data/ops half readable.

use super::*;

impl ZedisSearchManager {
    pub(super) fn render_header(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let title = i18n_search(cx, "title");
        let index_count_label = if self.indexes.is_empty() {
            SharedString::default()
        } else {
            SharedString::from(format!("({})", self.indexes.len()))
        };
        let current_label = self
            .selected_index
            .clone()
            .unwrap_or_else(|| i18n_search(cx, "pick_index"));
        let indexes = self.indexes.clone();

        h_flex()
            .items_center()
            .justify_between()
            .px_4()
            .h(px(40.))
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("search-back")
                            .ghost()
                            .small()
                            .icon(IconName::ArrowLeft)
                            .tooltip(i18n_common(cx, "back_to_editor"))
                            .on_click(|_, _w, cx| {
                                cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                    store.update(cx, |state, cx| state.go_to_view(ServerView::Editor, cx));
                                });
                            }),
                    )
                    .child(Icon::new(IconName::Search))
                    .child(Label::new(title).text_color(cx.theme().foreground))
                    .child(Label::new(index_count_label).text_color(muted).text_sm()),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        // Index picker — DropdownButton's main button shows
                        // the current selection (or a placeholder), and the
                        // chevron opens the list of indexes from FT._LIST.
                        // Actions carry the index into `indexes` rather
                        // than the name itself so the derived Action enum
                        // stays Copy.
                        DropdownButton::new("search-index-picker")
                            .button(
                                Button::new("search-index-current")
                                    .outline()
                                    .small()
                                    .label(current_label),
                            )
                            .dropdown_menu(move |menu, _w, _cx| {
                                let mut menu = menu;
                                for (idx, name) in indexes.iter().enumerate() {
                                    let label = name.clone();
                                    menu = menu.menu_element(
                                        Box::new(SearchManagerAction::SelectIndex(idx as u32)),
                                        move |_w, _cx| Label::new(label.clone()),
                                    );
                                }
                                menu
                            }),
                    )
                    .child(
                        // + New Index — opens the structured create form.
                        // Always available (also when there are zero
                        // indexes) so the empty-state isn't a dead end.
                        Button::new("search-new-index")
                            .outline()
                            .small()
                            .icon(IconName::Plus)
                            .tooltip(i18n_search(cx, "create_tooltip"))
                            .disabled(self.creating_index)
                            .on_click(cx.listener(|this, _, w, cx| this.open_create_dialog(w, cx))),
                    )
                    .child(
                        Button::new("search-refresh")
                            .outline()
                            .small()
                            .icon(Icon::new(CustomIconName::RotateCw))
                            .tooltip(i18n_search(cx, "refresh_tooltip"))
                            .on_click(cx.listener(|this, _, _w, cx| this.refresh_indexes(cx))),
                    )
                    .child(
                        Button::new("search-run")
                            .small()
                            .primary()
                            .icon(IconName::Search)
                            .label(i18n_search(cx, "run"))
                            .disabled(self.running_query || self.selected_index.is_none())
                            .on_click(cx.listener(|this, _, w, cx| this.run(w, cx))),
                    ),
            )
            .on_action(cx.listener(|this, action: &SearchManagerAction, _w, cx| match action {
                SearchManagerAction::SelectIndex(idx) => {
                    if let Some(name) = this.indexes.get(*idx as usize).cloned() {
                        this.select_index(name, cx);
                    }
                }
                SearchManagerAction::SetReducer(idx) => {
                    let all = ReducerFn::all();
                    if let Some(r) = all.get(*idx as usize) {
                        this.reducer_fn = r.clone();
                        cx.notify();
                    }
                }
            }))
    }

    /// Compact schema panel: one row per indexed field with type chip
    /// + flags. Becomes a "no schema" hint when no index is selected yet.
    pub(super) fn render_schema_panel(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let body = if let Some(info) = &self.index_info {
            let key_type = info.key_type.clone();
            let prefixes = info.prefixes.clone();
            let num_docs = info.num_docs;
            let is_indexing = info.indexing;
            let failures = info.indexing_failures;
            let theme_yellow = cx.theme().yellow;
            let theme_red = cx.theme().red;
            let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(info.fields.len());
            for f in &info.fields {
                rows.push(self.render_schema_row(f.clone(), cx).into_any_element());
            }
            let dropping = self.dropping_index;
            let altering = self.altering_index;
            v_flex()
                .gap_1()
                .p_2()
                .child(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .child(Label::new(i18n_search(cx, "schema_label")).text_sm().text_color(muted))
                        .when(!key_type.is_empty(), |this| {
                            this.child(
                                self.chip(key_type.into(), cx.theme().muted_foreground, cx)
                                    .into_any_element(),
                            )
                        })
                        .when(!prefixes.is_empty(), |this| {
                            // Quote each prefix so the trailing colon
                            // doesn't visually merge with the next label
                            // ("prefix: user: 0 docs" → "prefix: \"user:\"").
                            let joined = prefixes
                                .iter()
                                .map(|s| format!("\"{}\"", s.as_str()))
                                .collect::<Vec<_>>()
                                .join(", ");
                            this.child(
                                Label::new(format!("{}: {joined}", i18n_search(cx, "prefix_label")))
                                    .text_xs()
                                    .text_color(muted),
                            )
                        })
                        .child(
                            Label::new(format!("{num_docs} {}", i18n_search(cx, "docs_unit")))
                                .text_xs()
                                .text_color(muted),
                        )
                        // While RediSearch is backfilling, num_docs lags
                        // reality — surface that state so the user
                        // doesn't think the index is broken.
                        .when(is_indexing, |this| {
                            this.child(
                                self.chip(i18n_search(cx, "indexing_chip"), theme_yellow, cx)
                                    .into_any_element(),
                            )
                        })
                        // hash_indexing_failures is the direct signal
                        // for "keys matched the prefix but were the
                        // wrong storage type". Show count + tooltip.
                        .when(failures > 0, |this| {
                            this.child(
                                self.chip(
                                    SharedString::from(format!("{} {}", failures, i18n_search(cx, "failures_chip"))),
                                    theme_red,
                                    cx,
                                )
                                .into_any_element(),
                            )
                        })
                        // Schema actions pinned to the right: ALTER ADD
                        // (incremental) and DROPINDEX (destructive).
                        // Disabled while the corresponding task is
                        // running so users can't double-click their
                        // way into races.
                        .child(div().flex_1())
                        .child(
                            Button::new("search-alter-add-field")
                                .ghost()
                                .small()
                                .icon(IconName::Plus)
                                .tooltip(i18n_search(cx, "alter_add_tooltip"))
                                .disabled(altering)
                                .on_click(cx.listener(|this, _, w, cx| this.open_add_field_form(w, cx))),
                        )
                        .child(
                            Button::new("search-drop-index")
                                .ghost()
                                .small()
                                .icon(IconName::CircleX)
                                .tooltip(i18n_search(cx, "drop_tooltip"))
                                .disabled(dropping)
                                .on_click(cx.listener(|this, _, w, cx| this.confirm_drop_index(w, cx))),
                        ),
                )
                .child(v_flex().gap_1().children(rows))
                .into_any_element()
        } else if self.loading_info {
            div()
                .p_4()
                .child(Label::new(i18n_common(cx, "loading")).text_color(muted))
                .into_any_element()
        } else {
            div()
                .p_4()
                .child(Label::new(i18n_search(cx, "pick_index")).text_color(muted))
                .into_any_element()
        };

        div()
            .h(px(SCHEMA_PANEL_HEIGHT))
            .w_full()
            .border_b_1()
            .border_color(cx.theme().border)
            .overflow_y_scrollbar()
            .child(body)
    }

    pub(super) fn render_schema_row(&self, field: FieldSchema, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let kind = field.kind();
        let kind_color = match kind {
            FieldKind::Text => cx.theme().blue,
            FieldKind::Numeric => cx.theme().green,
            FieldKind::Tag => cx.theme().yellow,
            FieldKind::Geo | FieldKind::GeoShape => cx.theme().cyan,
            FieldKind::Vector => cx.theme().magenta,
            FieldKind::Unknown(_) => cx.theme().muted_foreground,
        };
        let mut flag_chips: Vec<gpui::AnyElement> = Vec::new();
        if field.sortable {
            flag_chips.push(
                self.chip("SORTABLE".into(), cx.theme().muted_foreground, cx)
                    .into_any_element(),
            );
        }
        if field.no_stem {
            flag_chips.push(
                self.chip("NOSTEM".into(), cx.theme().muted_foreground, cx)
                    .into_any_element(),
            );
        }
        if field.no_index {
            flag_chips.push(
                self.chip("NOINDEX".into(), cx.theme().muted_foreground, cx)
                    .into_any_element(),
            );
        }
        // Attribute text (weight= / sep=) reads as secondary but must stay
        // legible on the dark schema panel — `muted_foreground` was too faint.
        let attr_color = cx.theme().foreground.opacity(0.7);
        if let Some(w) = field.weight {
            flag_chips.push(
                Label::new(format!("weight={w}"))
                    .text_xs()
                    .text_color(attr_color)
                    .into_any_element(),
            );
        }
        if let Some(sep) = field.separator.clone() {
            flag_chips.push(
                Label::new(format!("sep={sep}"))
                    .text_xs()
                    .text_color(attr_color)
                    .into_any_element(),
            );
        }
        h_flex()
            .gap_2()
            .items_center()
            .px_2()
            .child(
                self.chip(field.kind_str.clone().into(), kind_color, cx)
                    .into_any_element(),
            )
            .child(Label::new(field.name.clone()).text_sm())
            .children(flag_chips)
    }

    /// Generic colored "pill" used for type chips and key-type chips.
    /// Kept inline because the rest of the codebase doesn't have a
    /// reusable chip component yet.
    pub(super) fn chip(&self, text: SharedString, color: gpui::Hsla, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let bg = color.opacity(0.18);
        let _ = cx;
        div()
            .px_2()
            .rounded_sm()
            .bg(bg)
            .child(Label::new(text).text_xs().text_color(color))
    }

    pub(super) fn render_query_bar(&self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        // Build field hint chips so users see what they can query.
        let mut hint_chips: Vec<gpui::AnyElement> = Vec::new();
        if let Some(info) = &self.index_info {
            for field in info.fields.iter().take(8) {
                hint_chips.push(
                    Label::new(format!("@{}", field.name))
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .into_any_element(),
                );
            }
        }
        let muted = cx.theme().muted_foreground;
        let mode_search_selected = self.mode == SearchMode::Search;
        let mode_aggregate_selected = self.mode == SearchMode::Aggregate;
        h_flex()
            .w_full()
            .gap_2()
            .px_2()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .items_center()
            .child(
                // Mode toggle: two buttons acting as a radio. The active
                // one switches from outline → primary so the selection is
                // visually obvious without a dedicated SegmentedControl.
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("search-mode-search")
                            .small()
                            .when(mode_search_selected, |b| b.primary())
                            .when(!mode_search_selected, |b| b.outline())
                            .label(i18n_search(cx, "mode_search"))
                            .on_click(cx.listener(|this, _, _w, cx| {
                                this.mode = SearchMode::Search;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("search-mode-aggregate")
                            .small()
                            .when(mode_aggregate_selected, |b| b.primary())
                            .when(!mode_aggregate_selected, |b| b.outline())
                            .label(i18n_search(cx, "mode_aggregate"))
                            .on_click(cx.listener(|this, _, _w, cx| {
                                this.mode = SearchMode::Aggregate;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .gap_1()
                    .child(Input::new(&self.query_input).small())
                    .when(!hint_chips.is_empty(), |this| {
                        this.child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(Label::new(i18n_search(cx, "fields_hint")).text_xs().text_color(muted))
                                .children(hint_chips),
                        )
                    }),
            )
    }

    pub(super) fn render_options_bar(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mode = self.mode;
        // Shared LIMIT row used by both modes.
        let limit_row = h_flex()
            .gap_2()
            .items_center()
            .child(Label::new(i18n_search(cx, "limit_label")).text_xs().text_color(muted))
            .child(Input::new(&self.limit_offset_input).small().w(px(70.0)))
            .child(Label::new("/").text_color(muted))
            .child(Input::new(&self.limit_count_input).small().w(px(70.0)));

        let mode_specific = match mode {
            SearchMode::Search => v_flex()
                .gap_2()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Label::new(i18n_search(cx, "return_label")).text_xs().text_color(muted))
                        .child(Input::new(&self.return_input).small().flex_1()),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            Label::new(i18n_search(cx, "highlight_label"))
                                .text_xs()
                                .text_color(muted),
                        )
                        .child(Input::new(&self.highlight_fields_input).small().flex_1())
                        .child(Label::new(i18n_search(cx, "tags_label")).text_xs().text_color(muted))
                        .child(Input::new(&self.highlight_open_input).small().w(px(72.0)))
                        .child(Input::new(&self.highlight_close_input).small().w(px(72.0))),
                )
                .into_any_element(),
            SearchMode::Aggregate => {
                let reducer_label = i18n_search(cx, "reducer_label");
                let current_reducer_label: SharedString = self.reducer_fn.as_str().to_string().into();
                let arity = self.reducer_fn.arity();
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(Label::new(i18n_search(cx, "groupby_label")).text_xs().text_color(muted))
                            .child(Input::new(&self.groupby_input).small().flex_1()),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(Label::new(reducer_label).text_xs().text_color(muted))
                            .child(
                                DropdownButton::new("search-reducer-picker")
                                    .button(
                                        Button::new("search-reducer-current")
                                            .outline()
                                            .small()
                                            .label(current_reducer_label),
                                    )
                                    .dropdown_menu(move |menu, _w, _cx| {
                                        let mut menu = menu;
                                        for (idx, r) in ReducerFn::all().iter().enumerate() {
                                            let label: SharedString = r.as_str().to_string().into();
                                            menu = menu.menu_element(
                                                Box::new(SearchManagerAction::SetReducer(idx as u32)),
                                                move |_w, _cx| Label::new(label.clone()),
                                            );
                                        }
                                        menu
                                    }),
                            )
                            .when(arity > 0, |this| {
                                this.child(Input::new(&self.reducer_args_input).small().flex_1())
                            })
                            .child(Label::new(i18n_search(cx, "alias_label")).text_xs().text_color(muted))
                            .child(Input::new(&self.reducer_alias_input).small().w(px(120.0))),
                    )
                    .into_any_element()
            }
        };

        v_flex()
            .px_2()
            .py_2()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(limit_row)
            .child(mode_specific)
    }

    /// Full-panel create-index form. Replaces the schema/query/results
    /// stack while open. Mutations route through view methods so all
    /// changes hit the standard `cx.notify` redraw path — no shared
    /// `Rc<RefCell<…>>` plumbing needed.
    pub(super) fn render_create_panel(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let Some(form) = self.create_form.as_ref() else {
            return div().into_any_element();
        };
        let name_input = form.name.clone();
        let prefixes_input = form.prefixes.clone();
        let on_json = form.on_json;
        let creating = self.creating_index;

        let mut field_rows: Vec<gpui::AnyElement> = Vec::with_capacity(form.fields.len());
        let row_count = form.fields.len();
        for f in &form.fields {
            field_rows.push(self.render_create_field_row(f, row_count, cx).into_any_element());
        }

        let error_banner: Option<gpui::AnyElement> = self.error.as_ref().map(|e| {
            div()
                .px_3()
                .py_2()
                .bg(cx.theme().red.opacity(0.15))
                .child(Label::new(e.clone()).text_color(cx.theme().red).text_xs())
                .into_any_element()
        });

        v_flex()
            .gap_3()
            .p_4()
            .w_full()
            .when_some(error_banner, |this, banner| this.child(banner))
            // Index name
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new(i18n_search(cx, "create_name_label"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(Input::new(&name_input).appearance(true)),
            )
            // ON HASH | ON JSON toggle
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new(i18n_search(cx, "create_on_label"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("create-on-hash")
                                    .small()
                                    .when(!on_json, |b| b.primary())
                                    .when(on_json, |b| b.outline())
                                    .label("ON HASH")
                                    .on_click(cx.listener(|this, _, _w, cx| this.set_create_key_type(false, cx))),
                            )
                            .child(
                                Button::new("create-on-json")
                                    .small()
                                    .when(on_json, |b| b.primary())
                                    .when(!on_json, |b| b.outline())
                                    .label("ON JSON")
                                    .on_click(cx.listener(|this, _, _w, cx| this.set_create_key_type(true, cx))),
                            ),
                    ),
            )
            // Prefixes
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new(i18n_search(cx, "create_prefixes_label"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(Input::new(&prefixes_input).appearance(true)),
            )
            // Schema fields
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .justify_between()
                            .child(
                                Label::new(i18n_search(cx, "create_fields_label"))
                                    .text_xs()
                                    .text_color(muted),
                            )
                            .child(
                                Button::new("create-add-field")
                                    .small()
                                    .outline()
                                    .icon(IconName::Plus)
                                    .label(i18n_search(cx, "create_add_field"))
                                    .on_click(cx.listener(|this, _, w, cx| this.add_create_field(w, cx))),
                            ),
                    )
                    .child(v_flex().gap_2().children(field_rows)),
            )
            // Footer buttons
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("create-cancel")
                            .small()
                            .outline()
                            .disabled(creating)
                            .label(i18n_common(cx, "cancel"))
                            .on_click(cx.listener(|this, _, _w, cx| this.close_create_dialog(cx))),
                    )
                    .child(
                        Button::new("create-submit")
                            .small()
                            .primary()
                            .disabled(creating)
                            .label(i18n_search(cx, "create"))
                            .on_click(cx.listener(|this, _, _w, cx| this.submit_create_index(cx))),
                    ),
            )
            .into_any_element()
    }

    /// Minimal one-field form used by `FT.ALTER … SCHEMA ADD`. Mirrors
    /// the layout of a single row from `render_create_field_row` but
    /// in panel form with its own submit/cancel buttons.
    pub(super) fn render_add_field_panel(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let Some(form) = self.add_field_form.as_ref() else {
            return div().into_any_element();
        };
        let index = self.selected_index.clone().unwrap_or_default();
        let altering = self.altering_index;
        let name_input = form.name.clone();
        let current_type = form.field_type.clone();
        let is_text = current_type.as_ref() == "TEXT";
        let sortable = form.sortable;
        let no_stem = form.no_stem;
        let no_index = form.no_index;

        let type_chips = h_flex().gap_1().children(CREATE_FIELD_TYPES.iter().map(|t| {
            let t = *t;
            let selected = current_type.as_ref() == t;
            let prefix: &'static str = match t {
                "TEXT" => "alter-type-text",
                "NUMERIC" => "alter-type-numeric",
                "TAG" => "alter-type-tag",
                "GEO" => "alter-type-geo",
                _ => "alter-type-other",
            };
            Button::new(prefix)
                .small()
                .when(selected, |b| b.primary())
                .when(!selected, |b| b.outline())
                .label(t)
                .on_click(cx.listener(move |this, _, _w, cx| this.set_add_field_type(t, cx)))
                .into_any_element()
        }));

        let error_banner: Option<gpui::AnyElement> = self.error.as_ref().map(|e| {
            div()
                .px_3()
                .py_2()
                .bg(cx.theme().red.opacity(0.15))
                .child(Label::new(e.clone()).text_color(cx.theme().red).text_xs())
                .into_any_element()
        });

        v_flex()
            .gap_3()
            .p_4()
            .w_full()
            .when_some(error_banner, |this, banner| this.child(banner))
            .child(
                Label::new(format!("{}: {}", i18n_search(cx, "alter_target_label"), index))
                    .text_sm()
                    .text_color(muted),
            )
            .child(Label::new(i18n_search(cx, "alter_hint")).text_xs().text_color(muted))
            .child(Input::new(&name_input).appearance(true))
            .child(type_chips)
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("alter-sortable")
                            .small()
                            .when(sortable, |b| b.primary())
                            .when(!sortable, |b| b.outline())
                            .label("SORTABLE")
                            .on_click(
                                cx.listener(|this, _, _w, cx| {
                                    this.toggle_add_field_flag(CreateFieldFlag::Sortable, cx)
                                }),
                            ),
                    )
                    .when(is_text, |this| {
                        this.child(
                            Button::new("alter-nostem")
                                .small()
                                .when(no_stem, |b| b.primary())
                                .when(!no_stem, |b| b.outline())
                                .label("NOSTEM")
                                .on_click(cx.listener(|this, _, _w, cx| {
                                    this.toggle_add_field_flag(CreateFieldFlag::NoStem, cx)
                                })),
                        )
                    })
                    .child(
                        Button::new("alter-noindex")
                            .small()
                            .when(no_index, |b| b.primary())
                            .when(!no_index, |b| b.outline())
                            .label("NOINDEX")
                            .on_click(
                                cx.listener(|this, _, _w, cx| this.toggle_add_field_flag(CreateFieldFlag::NoIndex, cx)),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("alter-cancel")
                            .small()
                            .outline()
                            .disabled(altering)
                            .label(i18n_common(cx, "cancel"))
                            .on_click(cx.listener(|this, _, _w, cx| this.close_add_field_form(cx))),
                    )
                    .child(
                        Button::new("alter-submit")
                            .small()
                            .primary()
                            .disabled(altering)
                            .label(i18n_search(cx, "alter_add"))
                            .on_click(cx.listener(|this, _, _w, cx| this.submit_add_field(cx))),
                    ),
            )
            .into_any_element()
    }

    /// One row of the schema field list inside the create dialog.
    pub(super) fn render_create_field_row(
        &self,
        row: &CreateFieldRow,
        total_rows: usize,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let id = row.id;
        let current_type = row.field_type.clone();
        let is_text = current_type.as_ref() == "TEXT";
        let name_input = row.name.clone();
        let sortable = row.sortable;
        let no_stem = row.no_stem;
        let no_index = row.no_index;
        let muted = cx.theme().muted_foreground;
        let can_remove = total_rows > 1;

        // Type-chip row. Inline buttons feel lighter than a popup menu
        // for a 4-option choice.
        let type_chips = h_flex().gap_1().children(CREATE_FIELD_TYPES.iter().map(|t| {
            let t = *t;
            let selected = current_type.as_ref() == t;
            // ElementId requires the tuple's second element to be a
            // primitive (u32/u64/usize), so we can't compose
            // `(static_prefix, "row_id-TYPE_NAME")`. Use one static
            // prefix per type and key by row id only.
            let prefix: &'static str = match t {
                "TEXT" => "create-type-text",
                "NUMERIC" => "create-type-numeric",
                "TAG" => "create-type-tag",
                "GEO" => "create-type-geo",
                _ => "create-type-other",
            };
            Button::new((prefix, id as u32))
                .small()
                .when(selected, |b| b.primary())
                .when(!selected, |b| b.outline())
                .label(t)
                .on_click(cx.listener(move |this, _, _w, cx| this.set_create_field_type(id, t, cx)))
                .into_any_element()
        }));

        h_flex()
            .gap_2()
            .items_center()
            .p_2()
            .border_1()
            .border_color(cx.theme().border)
            .rounded_sm()
            .child(Input::new(&name_input).appearance(true).flex_1())
            .child(type_chips)
            .child(
                // Toggle chips for the boolean flags. NOSTEM is gated to
                // TEXT (it's a no-op on other types and Redis rejects it).
                h_flex()
                    .gap_1()
                    .child(
                        Button::new(("create-sortable", id as u32))
                            .small()
                            .when(sortable, |b| b.primary())
                            .when(!sortable, |b| b.outline())
                            .label("SORTABLE")
                            .on_click(cx.listener(move |this, _, _w, cx| {
                                this.toggle_create_field_flag(id, CreateFieldFlag::Sortable, cx)
                            })),
                    )
                    .when(is_text, |this| {
                        this.child(
                            Button::new(("create-nostem", id as u32))
                                .small()
                                .when(no_stem, |b| b.primary())
                                .when(!no_stem, |b| b.outline())
                                .label("NOSTEM")
                                .on_click(cx.listener(move |this, _, _w, cx| {
                                    this.toggle_create_field_flag(id, CreateFieldFlag::NoStem, cx)
                                })),
                        )
                    })
                    .child(
                        Button::new(("create-noindex", id as u32))
                            .small()
                            .when(no_index, |b| b.primary())
                            .when(!no_index, |b| b.outline())
                            .label("NOINDEX")
                            .on_click(cx.listener(move |this, _, _w, cx| {
                                this.toggle_create_field_flag(id, CreateFieldFlag::NoIndex, cx)
                            })),
                    ),
            )
            .child(
                Button::new(("create-remove", id as u32))
                    .small()
                    .ghost()
                    .icon(IconName::CircleX)
                    .disabled(!can_remove)
                    .tooltip(i18n_search(cx, "create_remove_field"))
                    .on_click(cx.listener(move |this, _, _w, cx| this.remove_create_field(id, cx))),
            )
            .child(div().w(px(0.0)).child(Label::new("").text_color(muted))) // spacer
    }

    pub(super) fn render_results(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        if let Some(err) = &self.error {
            return div()
                .p_4()
                .child(Label::new(err.clone()).text_color(cx.theme().red))
                .into_any_element();
        }
        if self.running_query && self.last_result.is_none() {
            // Spinner + text, matching the config editor's loading treatment —
            // a bare gray label is easy to miss during a slow FT.SEARCH.
            return div()
                .p_4()
                .child(
                    h_flex()
                        .items_center()
                        .gap_1p5()
                        .child(Spinner::new().with_size(px(14.)).color(muted))
                        .child(Label::new(i18n_common(cx, "loading")).text_color(muted)),
                )
                .into_any_element();
        }
        match &self.last_result {
            Some(LastResult::Search(r)) => self.render_search_result(r.clone(), cx).into_any_element(),
            Some(LastResult::Aggregate(r)) => self.render_aggregate_result(r.clone(), cx).into_any_element(),
            None => div()
                .p_4()
                .child(Label::new(i18n_search(cx, "run_hint")).text_color(muted))
                .into_any_element(),
        }
    }

    pub(super) fn render_search_result(&self, r: SearchResult, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(r.hits.len());
        for hit in &r.hits {
            let id = hit.doc_id.clone();
            let mut field_lines: Vec<gpui::AnyElement> = Vec::new();
            for (k, v) in &hit.fields {
                field_lines.push(
                    h_flex()
                        .gap_2()
                        .child(Label::new(k.clone()).text_xs().text_color(muted))
                        .child(Label::new(v.clone()).text_sm().whitespace_normal())
                        .into_any_element(),
                );
            }
            rows.push(
                v_flex()
                    .gap_1()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(Label::new(id).text_sm().text_color(cx.theme().foreground))
                    .child(v_flex().gap_1().children(field_lines))
                    .into_any_element(),
            );
        }
        v_flex()
            .w_full()
            .child(
                h_flex()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .bg(cx.theme().muted.opacity(0.4))
                    .child(
                        Label::new(format!("{} {}", i18n_search(cx, "total_label"), r.total))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(
                        Label::new(format!("{} {}", i18n_search(cx, "returned_label"), r.hits.len()))
                            .text_xs()
                            .text_color(muted),
                    ),
            )
            .children(rows)
    }

    pub(super) fn render_aggregate_result(&self, r: AggregateResult, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(r.rows.len());
        for row in &r.rows {
            let mut cells: Vec<gpui::AnyElement> = Vec::with_capacity(row.len());
            for (k, v) in row {
                cells.push(
                    h_flex()
                        .gap_1()
                        .child(Label::new(k.clone()).text_xs().text_color(muted))
                        .child(Label::new(v.clone()).text_sm())
                        .into_any_element(),
                );
            }
            rows.push(
                h_flex()
                    .gap_4()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .children(cells)
                    .into_any_element(),
            );
        }
        v_flex()
            .w_full()
            .child(
                h_flex().gap_2().px_3().py_1().bg(cx.theme().muted.opacity(0.4)).child(
                    Label::new(format!("{} {}", i18n_search(cx, "rows_label"), r.rows.len()))
                        .text_xs()
                        .text_color(muted),
                ),
            )
            .children(rows)
    }
}

/// View-private action enum for dropdown items. Variants carry indices
/// (not strings) so the derive macro doesn't need to handle non-Copy
/// payloads — easier and dodges schemars surface-area issues.
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
enum SearchManagerAction {
    SelectIndex(u32),
    SetReducer(u32),
}
