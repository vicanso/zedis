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

use super::bar_menus::{FilterMenu, TreeMoreMenu};
use super::*;
use gpui::{AnyElement, ClickEvent};

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
        // A server without SCAN can never list keys: the auto-scan after
        // connect is skipped (see `ZedisServerState::select`), so an empty
        // tree here is permanent, not "no keys found". Show the open-by-name
        // banner instead — it clears as soon as an exact lookup lands a row.
        if self.key_tree_list_state.read(cx).delegate().items.is_empty()
            && server_state.command_block(ServerCommand::Scan).is_some()
        {
            let muted = cx.theme().muted_foreground;
            return Some(
                v_flex()
                    .w_full()
                    .gap_2()
                    .pt_5()
                    .px_3()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(Icon::new(IconName::Info).text_sm())
                            .child(
                                Label::new(i18n_features(cx, "key_tree_no_scan_title"))
                                    .text_sm()
                                    .font_semibold()
                                    .whitespace_normal(),
                            ),
                    )
                    .child(
                        Label::new(key_tree_no_scan_body(cx))
                            .text_sm()
                            .text_color(muted)
                            .whitespace_normal(),
                    )
                    .into_any_element(),
            );
        }
        if !self.key_tree_list_state.read(cx).delegate().items.is_empty() {
            return None;
        }

        // No rows and nothing scanning. In Exact mode the lookup is the
        // selected value; otherwise a scan that has finished since the
        // server was selected found nothing (a failed connect finishes none,
        // and says so elsewhere). The mode is the one the query ran in: a
        // regex keyword is scanned whatever the box was left on.
        let text = if server_state.keyword_query_mode() == QueryMode::Exact {
            match server_state.value() {
                Some(value) if value.is_expired() => i18n_key_tree(cx, "key_not_exists"),
                _ => SharedString::default(),
            }
        } else if self.state.last_scan.is_some() {
            i18n_key_tree(cx, "no_keys_found")
        } else {
            SharedString::default()
        };
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
        // Getting here while scanning means the tree has rows to show: the
        // skeleton above only stands in for an empty one. Those rows are the
        // *previous* answer during a refresh (`begin_refresh_scan` keeps them
        // until the first batch replaces them), so say so — floating over the
        // list rather than above it, which would shift every row on ⌘R.
        let scanning = self.server_state.read(cx).scanning();
        let muted = cx.theme().muted_foreground;
        let popover_bg = cx.theme().popover;

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
            .on_scroll(cx.listener(|_, _, _, cx| cx.notify()))
            .child(List::new(&self.key_tree_list_state))
            .when(scanning, |this| {
                this.child(
                    h_flex()
                        .absolute()
                        .bottom_2()
                        .left_0()
                        .right_0()
                        .justify_center()
                        .child(
                            h_flex()
                                .gap_1p5()
                                .items_center()
                                .px_2()
                                .py_0p5()
                                .rounded_full()
                                .border_1()
                                .border_color(border_color)
                                .bg(popover_bg)
                                .child(Spinner::new().with_size(px(12.)).color(muted))
                                .child(Label::new(i18n_common(cx, "loading")).text_xs().text_color(muted)),
                        ),
                )
            })
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
    /// - Regex error line under the bar when the pattern will not compile
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
        let regex_mode = self.state.regex_mode;
        let type_filter = self.server_state.read(cx).type_filter();
        let module_types = self.server_state.read(cx).module_types_seen();
        let show_key_tree_ttl = self.server_state.read(cx).show_key_tree_ttl();
        let ttl_filter = self.state.selected_ttl_filter;
        let sort = self.state.sort;
        // Always offer the tag filter next to Type/TTL (not buried in ⋯ and
        // not gated on whether any tag records exist — empty servers still
        // show "All keys").
        let tag_filter_active = self.state.selected_tag_filter;

        // Select icon based on query mode. In regex mode the mode no longer
        // reaches the server — the scan runs unfiltered — so the box says
        // "regex" instead of pretending to be one of the three.
        let icon = if regex_mode {
            Icon::new(CustomIconName::Regex)
        } else {
            match query_mode {
                QueryMode::All => Icon::new(IconName::Asterisk), // * for all keys
                QueryMode::Prefix => Icon::new(CustomIconName::ChevronUp), // ~ for prefix
                QueryMode::Exact => Icon::new(CustomIconName::Equal), // = for exact match
            }
        };
        let server_id_for_favorites: SharedString = server_id.clone().into();
        let filter_menu = FilterMenu {
            server_state_clone,
            server_id_for_favorites,
            recent_scope,
            query_mode,
            regex_mode,
            type_filter,
            module_types,
            tag_filter_active,
            show_key_tree_ttl,
            ttl_filter,
            sort,
        };
        let query_mode_dropdown = DropdownButton::new("dropdown")
            .button(Button::new("key-tree-query-mode-btn").ghost().px_2().icon(icon))
            .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, window, cx| {
                filter_menu.build(menu, window, cx)
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
        // "Multi-selecting" is the mode *or* any selection a modifier click
        // built without it — the menu entry has to say the same thing the
        // tree is doing.
        let (multi_select_mode, selected_count) = {
            let delegate = self.key_tree_list_state.read(cx).delegate();
            (delegate.enabled_multiple_selection, delegate.selected_items.len())
        };
        let enabled_multiple_selection = multi_select_mode || selected_count > 0;
        let refresh_interval_sec = self.state.refresh_interval_sec;
        let flat_view = self.state.flat_view;
        let regex_mode = self.state.regex_mode;

        let more_menu = TreeMoreMenu {
            readonly,
            enabled_multiple_selection,
            refresh_interval_sec,
            flat_view,
            regex_mode,
        };
        let more_dropdown = Button::new("key-tree-more-dropdown")
            .outline()
            .icon(Icon::new(IconName::Ellipsis))
            .tooltip(i18n_key_tree(cx, "more_tooltip"))
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, window, cx| {
                more_menu.build(menu, window, cx)
            });

        let active_filters = self.active_filters(type_filter, show_key_tree_ttl, ttl_filter, tag_filter_active, cx);

        // A regex that will not compile is reported under the bar, not by
        // emptying the tree: half a pattern is a normal state while typing.
        let regex_error = self.state.regex_error.clone();
        let danger = cx.theme().danger;
        let bar = h_flex()
            .flex_shrink_0()
            .px_2()
            .h(EDITOR_KEY_BAR_HEIGHT)
            .items_center()
            .w_full()
            .gap_x_2()
            .child(keyword_input)
            // How many keys are picked, and the one click that drops them.
            // Without it a selection built with Shift is invisible until a
            // right-click, and there is nothing obvious to undo it with.
            .when(selected_count > 0, |this| {
                this.child(clear_selection_button(selected_count, cx))
            })
            .child(add_key_button(readonly, cx))
            .child(more_dropdown);

        v_flex()
            .flex_shrink_0()
            .w_full()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(bar)
            .children((!active_filters.is_empty()).then(|| {
                h_flex()
                    .w_full()
                    .px_2()
                    .pb_1p5()
                    .gap_1()
                    .flex_wrap()
                    .children(active_filters)
            }))
            .children(regex_error.map(|message| {
                div()
                    .w_full()
                    .px_2()
                    .pb_1()
                    .child(Label::new(message).text_xs().text_color(danger).whitespace_normal())
            }))
    }

    /// What narrows the tree besides the keyword, one chip each, with the
    /// click that takes it off. The filters are picked three levels down
    /// a menu and used to leave no trace outside it: a tree showing 2 of
    /// 25 keys looked like a tree that had lost 23 (#184).
    fn active_filters(
        &self,
        type_filter: Option<KeyType>,
        show_key_tree_ttl: bool,
        ttl_filter: TtlFilter,
        tag_filter_active: Option<TagColor>,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let mut active_filters: Vec<AnyElement> = Vec::new();
        if let Some(key_type) = type_filter {
            let label = format!(
                "{}: {}",
                i18n_key_tree(cx, "type_filter"),
                type_filter_label(key_type, cx)
            );
            active_filters.push(filter_chip(
                "key-tree-filter-type",
                label.into(),
                None,
                cx.listener(|this, _, _, cx| {
                    this.server_state
                        .update(cx, |state, cx| state.set_type_filter(None, cx));
                }),
                cx,
            ));
        }
        // Only where the TTL chips are on: without them the filter is not
        // applied either (`update_key_tree`).
        if show_key_tree_ttl && let Some(label_key) = ttl_filter_label_key(ttl_filter) {
            active_filters.push(filter_chip(
                "key-tree-filter-ttl",
                i18n_key_tree(cx, label_key),
                None,
                cx.listener(|this, _, _, cx| this.set_ttl_filter(TtlFilter::All, cx)),
                cx,
            ));
        }
        if let Some(color) = tag_filter_active {
            active_filters.push(filter_chip(
                "key-tree-filter-tag",
                i18n_key_tag(cx, tag_color_label_key(color)),
                Some(theme_color_for_tag(color, cx)),
                cx.listener(|this, _, _, cx| this.set_tag_filter(None, cx)),
                cx,
            ));
        }
        active_filters
    }
}

