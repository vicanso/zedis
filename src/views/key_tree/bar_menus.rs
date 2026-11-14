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

//! The keyword bar's two menus: the one behind the search box's mode
//! button — the lists that lead back to a key, and every filter — and its
//! "…".

use super::*;
use crate::states::ModuleTypeId;
use gpui_kit::component::menu::PopupMenu;

/// The search box's menu, decided when the bar is drawn. The lists are
/// read when it opens.
pub(super) struct FilterMenu {
    pub(super) server_state_clone: Entity<ZedisServerState>,
    pub(super) server_id_for_favorites: SharedString,
    pub(super) recent_scope: String,
    pub(super) query_mode: QueryMode,
    pub(super) regex_mode: bool,
    pub(super) type_filter: Option<KeyType>,
    pub(super) module_types: Vec<ModuleTypeId>,
    pub(super) tag_filter_active: Option<TagColor>,
    pub(super) show_key_tree_ttl: bool,
    pub(super) ttl_filter: TtlFilter,
    pub(super) sort: KeySort,
}

impl FilterMenu {
    /// The menu, top to bottom.
    pub(super) fn build(&self, menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
        let menu = self.lists(menu, window, cx);
        let menu = self.query_modes(menu, window, cx);
        let menu = self.type_filter(menu, window, cx);
        let menu = self.tag_filter(menu, window, cx);
        let menu = self.ttl_filter(menu, window, cx);
        self.sort(menu, window, cx)
    }

    /// The way back to a key: what was searched for, the keys opened lately, the starred ones.
    fn lists(&self, menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
        let Self {
            server_state_clone,
            server_id_for_favorites,
            recent_scope,
            ..
        } = self;
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
    }

    /// All, prefix or exact — none of them while the keyword is a regex.
    fn query_modes(&self, menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
        let &Self {
            query_mode, regex_mode, ..
        } = self;
        menu.submenu_with_icon(
            Some(Icon::new(IconName::Asterisk)),
            i18n_key_tree(cx, "query_mode"),
            window,
            cx,
            move |submenu, _window, cx| {
                // A regex keyword takes the mode's place, so the
                // three are shown and cannot be picked while it is
                // on: choosing one changed nothing on screen and
                // decided, unseen, what the next search sent.
                [
                    (QueryMode::All, "query_mode_all"),
                    (QueryMode::Prefix, "query_mode_prefix"),
                    (QueryMode::Exact, "query_mode_exact"),
                ]
                .into_iter()
                .fold(submenu, |submenu, (mode, label)| {
                    submenu.menu_with_check_and_disabled(
                        i18n_key_tree(cx, label),
                        query_mode == mode,
                        Box::new(mode),
                        regex_mode,
                    )
                })
            },
        )
    }

    /// One type, or every one; a module type only where the loaded keys have it.
    fn type_filter(&self, menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
        let &Self {
            type_filter,
            ref module_types,
            ..
        } = self;
        menu.submenu_with_icon(
            Some(Icon::new(CustomIconName::Binary)),
            i18n_key_tree(cx, "type_filter"),
            window,
            cx,
            {
                let module_types = module_types.clone();
                move |submenu, _window, _cx| {
                    let submenu = submenu
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
                        // Module types: no `SCAN … TYPE` narrowing exists
                        // for these — the client-side post-filter over the
                        // resolved types does the work alone.
                        .menu_element_with_check(
                            type_filter == Some(KeyType::Json),
                            Box::new(KeyTypeFilter::Json),
                            |_, _| Label::new("JSON"),
                        )
                        .menu_element_with_check(
                            type_filter == Some(KeyType::TimeSeries),
                            Box::new(KeyTypeFilter::TimeSeries),
                            |_, cx| Label::new(i18n_timeseries(cx, "title")),
                        )
                        .menu_element_with_check(
                            type_filter == Some(KeyType::Vectorset),
                            Box::new(KeyTypeFilter::Vectorset),
                            |_, cx| Label::new(i18n_vector_set(cx, "title")),
                        )
                        .menu_element_with_check(
                            matches!(type_filter, Some(KeyType::Probabilistic(_))),
                            Box::new(KeyTypeFilter::Probabilistic),
                            |_, cx| Label::new(i18n_key_tree(cx, "type_probabilistic")),
                        );
                    // Module types the loaded keys have, by their raw
                    // TYPE name — there is nothing to translate.
                    let mut submenu = submenu;
                    for id in module_types.iter().copied() {
                        submenu = submenu.menu_element_with_check(
                            type_filter == Some(KeyType::Module(id)),
                            Box::new(KeyTypeFilter::Module(id.raw())),
                            move |_, _| Label::new(id.name()),
                        );
                    }
                    submenu
                }
            },
        )
    }

