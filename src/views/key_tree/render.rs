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

//! Render half of the key tree: scan status line, sticky ancestor
//! overlay, the virtualized tree list and the keyword/filter bar.
//! Split out of `key_tree.rs`.

use super::*;

impl ZedisKeyTree {
    pub(super) fn get_tree_status_view(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let server_state = self.server_state.read(cx);
        // if scanning, return None
        if server_state.scanning() {
            if self.key_tree_list_state.read(cx).delegate().items.is_empty() {
                return Some(
                    div()
                        .m_5()
                        .child(ZedisSkeletonLoading::new().text(i18n_common(cx, "loading")))
                        .into_any_element(),
                );
            }
            return None;
        }
        if !self.state.is_empty && self.state.error.is_none() {
            return None;
        }

        let mut text = SharedString::default();

        if self.state.query_mode == QueryMode::Exact {
            if let Some(value) = server_state.value()
                && value.is_expired()
            {
                text = i18n_key_tree(cx, "key_not_exists");
            }
        } else {
            text = self
                .state
                .error
                .clone()
                .unwrap_or_else(|| i18n_key_tree(cx, "no_keys_found"))
        }
        if text.is_empty() {
            return Some(h_flex().into_any_element());
        }
        Some(
            div()
                .h_flex()
                .w_full()
                .items_center()
                .justify_center()
                .gap_2()
                .pt_5()
                .px_2()
                .child(Icon::new(IconName::Info).text_sm())
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .child(Label::new(text).text_sm().whitespace_normal()),
                )
                .into_any_element(),
        )
    }

    /// Ancestor-folder chain of the first visible row — `(item index, label,
    /// depth)`, shallowest first — for the sticky overlay. Empty when the list
    /// is at the top / unmeasured, or the top row is top-level.
    ///
    /// The first visible index is derived from the scroll offset and a
    /// self-calibrated row pitch (content height ÷ row count — the List
    /// contract guarantees uniform row heights), so no height constant can
    /// drift out of sync with the row styling.
    pub(super) fn sticky_ancestors(&self, cx: &App) -> Vec<(usize, SharedString, usize)> {
        let state = self.key_tree_list_state.read(cx);
        let items = &state.delegate().items;
        if items.is_empty() {
            return Vec::new();
        }
        let handle = state.scroll_handle().base_handle();
        let scrolled = -handle.offset().y.as_f32();
        let viewport_h = handle.bounds().size.height.as_f32();
        if scrolled <= 0.0 || viewport_h <= 0.0 {
            return Vec::new();
        }
        let content_h = handle.max_offset().y.as_f32() + viewport_h;
        let row_h = content_h / items.len() as f32;
        if row_h <= 0.0 {
            return Vec::new();
        }
        let first_visible = ((scrolled / row_h) as usize).min(items.len() - 1);
        // The breadcrumb overlay covers roughly one row at the top, so the row
        // it must describe is the one just below it — anchoring on the covered
        // row makes the sticky switch a row late and linger at boundaries.
        let anchor = (first_visible + 1).min(items.len() - 1);
        let mut chain: Vec<(usize, SharedString, usize)> = Vec::new();
        let mut cur = items[anchor].parent_ix;
        while let Some(ix) = cur {
            chain.push((ix, items[ix].label.clone(), items[ix].depth));
            cur = items[ix].parent_ix;
        }
        chain.reverse();
        // Trim (deepest first) every entry whose subtree end is already on
        // screen below the overlay — a folder that fits in the viewport would
        // otherwise flash a sticky row while scrolling past it. If the deepest
        // survivor extends beyond the viewport, its ancestors necessarily do.
        let viewport_rows = (viewport_h / row_h).ceil() as usize;
        while let Some((_, _, depth)) = chain.last() {
            if subtree_ends_before(items, anchor, *depth, first_visible + viewport_rows) {
                chain.pop();
            } else {
                break;
            }
        }
        chain
    }

    /// Render the tree view or empty state message
    ///
    /// Displays:
    /// - Tree structure with keys and folders (normal state)
    /// - "Key not exists" message (Exact mode with expired key)
    /// - Error or "no keys found" message (empty state)
    pub(super) fn render_tree(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(status_view) = self.get_tree_status_view(cx) {
            return status_view.into_any_element();
        }

        let sticky = self.sticky_ancestors(cx);
        let border_color = cx.theme().border;
        let sticky_bg = cx.theme().sidebar;
        let icon_color = cx.theme().foreground.alpha(0.9);
        let label_color = cx.theme().foreground;

        div()
            .p_1()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .h_full()
            // Positioning context for the sticky-ancestor overlay below.
            .relative()
            // Wheel/trackpad scrolling repaints the list without notifying its
            // entity, so nudge a re-render here to keep the sticky overlay in
            // step (scrollbar drags & keyboard nav land on existing notifies).
            .on_scroll_wheel(cx.listener(|_, _, _, cx| cx.notify()))
            .child(List::new(&self.key_tree_list_state))
            .when(!sticky.is_empty(), |this| {
                // Pinned ancestor path as ONE breadcrumb row — the chain joined
                // with the key separator ("bench:gui:v1:hash") instead of one
                // row per level, so deep nesting doesn't eat the viewport.
                // Clicking jumps to the deepest pinned folder (the subtree the
                // top of the viewport is inside).
                let separator = self.server_state.read(cx).key_separator().to_string();
                let deep_ix = sticky.last().map(|(ix, _, _)| *ix).unwrap_or_default();
                let path: SharedString = sticky
                    .iter()
                    .map(|(_, label, _)| label.as_ref())
                    .collect::<Vec<_>>()
                    .join(separator.as_str())
                    .into();
                this.child(
                    h_flex()
                        .id("ktree-sticky-path")
                        .absolute()
                        // Anchor at the container's very top and re-create the
                        // `p_1` inset as own top padding — anchoring at top(4px)
                        // left a sliver of the scrolled rows peeking above the
                        // overlay (1px flicker while scrolling).
                        .top_0()
                        .left(px(4.))
                        .right(px(4.))
                        .pt(px(12.))
                        .pb_2()
                        .px_2()
                        // Root indent + 3px for the rows' selection border so
                        // the icon lines up with top-level rows.
                        .pl(px(TREE_INDENT_OFFSET + 3.))
                        .gap_2()
                        .items_center()
                        .cursor_pointer()
                        .font_family(get_mono_font_family())
                        .bg(sticky_bg)
                        .border_b_1()
                        .border_color(border_color)
                        .child(Icon::new(IconName::FolderOpen).text_color(icon_color))
                        .child(
                            Label::new(path)
                                .text_color(label_color)
                                .text_ellipsis()
                                .whitespace_nowrap(),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.key_tree_list_state.update(cx, |state, cx| {
                                state.scroll_to_item(IndexPath::new(deep_ix), ScrollStrategy::Top, window, cx);
                            });
                        })),
                )
            })
            .into_any_element()
    }
    /// Render the search/filter input bar with query mode selector
    ///
    /// Features:
    /// - Query mode dropdown (All/Prefix/Exact) with visual indicators
    /// - Search input field with placeholder
    /// - Search button (with loading state during scan)
    /// - Clearable input (X button appears when text entered)
    pub(super) fn render_keyword_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state_clone = self.server_state.clone();
        let server_state = self.server_state.read(cx);
        let scanning = server_state.scanning();
        let readonly = server_state.readonly();
        let server_id = server_state.server_id().to_string();
        let recent_scope = recent_keys_scope(server_id.as_str(), server_state.db());
        let server_id_changed = server_id.as_str() != self.state.server_id.as_str();
        let _ = server_state;
        if server_id_changed {
            self.state.server_id = server_id.clone().into();
            self.keyword_state.update(cx, |state, cx| {
                state.set_value(SharedString::default(), window, cx);
            });
        }
        let query_mode = self.state.query_mode;
        let type_filter = self.server_state.read(cx).type_filter();
        let show_key_tree_ttl = self.server_state.read(cx).show_key_tree_ttl();
        let ttl_filter = self.state.selected_ttl_filter;
        // Always offer the tag filter next to Type/TTL (not buried in ⋯ and
        // not gated on has_any_records — empty servers still show "All keys").
        let tag_filter_active = self.state.selected_tag_filter;

        // Select icon based on query mode
        let icon = match query_mode {
            QueryMode::All => Icon::new(IconName::Asterisk), // * for all keys
            QueryMode::Prefix => Icon::new(CustomIconName::ChevronUp), // ~ for prefix
            QueryMode::Exact => Icon::new(CustomIconName::Equal), // = for exact match
        };
        let server_id_for_favorites: SharedString = server_id.clone().into();
        let query_mode_dropdown = DropdownButton::new("dropdown")
            .button(Button::new("key-tree-query-mode-btn").ghost().px_2().icon(icon))
            .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, window, cx| {
                let favorites = get_favorites_manager()
                    .records(server_id_for_favorites.as_ref())
                    .unwrap_or_default();
                let recent_keys = get_recent_keys_manager()
                    .records(recent_scope.as_str())
                    .unwrap_or_default();
                let server_state_for_history = server_state_clone.clone();
                menu.submenu_with_icon(
                    Some(Icon::new(CustomIconName::Clock3)),
                    i18n_key_tree(cx, "search_history"),
                    window,
                    cx,
                    move |submenu, _window, cx| {
                        let mut submenu = submenu;
                        let keywords = server_state_for_history.read(cx).search_history();
                        if keywords.is_empty() {
                            submenu = submenu.label(i18n_key_tree(cx, "no_search_history"));
                        } else {
                            for keyword in keywords {
                                submenu = submenu
                                    .menu_element(Box::new(KeyTreeAction::Search(keyword.clone())), move |_, _cx| {
                                        Label::new(keyword.clone())
                                    });
                            }
                            submenu = submenu.separator().menu_element_with_icon(
                                CustomIconName::Eraser,
                                Box::new(KeyTreeAction::Clear),
                                move |_, cx| Label::new(i18n_key_tree(cx, "clear_history")),
                            );
                        }
                        submenu
                    },
                )
                .submenu_with_icon(
                    Some(Icon::new(CustomIconName::Activity)),
                    i18n_key_tree(cx, "recent_keys"),
                    window,
                    cx,
                    move |submenu, _window, cx| {
                        let mut submenu = submenu;
                        if recent_keys.is_empty() {
                            submenu = submenu.label(i18n_key_tree(cx, "no_recent_keys"));
                        } else {
                            for key in &recent_keys {
                                let key_clone = key.clone();
                                submenu = submenu.menu_element(
                                    Box::new(KeyTreeAction::SelectRecentKey(key.clone().into())),
                                    move |_, _cx| Label::new(key_clone.clone()).text_ellipsis(),
                                );
                            }
                            submenu = submenu.separator().menu_element_with_icon(
                                CustomIconName::Eraser,
                                Box::new(KeyTreeAction::ClearRecentKeys),
                                move |_, cx| Label::new(i18n_key_tree(cx, "clear_recent_keys")),
                            );
                        }
                        submenu
                    },
                )
                .submenu_with_icon(
                    Some(Icon::new(IconName::Star)),
                    i18n_key_tree(cx, "favorite_keys"),
                    window,
                    cx,
                    move |submenu, _window, cx| {
                        let mut submenu = submenu;
                        if favorites.is_empty() {
                            submenu = submenu.label(i18n_key_tree(cx, "no_favorite_keys"));
                        } else {
                            for key in &favorites {
                                let key_clone = key.clone();
                                submenu = submenu.menu_element(
                                    Box::new(KeyTreeAction::SelectFavoriteKey(key.clone().into())),
                                    move |_, _cx| Label::new(key_clone.clone()).text_ellipsis(),
                                );
                            }
                            submenu = submenu.separator().menu_element_with_icon(
                                CustomIconName::Eraser,
                                Box::new(KeyTreeAction::ClearFavorites),
                                move |_, cx| Label::new(i18n_key_tree(cx, "clear_favorites")),
                            );
                        }
                        submenu
                    },
                )
                .submenu_with_icon(
                    Some(Icon::new(IconName::Asterisk)),
                    i18n_key_tree(cx, "query_mode"),
                    window,
                    cx,
                    move |submenu, _window, _cx| {
                        submenu
                            .menu_element_with_check(query_mode == QueryMode::All, Box::new(QueryMode::All), |_, cx| {
                                Label::new(i18n_key_tree(cx, "query_mode_all"))
                            })
                            .menu_element_with_check(
                                query_mode == QueryMode::Prefix,
                                Box::new(QueryMode::Prefix),
                                |_, cx| Label::new(i18n_key_tree(cx, "query_mode_prefix")),
                            )
                            .menu_element_with_check(
                                query_mode == QueryMode::Exact,
                                Box::new(QueryMode::Exact),
                                |_, cx| Label::new(i18n_key_tree(cx, "query_mode_exact")),
                            )
                    },
                )
                .submenu_with_icon(
                    Some(Icon::new(CustomIconName::Binary)),
                    i18n_key_tree(cx, "type_filter"),
                    window,
                    cx,
                    move |submenu, _window, _cx| {
                        submenu
                            .menu_element_with_check(type_filter.is_none(), Box::new(KeyTypeFilter::All), |_, cx| {
                                Label::new(i18n_key_tree(cx, "type_filter_all"))
                            })
                            .menu_element_with_check(
                                type_filter == Some(KeyType::String),
                                Box::new(KeyTypeFilter::String),
                                |_, _| Label::new("String"),
                            )
                            .menu_element_with_check(
                                type_filter == Some(KeyType::Hash),
                                Box::new(KeyTypeFilter::Hash),
                                |_, _| Label::new("Hash"),
                            )
                            .menu_element_with_check(
                                type_filter == Some(KeyType::List),
                                Box::new(KeyTypeFilter::List),
                                |_, _| Label::new("List"),
                            )
                            .menu_element_with_check(
                                type_filter == Some(KeyType::Set),
                                Box::new(KeyTypeFilter::Set),
                                |_, _| Label::new("Set"),
                            )
                            .menu_element_with_check(
                                type_filter == Some(KeyType::Zset),
                                Box::new(KeyTypeFilter::Zset),
                                |_, _| Label::new("Zset"),
                            )
                            .menu_element_with_check(
                                type_filter == Some(KeyType::Stream),
                                Box::new(KeyTypeFilter::Stream),
                                |_, _| Label::new("Stream"),
                            )
                    },
                )
                // Tag colour filter (local metadata AND). Always visible so
                // it is discoverable next to Type / TTL — not only after the
                // first tag exists and not only under the ⋯ menu.
                .submenu_with_icon(
                    Some(Icon::new(CustomIconName::SwatchBook)),
                    i18n_key_tag(cx, "filter_button_tooltip"),
                    window,
                    cx,
                    move |submenu, _window, _cx| {
                        let mut submenu = submenu.menu_element_with_check(
                            tag_filter_active.is_none(),
                            Box::new(KeyTreeAction::SetTagFilter(SharedString::default())),
                            move |_, cx| Label::new(i18n_key_tag(cx, "filter_all")),
                        );
                        for color in TagColor::ALL {
                            let label_key = match color {
                                TagColor::Red => "color_red",
                                TagColor::Orange => "color_orange",
                                TagColor::Yellow => "color_yellow",
                                TagColor::Green => "color_green",
                                TagColor::Blue => "color_blue",
                                TagColor::Purple => "color_purple",
                            };
                            let color_name: SharedString = color.as_str().into();
                            submenu = submenu.menu_element_with_check(
                                tag_filter_active == Some(color),
                                Box::new(KeyTreeAction::SetTagFilter(color_name)),
                                move |_, cx| Label::new(i18n_key_tag(cx, label_key)),
                            );
                        }
                        submenu
                    },
                )
                // Local TTL-range filter (AND with type + tag). Only when
                // tree TTL chips are enabled — otherwise `key_ttls` is empty
                // and every constrained filter would yield an empty tree.
                .when(show_key_tree_ttl, |this| {
                    this.submenu_with_icon(
                        Some(Icon::new(CustomIconName::Clock3)),
                        i18n_key_tree(cx, "ttl_filter"),
                        window,
                        cx,
                        move |submenu, _window, _cx| {
                            let mut submenu = submenu.menu_element_with_check(
                                matches!(ttl_filter, TtlFilter::All),
                                Box::new(KeyTreeAction::SetTtlFilter(TtlFilter::All.as_str().into())),
                                move |_, cx| Label::new(i18n_key_tree(cx, "ttl_filter_all")),
                            );
                            for (filter, label_key) in [
                                (TtlFilter::NoTtl, "ttl_filter_no_ttl"),
                                (TtlFilter::Expiring, "ttl_filter_expiring"),
                                (TtlFilter::Lt1h, "ttl_filter_lt_1h"),
                                (TtlFilter::Lt1d, "ttl_filter_lt_1d"),
                                (TtlFilter::Lt7d, "ttl_filter_lt_7d"),
                                (TtlFilter::Gte7d, "ttl_filter_gte_7d"),
                            ] {
                                let id: SharedString = filter.as_str().into();
                                submenu = submenu.menu_element_with_check(
                                    ttl_filter == filter,
                                    Box::new(KeyTreeAction::SetTtlFilter(id)),
                                    move |_, cx| Label::new(i18n_key_tree(cx, label_key)),
                                );
                            }
                            submenu
                        },
                    )
                })
            });
        let search_btn = Button::new("key-tree-search-btn")
            .ghost()
            .loading(scanning)
            .disabled(scanning)
            .icon(IconName::Search)
            .on_click(cx.listener(|this, _, _, cx| {
                // Explicit search from the box → always a fresh query.
                this.handle_filter(true, cx);
            }));
        // keyword input
        let keyword_input = Input::new(&self.keyword_state)
            .w_full()
            .flex_1()
            .px_0()
            .prefix(query_mode_dropdown)
            .suffix(search_btn)
            .cleanable(true);
        let enabled_multiple_selection = self.key_tree_list_state.read(cx).delegate().enabled_multiple_selection;
        let refresh_interval_sec = self.state.refresh_interval_sec;

        let more_dropdown = Button::new("key-tree-more-dropdown")
            .outline()
            .icon(Icon::new(IconName::Ellipsis))
            .tooltip(i18n_key_tree(cx, "more_tooltip"))
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, window, cx| {
                menu.menu_element_with_icon(
                    Icon::new(CustomIconName::RotateCw),
                    Box::new(KeyTreeAction::RefreshAll),
                    move |_, cx| {
                        Label::new(format!(
                            "{} ({})",
                            i18n_key_tree(cx, "refresh_keys"),
                            humanize_keystroke("cmd-r")
                        ))
                    },
                )
                // Read-class capabilities: allowed on read-only connections
                // today, but routed through the matrix so a future stricter
                // mode (e.g. export-restricted compliance) has one switch.
                .when(Capability::CollapseTree.allowed(readonly), |this| {
                    this.menu_element_with_icon(
                        Icon::new(CustomIconName::ListChecvronsDownUp),
                        Box::new(KeyTreeAction::CollapseAllKeys),
                        move |_, cx| Label::new(i18n_key_tree(cx, "collapse_keys")),
                    )
                })
                // Multi-select is local UI (enables bulk export); allowed in RO.
                .when(Capability::ToggleMultiSelect.allowed(readonly), |this| {
                    let icon = if enabled_multiple_selection {
                        Icon::new(IconName::Check)
                    } else {
                        Icon::new(CustomIconName::ListCheck)
                    };
                    this.menu_element_with_icon(icon, Box::new(KeyTreeAction::ToggleMultiSelectMode), move |_, cx| {
                        Label::new(i18n_key_tree(cx, "toggle_multi_select_mode"))
                    })
                })
                .submenu_with_icon(
                    Some(Icon::new(CustomIconName::RotateCw)),
                    i18n_key_tree(cx, "auto_refresh"),
                    window,
                    cx,
                    move |submenu, _window, cx| {
                        let mut submenu = submenu;
                        for interval in [0, 1, 5, 10, 30, 60, 120] {
                            let label = if interval == 0 {
                                i18n_key_tree(cx, "disable_auto_refresh")
                            } else {
                                format!("{}s", interval).into()
                            };
                            submenu = submenu.menu_element_with_check(
                                refresh_interval_sec == interval,
                                Box::new(KeyTreeAction::AutoRefresh(interval)),
                                move |_, _cx| Label::new(label.clone()),
                            )
                        }

                        submenu
                    },
                )
                .menu_element_with_icon(
                    Icon::new(CustomIconName::Rss),
                    Box::new(KeyTreeAction::ChangeChannelMode),
                    move |_, cx| Label::new(i18n_key_tree(cx, "pubsub_mode")),
                )
            });

        h_flex()
            .flex_shrink_0()
            .px_2()
            .h(EDITOR_KEY_BAR_HEIGHT)
            .border_b_1()
            .border_color(cx.theme().border)
            .items_center()
            .w_full()
            .gap_x_2()
            .child(keyword_input)
            .child({
                let can_create = Capability::CreateKey.allowed(readonly);
                Button::new("key-tree-add-btn")
                    .disabled(!can_create)
                    .when(!can_create, |this| this.tooltip(i18n_common(cx, "disable_in_readonly")))
                    .when(can_create, |this| {
                        let tooltip = format!(
                            "{} ({})",
                            i18n_key_tree(cx, "add_key_tooltip"),
                            humanize_keystroke("cmd-n")
                        );
                        this.tooltip(tooltip)
                    })
                    .outline()
                    .icon(CustomIconName::FilePlusCorner)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.handle_add_key(window, cx);
                    }))
            })
            .child(more_dropdown)
    }
}
