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

//! Toolbar, group sections, and connection cards for `ZedisServers`.

use super::*;
use gpui::rems;

/// Width of the toolbar search input in `rems` so it scales with UI font size.
const SEARCH_INPUT_WIDTH_REMS: f32 = 13.0;

/// What every card on the page is drawn with, worked out once a render.
pub(super) struct PageLook {
    /// Whether this account may change an entry: a read-only bridge
    /// account may not.
    pub(super) may_change: bool,
    pub(super) sort: ServerSort,
    pub(super) cols: u16,
    pub(super) bg: Hsla,
    pub(super) dark: bool,
    pub(super) locale: String,
    pub(super) subtitle_font: String,
    pub(super) update_tooltip: SharedString,
    pub(super) move_up_tooltip: SharedString,
    pub(super) move_down_tooltip: SharedString,
    pub(super) ungrouped_label: SharedString,
}

impl ZedisServers {
    /// The toolbar: "Connections" and how many, the search box, the
    /// order, and New / Import / Export.
    pub(super) fn toolbar(
        &self,
        all_servers: &[RedisServer],
        sort: ServerSort,
        may_change: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let muted_fg = cx.theme().muted_foreground;
        h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .pt_1()
            .px_1()
            .pb_3()
            // Full-width divider under the toolbar (design).
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .flex_none()
                    .child(
                        Label::new(i18n_servers(cx, "connections"))
                            .font_semibold()
                            .text_color(cx.theme().foreground),
                    )
                    .child(
                        div().px_1p5().rounded_full().bg(cx.theme().muted).child(
                            Label::new(SharedString::from(all_servers.len().to_string()))
                                .text_xs()
                                .text_color(muted_fg),
                        ),
                    ),
            )
            .child(
                Input::new(&self.search_state)
                    .small()
                    .w(rems(SEARCH_INPUT_WIDTH_REMS))
                    .prefix(Icon::new(IconName::Search).text_color(muted_fg)),
            )
            // Order selector — sorts the cards inside each group. Sits with the
            // search box because both narrow/arrange the same list.
            .child(
                Button::new("servers-toolbar-sort")
                    .outline()
                    .small()
                    .icon(IconName::SortAscending)
                    .tooltip(i18n_servers(cx, "sort_tooltip"))
                    .label(i18n_servers(cx, sort.i18n_key()))
                    .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _window, _cx| {
                        let mut menu = menu;
                        for option in ServerSort::ALL {
                            let id: SharedString = option.as_str().into();
                            let label_key = option.i18n_key();
                            menu = menu.menu_element_with_check(
                                sort == option,
                                Box::new(ServersToolbarAction::SetSort(id)),
                                move |_, cx| Label::new(i18n_servers(cx, label_key)),
                            );
                        }
                        menu
                    }),
            )
            // Spacer pushes the action buttons to the right edge while the
            // search box keeps a consistent rem-scaled width.
            .child(div().flex_1())
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .flex_none()
                    .when(may_change, |actions| {
                        actions
                            .child(
                                Button::new(SERVERS_TOOLBAR_ADD_ID)
                                    .primary()
                                    .small()
                                    .icon(IconName::Plus)
                                    .label(i18n_servers(cx, "toolbar_new"))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.add_or_update_server_dialog(
                                            &RedisServer {
                                                port: DEFAULT_REDIS_PORT,
                                                ..Default::default()
                                            },
                                            window,
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                Button::new("servers-toolbar-import")
                                    .outline()
                                    .small()
                                    .icon(IconName::Asterisk)
                                    .label(i18n_servers(cx, "toolbar_import"))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.import_server_dialog(window, cx);
                                    })),
                            )
                    })
                    .child(
                        Button::new("servers-toolbar-export")
                            .outline()
                            .small()
                            .icon(IconName::ExternalLink)
                            .label(i18n_servers(cx, "toolbar_export"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.export_servers_dialog(window, cx);
                            })),
                    ),
            )
    }
}