    /// One tag colour, or every key.
    fn tag_filter(&self, menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
        let &Self { tag_filter_active, .. } = self;
        menu
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
    }

    /// A TTL range, where the TTL column is on.
    fn ttl_filter(&self, menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
        let &Self {
            show_key_tree_ttl,
            ttl_filter,
            ..
        } = self;
        menu
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
    }

    /// The order of a folder's children.
    fn sort(&self, menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
        let &Self {
            show_key_tree_ttl,
            sort,
            ..
        } = self;
        menu
            // Sibling order. TTL orders read the same cache the TTL
            // chips do, so they need it enabled too.
            .submenu_with_icon(
                Some(Icon::new(CustomIconName::ListChecvronsDownUp)),
                i18n_key_tree(cx, "sort"),
                window,
                cx,
                move |submenu, _window, _cx| {
                    let mut submenu = submenu;
                    for option in KeySort::ALL {
                        if !show_key_tree_ttl && matches!(option, KeySort::TtlAsc | KeySort::TtlDesc) {
                            continue;
                        }
                        let id: SharedString = option.as_str().into();
                        let label_key = option.i18n_key();
                        submenu = submenu.menu_element_with_check(
                            sort == option,
                            Box::new(KeyTreeAction::SetSort(id)),
                            move |_, cx| Label::new(i18n_key_tree(cx, label_key)),
                        );
                    }
                    submenu
                },
            )
    }
}

/// The bar's "…" menu, decided when the bar is drawn.
pub(super) struct TreeMoreMenu {
    pub(super) readonly: bool,
    pub(super) enabled_multiple_selection: bool,
    pub(super) refresh_interval_sec: u32,
    pub(super) flat_view: bool,
    pub(super) regex_mode: bool,
}

impl TreeMoreMenu {
    /// The menu, top to bottom: the tree's own commands, then how it is
    /// read — flat, by regex.
    pub(super) fn build(&self, menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
        let &Self {
            readonly,
            enabled_multiple_selection,
            refresh_interval_sec,
            flat_view,
            regex_mode,
            ..
        } = self;
        menu.menu_element_with_icon(
            Icon::new(CustomIconName::RotateCw),
            Box::new(KeyTreeAction::RefreshAll),
            move |_, cx| Label::new(with_hot_key("reload_keys", &i18n_key_tree(cx, "refresh_keys"))),
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
        .when(
            Capability::DeleteKeys.allowed(readonly) || Capability::SetTtl.allowed(readonly),
            |this| {
                this.menu_element_with_icon(
                    Icon::new(CustomIconName::Eraser),
                    Box::new(KeyTreeAction::PatternBulkOps),
                    move |_, cx| Label::new(i18n_key_tree(cx, "pattern_bulk_ops")),
                )
            },
        )
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
        .separator()
        // A check replaces the icon while the mode is on — the same
        // shape the multi-select entry above uses, since the menu API
        // carries an icon *or* a check, not both.
        .menu_element_with_icon(
            if flat_view {
                Icon::new(IconName::Check)
            } else {
                Icon::new(CustomIconName::List)
            },
            Box::new(KeyTreeAction::ToggleFlatView),
            move |_, cx| Label::new(i18n_key_tree(cx, "flat_view")),
        )
        .menu_element_with_icon(
            if regex_mode {
                Icon::new(IconName::Check)
            } else {
                Icon::new(CustomIconName::Regex)
            },
            Box::new(KeyTreeAction::ToggleRegexMode),
            move |_, cx| Label::new(i18n_key_tree(cx, "regex_mode")),
        )
    }
}