/// How many keys are picked, and the one click that drops them.
fn clear_selection_button(selected_count: usize, cx: &Context<ZedisKeyTree>) -> Button {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
    let label = t!("key_tree.selected_count", count = selected_count, locale = &locale).to_string();
    Button::new("key-tree-clear-selection")
        .outline()
        .label(label)
        .icon(IconName::Close)
        .tooltip(i18n_key_tree(cx, "clear_selection"))
        .on_click(cx.listener(|_this, _, window, cx| {
            window.dispatch_action(Box::new(KeyTreeAction::ClearSelection), cx);
        }))
}

/// New key, where the connection may write.
fn add_key_button(readonly: bool, cx: &Context<ZedisKeyTree>) -> Button {
    let can_create = Capability::CreateKey.allowed(readonly);
    Button::new("key-tree-add-btn")
        .disabled(!can_create)
        .when(!can_create, |this| this.tooltip(i18n_common(cx, "disable_in_readonly")))
        .when(can_create, |this| {
            this.tooltip(with_hot_key("new_key", &i18n_key_tree(cx, "add_key_tooltip")))
        })
        .outline()
        .icon(CustomIconName::FilePlusCorner)
        .on_click(cx.listener(|this, _, window, cx| {
            this.handle_add_key(window, cx);
        }))
}