/// One group's section: its header, which opens and closes it, and the
/// grid of its cards. `first_index` is its first card's place on the
/// page, which the card ids carry.
pub(super) fn group_section(
    look: &PageLook,
    group_label: &Option<String>,
    group_servers: &[RedisServer],
    first_index: usize,
    last_used: &HashMap<String, i64>,
    cx: &Context<ZedisServers>,
) -> AnyElement {
    let &PageLook {
        cols,
        ref ungrouped_label,
        ..
    } = look;
    let group_name = match group_label {
        Some(g) => SharedString::from(g.clone()),
        None => ungrouped_label.clone(),
    };
    let group_count = SharedString::from(group_servers.len().to_string());
    let group_key = group_label.as_deref().unwrap_or("__none__").to_string();
    let is_collapsed = cx
        .global::<ZedisGlobalStore>()
        .read(cx)
        .is_server_group_collapsed(&group_key);
    let toggle_key = group_key.clone();
    let section_id = SharedString::from(format!("servers-group-{group_key}"));
    // Skip building the (listener-heavy) cards entirely when
    // the group is collapsed — but still advance the index
    // counter so element ids stay stable across toggles.
    let cards: Vec<gpui::AnyElement> = if is_collapsed {
        Vec::new()
    } else {
        group_servers
            .iter()
            .enumerate()
            .map(|(in_group_index, server)| {
                server_card(
                    look,
                    group_servers,
                    in_group_index,
                    server,
                    first_index + in_group_index,
                    last_used,
                    cx,
                )
            })
            .collect()
    };
    let chevron = if is_collapsed {
        IconName::ChevronRight
    } else {
        IconName::ChevronDown
    };
    v_flex()
        .id(section_id)
        .gap_2()
        .w_full()
        .child(
            // px_1 matches the `.m_1()` margin every
            // ZedisCard applies, so the group label lines
            // up with the cards' left edge. Whole header
            // row toggles collapse on click.
            h_flex()
                .id(SharedString::from(format!("group-header-{group_key}")))
                .items_center()
                .gap_2()
                .h(rems(1.5))
                .px_1()
                .child(Icon::new(chevron).text_color(cx.theme().muted_foreground))
                .child(Label::new(group_name).text_sm().text_color(cx.theme().muted_foreground))
                // Count as a theme-colored pill badge —
                // same chip language as the sidebar tag
                // chips, adapts to light/dark via theme
                // tokens.
                .child(
                    div().px_1p5().rounded_full().bg(cx.theme().muted).child(
                        Label::new(group_count)
                            .text_xs()
                            .text_color(cx.theme().muted_foreground),
                    ),
                )
                // Divider line trailing the group label out to the
                // right edge (design).
                .child(div().flex_1().h_px().bg(cx.theme().border))
                .on_click(cx.listener(move |_this, _, _window, cx| {
                    let key = toggle_key.clone();
                    update_app_state_and_save(cx, "toggle_server_group_collapsed", move |state, _| {
                        state.toggle_server_group_collapsed(&key);
                    });
                })),
        )
        .when(!is_collapsed, |this| {
            this.child(div().grid().grid_cols(cols).w_full().children(cards))
        })
        .into_any_element()
}