/// One active filter in the strip under the bar: what it is, and a click on
/// it takes it off.
fn filter_chip(
    id: &'static str,
    label: SharedString,
    swatch: Option<Hsla>,
    on_clear: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    Button::new(id)
        .outline()
        .xsmall()
        .tooltip(i18n_key_tree(cx, "remove_filter"))
        .child(
            h_flex()
                .items_center()
                .gap_1()
                .when_some(swatch, |this, color| {
                    this.child(div().flex_none().size(px(8.)).rounded_full().bg(color))
                })
                .child(Label::new(label).text_xs())
                .child(Icon::new(IconName::Close).xsmall()),
        )
        .on_click(on_clear)
        .into_any_element()
}

/// A type filter as the Type menu names it.
fn type_filter_label(key_type: KeyType, cx: &App) -> SharedString {
    match key_type {
        KeyType::String => "String".into(),
        KeyType::Hash => "Hash".into(),
        KeyType::List => "List".into(),
        KeyType::Set => "Set".into(),
        KeyType::Zset => "Zset".into(),
        KeyType::Stream => "Stream".into(),
        KeyType::Json => "JSON".into(),
        KeyType::TimeSeries => i18n_timeseries(cx, "title"),
        KeyType::Vectorset => i18n_vector_set(cx, "title"),
        KeyType::Probabilistic(_) => i18n_key_tree(cx, "type_probabilistic"),
        KeyType::Module(id) => id.name().into(),
        KeyType::Unknown | KeyType::Channel => key_type.as_str().into(),
    }
}