/// One connection's card: its name, address, environment and note, when
/// it was last used, the arrows that reorder it within its group, edit and
/// a "…" menu; a click on it connects it.
fn server_card(
    look: &PageLook,
    group_servers: &[RedisServer],
    in_group_index: usize,
    server: &RedisServer,
    index: usize,
    last_used: &HashMap<String, i64>,
    cx: &Context<ZedisServers>,
) -> AnyElement {
    let &PageLook {
        may_change,
        sort,
        bg,
        dark,
        ref locale,
        ref subtitle_font,
        ref update_tooltip,
        ref move_up_tooltip,
        ref move_down_tooltip,
        ..
    } = look;
    let is_first = in_group_index == 0;
    let is_last = in_group_index + 1 == group_servers.len();
    let single_in_group = group_servers.len() == 1;

    let select_server_id = server.id.clone();
    let update_server = server.clone();
    let more_server_id: SharedString = server.id.clone().into();
    let move_up = in_group_index
        .checked_sub(1)
        .and_then(|above| group_servers.get(above))
        .map(|above| (server.id.clone(), above.id.clone()));
    let move_down = group_servers
        .get(in_group_index + 1)
        .map(|below| (server.id.clone(), below.id.clone()));

    let description = server.description.as_deref().unwrap_or_default();
    let updated_at = if let Some(updated_at) = &server.updated_at {
        updated_at.substring(0, UPDATED_AT_SUBSTRING_LENGTH).to_string()
    } else {
        String::new()
    };
    let updated_relative = server
        .updated_at
        .as_deref()
        .map(|s| format_updated_relative(s, locale))
        .unwrap_or_default();
    let last_used_at = last_used.get(&server.id).copied();
    let title = server.name.clone();
    let tag_label = server.tag_label().unwrap_or_default().to_string();
    let tag_chip = resolve_tag_chip(server.tag_color.as_deref(), dark);
    let subtitle = format!("{}:{}", server.host, server.port);
    let updated_label = if updated_at.is_empty() {
        String::new()
    } else {
        t!("servers.updated_at_label", date = updated_at, locale = locale).to_string()
    };

    // Reorder ↑/↓ stay top-right as always-visible xsmall ghost buttons when
    // Manual sort is active (never hidden behind hover, so keyboard focus
    // reaches them directly with zero layout shift).
    let mut reorder_actions: Vec<Button> = Vec::new();
    if may_change && !single_in_group && sort == ServerSort::Manual {
        reorder_actions.push(
            Button::new(("servers-card-action-up", index))
                .ghost()
                .xsmall()
                .tooltip(move_up_tooltip.clone())
                .icon(IconName::ChevronUp)
                .disabled(is_first)
                .on_click(cx.listener(move |_this, _, _window, cx| {
                    cx.stop_propagation();
                    let Some((id, neighbor)) = move_up.clone() else {
                        return;
                    };
                    cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                        store.update(cx, |state, cx| {
                            state.reorder_server(&id, &neighbor, cx);
                        });
                    });
                })),
        );
        reorder_actions.push(
            Button::new(("servers-card-action-down", index))
                .ghost()
                .xsmall()
                .tooltip(move_down_tooltip.clone())
                .icon(IconName::ChevronDown)
                .disabled(is_last)
                .on_click(cx.listener(move |_this, _, _window, cx| {
                    cx.stop_propagation();
                    let Some((id, neighbor)) = move_down.clone() else {
                        return;
                    };
                    cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                        store.update(cx, |state, cx| {
                            state.reorder_server(&id, &neighbor, cx);
                        });
                    });
                })),
        );
    }

    let (meta, card_actions) = {
        let muted = cx.theme().muted_foreground;
        let (relative, tip) = match last_used_at {
            Some(at) => (
                SharedString::from(format_last_used(at, locale)),
                SharedString::from(format_unix_secs(at).unwrap_or_default()),
            ),
            None => (updated_relative.clone().into(), updated_label.clone().into()),
        };
        let has_time = last_used_at.is_some() || !updated_at.is_empty();
        let edit_server = update_server.clone();
        let more_id = more_server_id.clone();
        let meta = has_time.then(|| {
            div()
                .id(("servers-card-updated", index))
                .child(Label::new(relative.clone()).text_xs().text_color(muted))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        });
        (
            meta,
            h_flex()
                .id(("servers-card-footer-actions", index))
                .items_center()
                .gap_1()
                .on_click(|_, _, cx| cx.stop_propagation())
                .when(may_change, |actions| {
                    actions.child(
                        Button::new(("servers-card-edit", index))
                            .ghost()
                            .xsmall()
                            .tooltip(update_tooltip.clone())
                            .icon(CustomIconName::FilePenLine)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.add_or_update_server_dialog(&edit_server, window, cx);
                            })),
                    )
                })
                .child(
                    Button::new(("servers-card-more", index))
                        .ghost()
                        .xsmall()
                        .tooltip(i18n_key_tree(cx, "more_tooltip"))
                        .icon(IconName::Ellipsis)
                        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _window, _cx| {
                            let clone_id = more_id.clone();
                            let exp_id = more_id.clone();
                            let del_id = more_id.clone();
                            menu.when(may_change, |menu| {
                                menu.menu_element_with_icon(
                                    Icon::new(IconName::Copy),
                                    Box::new(ServersCardAction::Clone(clone_id)),
                                    |_, cx| Label::new(i18n_servers(cx, "clone_tooltip")),
                                )
                            })
                            .menu_element_with_icon(
                                Icon::new(IconName::ExternalLink),
                                Box::new(ServersCardAction::Export(exp_id)),
                                |_, cx| Label::new(i18n_servers(cx, "export_tooltip")),
                            )
                            .when(may_change, |menu| {
                                menu.menu_element_with_icon(
                                    Icon::new(CustomIconName::FileXCorner),
                                    Box::new(ServersCardAction::Delete(del_id)),
                                    |_, cx| Label::new(i18n_servers(cx, "remove_tooltip")),
                                )
                            })
                        }),
                ),
        )
    };

    let handle_select_server = cx.listener(move |_this, _, _, cx| {
        let select_server_id = select_server_id.clone();
        cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
            store.update(cx, |state, cx| {
                let db = state.open_db_for(&select_server_id);
                state.connect_server(select_server_id.clone(), db, cx);
            });
        });
    });

    ZedisCard::new(("servers-card", index))
        .icon(Icon::new(CustomIconName::Database))
        .title(title)
        .subtitle(subtitle)
        .subtitle_font(subtitle_font.clone())
        .tag(tag_label, tag_chip)
        .bg(bg)
        .when(!description.is_empty(), |this| {
            this.description(description.to_string())
        })
        .when(!reorder_actions.is_empty(), |this| this.actions(reorder_actions))
        .when_some(meta, |this, meta| this.meta(meta))
        .trailing(card_actions)
        .on_click(Box::new(handle_select_server))
        .into_any_element()
}