/// The label of a TTL filter that narrows the tree; `None` for "any TTL".
fn ttl_filter_label_key(filter: TtlFilter) -> Option<&'static str> {
    match filter {
        TtlFilter::All => None,
        TtlFilter::NoTtl => Some("ttl_filter_no_ttl"),
        TtlFilter::Expiring => Some("ttl_filter_expiring"),
        TtlFilter::Lt1h => Some("ttl_filter_lt_1h"),
        TtlFilter::Lt1d => Some("ttl_filter_lt_1d"),
        TtlFilter::Lt7d => Some("ttl_filter_lt_7d"),
        TtlFilter::Gte7d => Some("ttl_filter_gte_7d"),
    }
}

/// i18n key per tag colour — the `[key_tag]` section's.
pub(super) fn tag_color_label_key(color: TagColor) -> &'static str {
    match color {
        TagColor::Red => "color_red",
        TagColor::Orange => "color_orange",
        TagColor::Yellow => "color_yellow",
        TagColor::Green => "color_green",
        TagColor::Blue => "color_blue",
        TagColor::Purple => "color_purple",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::states::{KeyTypeFilter, QueryMode};
    use crate::test_support::init_ui;
    use gpui::{AnyWindowHandle, MouseButton, TestAppContext, point, size};
    use gpui_kit::component::Root;
    use gpui_kit::test::{TestEventExt, TestWindowExt};

    type Sent = Rc<RefCell<Vec<String>>>;

    /// The tree inside a view that takes the actions its bar's menus send
    /// on their way down and keeps them: what an item sends is known, and
    /// nothing it would do is done.
    struct TreeHarness {
        tree: Entity<ZedisKeyTree>,
        sent: Sent,
    }

    impl Render for TreeHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let (tree_actions, modes, types) = (self.sent.clone(), self.sent.clone(), self.sent.clone());
            div()
                .size_full()
                .capture_action(move |action: &KeyTreeAction, _, cx| {
                    tree_actions.borrow_mut().push(format!("{action:?}"));
                    cx.stop_propagation();
                })
                .capture_action(move |action: &QueryMode, _, cx| {
                    modes.borrow_mut().push(format!("{action:?}"));
                    cx.stop_propagation();
                })
                .capture_action(move |action: &KeyTypeFilter, _, cx| {
                    types.borrow_mut().push(format!("{action:?}"));
                    cx.stop_propagation();
                })
                .child(self.tree.clone())
        }
    }

    /// A tree on a server the scratch configuration does not know. Selecting
    /// it resolves the TTL column from the settings, where it is on.
    fn open_tree(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<ZedisKeyTree>, Entity<ZedisServerState>, Sent) {
        let server_state = cx.new(|_| ZedisServerState::default());
        server_state.update(cx, |state, cx| state.select("scratch".into(), 0, cx));
        cx.run_until_parked();
        let sent = Sent::default();
        let mut tree = None;
        let handle = cx.open_window(size(px(900.), px(720.)), |window, cx| {
            let view = cx.new(|cx| ZedisKeyTree::new(server_state.clone(), window, cx));
            tree = Some(view.clone());
            let harness = cx.new(|_| TreeHarness {
                tree: view,
                sent: sent.clone(),
            });
            Root::new(harness, window, cx)
        });
        cx.run_until_parked();
        (
            handle.into(),
            tree.expect("the window built the tree"),
            server_state,
            sent,
        )
    }

    fn close_menus(cx: &mut TestAppContext, handle: AnyWindowHandle) {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let empty = point(px(6.), px(714.));
            window.pointer_down(empty, MouseButton::Left, cx);
            window.pointer_up(empty, MouseButton::Left, cx);
        })
        .expect("the window is open");
        cx.run_until_parked();
    }

    /// Open the menu `open` shows, hover its item `item` and say whether a
    /// submenu opened; `None` when the menu has no such item.
    fn hover_item(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        open: &impl Fn(&mut Window, &mut App),
        item: usize,
    ) -> Option<bool> {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            open(window, cx);
        })
        .expect("the window is open");
        cx.run_until_parked();
        let offered = cx
            .update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let offered = window.within("popup-menu").try_find(item).is_some();
                if offered {
                    window.within("popup-menu").hover(item, cx);
                }
                offered
            })
            .expect("the window is open");
        cx.run_until_parked();
        offered.then(|| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.try_find("submenu").is_some()
            })
            .expect("the window is open")
        })
    }

    fn last_sent(sent: &Sent, item: usize) -> String {
        let sent = sent.borrow();
        assert!(sent.len() <= 1, "item {item} sent {sent:?}");
        sent.first().cloned().unwrap_or_else(|| "-".to_string())
    }

    /// What the menu `open` shows offers, top to bottom: each item clicked
    /// in a menu opened afresh — `-` for one that sends nothing, a separator
    /// or a label — and a submenu's items, read the same way, in brackets.
    fn menu_items(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        sent: &Sent,
        open: impl Fn(&mut Window, &mut App),
    ) -> Vec<String> {
        let mut items = Vec::new();
        for item in 0usize..30 {
            sent.borrow_mut().clear();
            let Some(submenu) = hover_item(cx, handle, &open, item) else {
                close_menus(cx, handle);
                return items;
            };
            if !submenu {
                cx.update_window(handle, |_, window, cx| {
                    window.within("popup-menu").click(item, cx);
                })
                .expect("the window is open");
                cx.run_until_parked();
                close_menus(cx, handle);
                items.push(last_sent(sent, item));
                continue;
            }
            close_menus(cx, handle);
            let mut subitems = Vec::new();
            for subitem in 0usize..30 {
                sent.borrow_mut().clear();
                hover_item(cx, handle, &open, item);
                let offered = cx
                    .update_window(handle, |_, window, cx| {
                        let mut submenu = window.within("submenu");
                        let mut nested = submenu.within("popup-menu");
                        let offered = nested.try_find(subitem).is_some();
                        if offered {
                            nested.click(subitem, cx);
                        }
                        offered
                    })
                    .expect("the window is open");
                cx.run_until_parked();
                close_menus(cx, handle);
                if !offered {
                    break;
                }
                subitems.push(last_sent(sent, subitem));
            }
            items.push(format!("[{}]", subitems.join(", ")));
        }
        panic!("the menu kept offering items");
    }

    fn filter_menu(window: &mut Window, cx: &mut App) {
        window.within("dropdown").click("popup", cx);
    }

    fn more_menu(window: &mut Window, cx: &mut App) {
        window.click("key-tree-more-dropdown", cx);
    }

    /// The bar's controls, and both of its menus item by item: the search
    /// menu's lists and filters (the TTL ones only with the TTL column),
    /// and the "…" menu.
    #[gpui::test]
    fn the_bar_offers_its_controls_and_both_menus(cx: &mut TestAppContext) {
        init_ui(cx);
        let (handle, _, _, sent) = open_tree(cx);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            for id in [
                "key-tree-query-mode-btn",
                "key-tree-search-btn",
                "key-tree-add-btn",
                "key-tree-more-dropdown",
            ] {
                assert!(window.try_find(id).is_some(), "{id}");
            }
            assert_eq!(window.find("key-tree-add-btn").disabled(), None);
            assert!(window.try_find("key-tree-clear-selection").is_none());
        })
        .expect("the window is open");

        let sorts: Vec<String> = KeySort::ALL
            .iter()
            .map(|sort| format!("SetSort({:?})", sort.as_str()))
            .collect();
        let tags: Vec<String> = std::iter::once("SetTagFilter(\"\")".to_string())
            .chain(TagColor::ALL.iter().map(|c| format!("SetTagFilter({:?})", c.as_str())))
            .collect();
        let ttls = [
            TtlFilter::All,
            TtlFilter::NoTtl,
            TtlFilter::Expiring,
            TtlFilter::Lt1h,
            TtlFilter::Lt1d,
            TtlFilter::Lt7d,
            TtlFilter::Gte7d,
        ]
        .map(|filter| format!("SetTtlFilter({:?})", filter.as_str()));
        assert_eq!(
            menu_items(cx, handle, &sent, filter_menu),
            [
                "[-]".to_string(),
                "[-]".to_string(),
                "[-]".to_string(),
                "[All, Prefix, Exact]".to_string(),
                "[All, String, Hash, List, Set, Zset, Stream, Json, TimeSeries, Vectorset, Probabilistic]".to_string(),
                format!("[{}]", tags.join(", ")),
                format!("[{}]", ttls.join(", ")),
                format!("[{}]", sorts.join(", ")),
            ]
        );
        assert_eq!(
            menu_items(cx, handle, &sent, more_menu),
            [
                "RefreshAll",
                "CollapseAllKeys",
                "ToggleMultiSelectMode",
                "[AutoRefresh(0), AutoRefresh(1), AutoRefresh(5), AutoRefresh(10), AutoRefresh(30), AutoRefresh(60), AutoRefresh(120)]",
                "ChangeChannelMode",
                "-",
                "ToggleFlatView",
                "ToggleRegexMode",
            ]
        );
    }

    /// Each filter is a chip under the bar, and a click on the chip takes
    /// that filter off; with a regex keyword the query modes are shown and
    /// cannot be picked.
    #[gpui::test]
    fn a_filter_is_a_chip_that_takes_it_off(cx: &mut TestAppContext) {
        init_ui(cx);
        let (handle, tree, server_state, sent) = open_tree(cx);
        // The regex switch is the server state's: the tree reads it back on
        // every rebuild, which the type filter starts.
        server_state.update(cx, |state, cx| {
            state.set_regex_keyword(true, cx);
            state.set_type_filter(Some(KeyType::Hash), cx);
        });
        tree.update(cx, |tree, cx| {
            tree.state.selected_ttl_filter = TtlFilter::Lt1h;
            tree.state.selected_tag_filter = Some(TagColor::Red);
            tree.state.regex_mode = true;
            cx.notify();
        });
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            for id in ["key-tree-filter-type", "key-tree-filter-ttl", "key-tree-filter-tag"] {
                assert!(window.try_find(id).is_some(), "{id}");
                window.click(id, cx);
            }
        })
        .expect("the window is open");
        cx.run_until_parked();
        assert_eq!(server_state.read_with(cx, |state, _| state.type_filter()), None);
        tree.read_with(cx, |tree, _| {
            assert_eq!(tree.state.selected_ttl_filter, TtlFilter::All);
            assert_eq!(tree.state.selected_tag_filter, None);
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            for id in ["key-tree-filter-type", "key-tree-filter-ttl", "key-tree-filter-tag"] {
                assert!(window.try_find(id).is_none(), "{id} is off");
            }
        })
        .expect("the window is open");
        assert_eq!(menu_items(cx, handle, &sent, filter_menu)[3], "[-, -, -]");
    }

    /// A read-only connection cannot add a key; picked keys bring the chip
    /// that drops them; and another server empties the keyword.
    #[gpui::test]
    fn the_bar_follows_access_picks_and_the_server(cx: &mut TestAppContext) {
        init_ui(cx);
        let (handle, tree, server_state, sent) = open_tree(cx);
        server_state.update(cx, |state, cx| state.toggle_readonly(cx));
        tree.update(cx, |tree, cx| {
            tree.key_tree_list_state.update(cx, |list, _| {
                list.delegate_mut().set_pick("a".into(), true, 0);
            });
            cx.notify();
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("key-tree-add-btn").disabled(), Some(true));
            window.click("key-tree-clear-selection", cx);
        })
        .expect("the window is open");
        cx.run_until_parked();
        assert_eq!(*sent.borrow(), ["ClearSelection"]);

        cx.update_window(handle, |_, window, cx| {
            tree.update(cx, |tree, cx| {
                tree.keyword_state
                    .update(cx, |state, cx| state.set_value("user:", window, cx))
            });
        })
        .expect("the window is open");
        server_state.update(cx, |state, cx| state.select("another".into(), 0, cx));
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("the window is open");
        assert_eq!(tree.read_with(cx, |tree, cx| tree.keyword_state.read(cx).value()), "");
    }
}
