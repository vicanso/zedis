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

use crate::{
    assets::CustomIconName,
    connection::get_servers,
    helpers::resolve_tag_color,
    states::{GlobalEvent, Route, ServerView, ZedisGlobalStore, i18n_servers, update_app_state_and_save},
};
use gpui::{Context, Hsla, SharedString, Subscription, Window, div, prelude::*, px, rgb};
use gpui_component::scroll::ScrollableElement;
use gpui_component::tooltip::Tooltip;
use gpui_component::{
    ActiveTheme, Icon, IconName, StyledExt,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    list::ListItem,
    v_flex,
};
use tracing::info;

/// Internal state for sidebar component
///
/// Caches server list bucketed by group, so the render pass only has
/// to read collapse state and lay out rows. The home row is rendered
/// separately and is not stored here.
#[derive(Default)]
struct SidebarState {
    /// Server sections in canonical sort order — same partitioning
    /// the servers page uses (`servers.rs`'s group loop), so the
    /// collapse state stored in `ZedisGlobalStore::collapsed_server_groups`
    /// applies consistently across both views.
    sections: Vec<SidebarSection>,

    /// Currently selected server ID (empty string means home page)
    server_id: SharedString,
}

#[derive(Clone, Default)]
struct SidebarSection {
    /// Stable collapse key — the trimmed group name, or `"__none__"`
    /// for ungrouped. Matches the key the servers page passes to
    /// `is_server_group_collapsed` / `toggle_server_group_collapsed`,
    /// so collapse decisions stay in sync.
    key: String,
    /// Raw group string. `None` means ungrouped; resolved to the
    /// localized "Ungrouped" label at render time (locale switches
    /// don't currently re-trigger `update_server_names`, so resolve
    /// late rather than caching).
    group: Option<String>,
    servers: Vec<SidebarServerEntry>,
}

#[derive(Clone, Default)]
struct SidebarServerEntry {
    id: SharedString,
    name: SharedString,
    tag: SharedString,
    color: Option<Hsla>,
}

/// Sidebar navigation component
///
/// Features:
/// - Star button (link to GitHub)
/// - Server list for quick navigation between servers and home
/// - Settings menu with theme and language options
///
/// The sidebar provides quick access to:
/// - Home page (server management)
/// - Connected Redis servers
/// - Application settings (theme, language)
pub struct ZedisSidebar {
    /// Internal state with cached server list
    state: SidebarState,

    /// Event subscriptions for reactive updates
    _subscriptions: Vec<Subscription>,
}

impl ZedisSidebar {
    /// Create a new sidebar component with event subscriptions
    ///
    /// Sets up listeners for:
    /// - Server selection changes (updates current selection)
    /// - Server list updates (refreshes displayed servers)
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = vec![];

        let global_state = cx.global::<ZedisGlobalStore>().state();
        subscriptions.push(cx.subscribe(&global_state, |this, _global_state, event, cx| {
            match event {
                GlobalEvent::ServerListUpdated => {
                    this.update_server_names(cx);
                }
                GlobalEvent::ServerSelected(server_id, _) => {
                    // Refresh server list when servers are added/removed/updated
                    this.state.server_id = server_id.clone();
                }
                _ => {}
            }
            cx.notify();
        }));

        let mut this = Self {
            state: SidebarState::default(),
            _subscriptions: subscriptions,
        };

        info!("Creating new sidebar view");

        // Load initial server list
        this.update_server_names(cx);
        this
    }

    /// Rebuild the cached `sections` from the current server config.
    ///
    /// Mirrors the bucketing in `views::servers::render_server_grid`:
    /// `get_servers()` returns servers in canonical order (group A→Z,
    /// then sort_order ASC, ungrouped last), so a single pass that
    /// merges adjacent same-group entries reconstructs the groups
    /// without sorting.
    fn update_server_names(&mut self, _cx: &mut Context<Self>) {
        let Ok(servers) = get_servers() else { return };

        let mut sections: Vec<SidebarSection> = Vec::new();
        for server in servers.iter() {
            let group = server
                .group
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from);
            let key = group.as_deref().unwrap_or("__none__").to_string();

            let entry = SidebarServerEntry {
                id: server.id.clone().into(),
                name: server.name.clone().into(),
                tag: server.tag_label().unwrap_or_default().to_string().into(),
                color: resolve_tag_color(server.tag_color.as_deref()),
            };

            match sections.last_mut() {
                Some(s) if s.key == key => s.servers.push(entry),
                _ => sections.push(SidebarSection {
                    key,
                    group,
                    servers: vec![entry],
                }),
            }
        }

        self.state.sections = sections;
    }

    /// Render the scrollable server list.
    ///
    /// Layout:
    /// - Home row at the very top (never inside a group)
    /// - One section per server group, each with a collapsible header
    ///   ("Production · 3") + the section's server rows
    /// - Ungrouped servers live in the last section, headed by the
    ///   localized "Ungrouped" label and the `__none__` collapse key
    ///
    /// Collapse state is read directly from `ZedisGlobalStore` so the
    /// same key the servers page toggles also collapses the sidebar
    /// section. A collapsed section still renders its header (so the
    /// user can re-expand) but skips all of its server rows.
    fn render_server_list(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let current_server_id = self.state.server_id.clone();
        // The config editor (`Route::Server(ServerView::Config)`) operates on the active server, so
        // it keeps that server row highlighted; only the Protos / Scripts
        // managers drop the server-row selection.
        let is_match_route = !matches!(
            cx.global::<ZedisGlobalStore>().read(cx).route(),
            Route::Protos | Route::Scripts,
        );

        // App brand name as the top entry — matches the convention
        // used by other places that show the product name ("Zedis"
        // is hard-coded in `main.rs`'s window title, `tray.rs`, and
        // `about.rs`; following the same pattern rather than adding
        // an APP_NAME constant just for this one site).
        let home_label = SharedString::from("Zedis");
        let ungrouped_label = i18n_servers(cx, "ungrouped_label");
        // `list_active` from the theme is intentionally subtle for
        // tightly-packed list views, but in this sparse sidebar it
        // reads as ~no change. Use a 10% foreground overlay instead:
        // ~rgba(255,255,255,0.1) in dark mode, ~rgba(0,0,0,0.1) in
        // light mode. Theme-neutral so any tag colour underneath
        // still reads — important because tagged servers keep their
        // tag-coloured icon on top of the selection pill.
        let list_active_color = cx.theme().foreground.alpha(0.1);
        // Both home and server rows use the same muted/foreground
        // toggle on selection so the icon column registers selection,
        // not just the 3px right strip. The server's tag colour is no
        // longer painted onto the icon itself (it read as jarring) —
        // the icon stays a calm muted/foreground grey and the tag
        // colour shows as a small dot badge at the icon's lower-right
        // corner instead.
        let muted_icon_color = cx.theme().muted_foreground;
        let active_icon_color = cx.theme().foreground;
        // Subtle 1px divider drawn between the home row and the first
        // group section to set Home apart visually as a top-level
        // entry, distinct from the group/server tree below it.
        let divider_color = cx.theme().border;
        // Selection accent (#6b95c4, same in both themes): drives both the left
        // bar and the selected server's database icon.
        let accent_color: Hsla = rgb(0x6b95c4).into();
        // Green status dot on the selected server's row (#69b083 — same green as
        // the status-bar "Connected" indicator).
        let connected_color: Hsla = rgb(0x69b083).into();
        // Ring colour around the tag dot so it reads as a crisp badge over the
        // icon instead of bleeding into the glyph — matches the sidebar panel
        // background (see main.rs).
        let dot_ring_color = cx.theme().background;

        // Snapshot collapse state up front so the click closures
        // don't need to re-borrow `cx`. Server counts are bounded
        // (typically < 50), so a HashSet is overkill — a Vec walk
        // would be fine — but HashSet keeps the lookup site terse.
        let global_store_ref = cx.global::<ZedisGlobalStore>().read(cx);
        // Whole-sidebar collapse (icon-only rail). Distinct from per-group
        // `collapsed_keys` below.
        let sidebar_collapsed = global_store_ref.sidebar_collapsed();
        let collapsed_keys: std::collections::HashSet<String> = self
            .state
            .sections
            .iter()
            .filter(|s| global_store_ref.is_server_group_collapsed(&s.key))
            .map(|s| s.key.clone())
            .collect();

        let mut rows: Vec<gpui::AnyElement> = Vec::new();

        // --- Home row ---
        // Always at the top, outside every group. Highlights when
        // no server is selected (server_id empty) and the route is
        // one of the server-context routes.
        let is_home_current = is_match_route && current_server_id.is_empty();
        // Collapse / expand toggle, placed INSIDE the home item so it shares the
        // same row background. Default (Medium) size renders an icon button at
        // `size_8` (32px) — exactly the `h_8` home-row height — so the chevron
        // fills the row and is a full-height tap target. The click closure stops
        // propagation so it toggles collapse without also firing the home row's
        // on_click.
        let toggle_btn = Button::new("sidebar-collapse-toggle")
            .ghost()
            .icon(if sidebar_collapsed {
                IconName::ChevronRight
            } else {
                IconName::ChevronLeft
            })
            .on_click(move |_, _window, cx| {
                // gpui-component Button doesn't stop normal-click propagation, so
                // without this the click bubbles to the home_item's on_click and
                // navigates home. Stop it so the toggle only collapses/expands.
                cx.stop_propagation();
                update_app_state_and_save(cx, "toggle_sidebar_collapsed", |state, _| {
                    state.toggle_sidebar_collapsed();
                });
            });
        let home_item = ListItem::new("sidebar-home-item")
            .w_full()
            .h_8()
            .px_2()
            .rounded_md()
            .when(is_home_current, |this| this.bg(list_active_color))
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .w_full()
                    .overflow_hidden()
                    .when(sidebar_collapsed, |this| this.justify_center())
                    // Dashboard icon + "Zedis" heading — hidden in the icon rail,
                    // where only the toggle remains.
                    .when(!sidebar_collapsed, |this| {
                        this.child(Icon::new(IconName::LayoutDashboard).text_color(active_icon_color))
                            .child(
                                Label::new(home_label.clone())
                                    .text_sm()
                                    .font_semibold()
                                    .text_color(active_icon_color)
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .flex_1()
                                    .min_w_0(),
                            )
                    })
                    .child(toggle_btn),
            )
            .on_click(move |_, _window, cx| {
                // Collapsed rail: the whole home row is the expand target — the
                // toggle alone is a small tap area, and Home/Zedis are hidden here,
                // so clicking anywhere on the row expands the sidebar.
                if sidebar_collapsed {
                    update_app_state_and_save(cx, "toggle_sidebar_collapsed", |state, _| {
                        state.toggle_sidebar_collapsed();
                    });
                    return;
                }
                if is_home_current {
                    return;
                }
                cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                    store.update(cx, |state, cx| {
                        state.go_to(Route::Home, cx);
                        state.clear_selected_server(cx);
                    });
                });
            });
        rows.push(div().id("sidebar-home-row").mx_2().child(home_item).into_any_element());
        // Thin divider with deliberately asymmetric breathing room —
        // 8px above, 12px below — so the gap below Home reads as a
        // section break rather than uniform list spacing. Sets the
        // top-level Home entry apart from the group tree without
        // competing with the floating selection pill.
        rows.push(div().mx_2().h_px().mt_2().mb_3().bg(divider_color).into_any_element());

        // --- Group sections ---
        for (section_idx, section) in self.state.sections.iter().enumerate() {
            let is_collapsed = collapsed_keys.contains(&section.key);
            // Group header — hidden in the collapsed icon rail.
            if !sidebar_collapsed {
                let header_label = match &section.group {
                    Some(g) => SharedString::from(g.clone()),
                    None => ungrouped_label.clone(),
                };
                let count_label = SharedString::from(section.servers.len().to_string());
                let chevron = if is_collapsed {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronDown
                };
                let toggle_key = section.key.clone();
                let header_id = SharedString::from(format!("sidebar-grp-h-{}", section.key));
                rows.push(
                    h_flex()
                        .id(header_id)
                        .mx_2()
                        .h_6()
                        .gap_1()
                        .items_center()
                        .cursor_pointer()
                        .child(Icon::new(chevron).text_color(muted_icon_color))
                        .child(
                            Label::new(header_label)
                                .text_xs()
                                .text_color(muted_icon_color)
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .flex_1()
                                .min_w_0(),
                        )
                        .child(Label::new(count_label).text_xs().text_color(muted_icon_color))
                        .on_click(move |_, _window, cx| {
                            let key = toggle_key.clone();
                            update_app_state_and_save(cx, "toggle_server_group_collapsed", move |state, _| {
                                state.toggle_server_group_collapsed(&key);
                            });
                        })
                        .into_any_element(),
                );
            } else if section_idx > 0 {
                // Collapsed rail: a short divider keeps groups visually separated.
                rows.push(div().mx_2().h_px().my_1().bg(divider_color).into_any_element());
            }

            if is_collapsed {
                continue;
            }

            for server in &section.servers {
                let entry = server.clone();
                let is_current = is_match_route && entry.id == current_server_id;
                let name = entry.name.clone();
                // Full, untruncated name for the row tooltip — the
                // visible label uses text_ellipsis in this narrow
                // strip so long names ("aliyun-clu…") are unreadable
                // without it. Append the tag label when one is set
                // ("aliyun-cluster · prod"); the corner dot carries
                // the colour, the tooltip carries the word.
                let tooltip_text: SharedString = if entry.tag.is_empty() {
                    name.clone()
                } else {
                    SharedString::from(format!("{} · {}", name, entry.tag))
                };

                let server_id = entry.id.clone();
                let tag_color = entry.color;
                let icon_color = if is_current { accent_color } else { muted_icon_color };
                // Initials for the collapsed rail so servers stay tellable apart
                // at a glance: two letters for Latin names ("upstash" → "UP"), but
                // a single glyph for CJK so a wide character (缓 / 中) isn't cramped.
                let monogram: SharedString = {
                    let mut chars = name.chars().filter(|c| c.is_alphanumeric());
                    match chars.next() {
                        Some(first) if first.is_ascii() => {
                            let mut s = first.to_ascii_uppercase().to_string();
                            if let Some(second) = chars.next().filter(|c| c.is_ascii()) {
                                s.push(second.to_ascii_uppercase());
                            }
                            s
                        }
                        Some(first) => first.to_string(),
                        None => "?".to_string(),
                    }
                    .into()
                };

                let item_id = SharedString::from(format!("sidebar-srv-{}", entry.id));
                let item = ListItem::new(item_id)
                    .w_full()
                    .h_8()
                    // Collapsed rail centers the icon; expanded indents the row.
                    .when(!sidebar_collapsed, |this| this.pl_4().pr_2())
                    .when(sidebar_collapsed, |this| this.px_1())
                    .rounded_md()
                    // Expanded: the full-row pill marks selection. Collapsed: the
                    // monogram box carries the fill, so skip the row bg here to
                    // avoid a doubled (wider, lighter) background behind it.
                    .when(is_current && !sidebar_collapsed, |this| this.bg(list_active_color))
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .w_full()
                            .overflow_hidden()
                            .when(sidebar_collapsed, |this| this.justify_center())
                            // Expanded: database-cylinder icon with the tag colour
                            // as a corner dot badge (ringed so it doesn't merge in).
                            .when(!sidebar_collapsed, |this| {
                                this.child(
                                    div()
                                        .relative()
                                        .flex_none()
                                        .child(Icon::new(CustomIconName::Database).text_color(icon_color))
                                        .when_some(tag_color, |this, color| {
                                            this.child(
                                                div()
                                                    .absolute()
                                                    .bottom_0()
                                                    .right_0()
                                                    .size(px(8.))
                                                    .rounded_full()
                                                    .bg(color)
                                                    .border_2()
                                                    .border_color(dot_ring_color),
                                            )
                                        }),
                                )
                            })
                            // Collapsed rail: an initials monogram (tag dot kept) so
                            // each server is distinguishable without expanding.
                            .when(sidebar_collapsed, |this| {
                                this.child(
                                    div()
                                        .relative()
                                        .flex_none()
                                        .size(px(28.))
                                        .rounded_md()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        // Only the selected server gets a filled box
                                        // (design); the rest are plain letters.
                                        .when(is_current, |this| this.bg(list_active_color))
                                        .child(Label::new(monogram).text_xs().font_bold().text_color(icon_color))
                                        // Corner dot: green for the selected server,
                                        // else the tag colour for tagged servers.
                                        .when(is_current, |this| {
                                            this.child(
                                                div()
                                                    .absolute()
                                                    .bottom_0()
                                                    .right_0()
                                                    .size(px(9.))
                                                    .rounded_full()
                                                    .bg(connected_color)
                                                    .border_2()
                                                    .border_color(dot_ring_color),
                                            )
                                        })
                                        .when(!is_current, |this| {
                                            this.when_some(tag_color, |this, color| {
                                                this.child(
                                                    div()
                                                        .absolute()
                                                        .bottom_0()
                                                        .right_0()
                                                        .size(px(9.))
                                                        .rounded_full()
                                                        .bg(color)
                                                        .border_2()
                                                        .border_color(dot_ring_color),
                                                )
                                            })
                                        }),
                                )
                            })
                            // Name + selected green dot — hidden in the icon rail
                            // (the wrapper tooltip surfaces the name on hover).
                            .when(!sidebar_collapsed, |this| {
                                this.child(
                                    Label::new(name)
                                        .text_xs()
                                        // Bold the selected server's name (design).
                                        .when(is_current, |this| this.font_extrabold())
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .flex_1()
                                        .min_w_0(),
                                )
                                .when(is_current, |this| {
                                    this.child(div().flex_none().size(px(7.)).rounded_full().bg(connected_color))
                                })
                            }),
                    )
                    .on_click(move |_, _window, cx| {
                        if is_current {
                            return;
                        }
                        cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                            store.update(cx, |state, cx| {
                                state.go_to(Route::Server(ServerView::Editor), cx);
                                let id = server_id.to_string();
                                let db = state.last_db_for(&id);
                                state.set_selected_server((id, db), cx);
                            });
                        });
                    });

                // ListItem doesn't impl InteractiveElement, so the
                // tooltip lives on a thin stateful wrapper.
                let wrap_id = SharedString::from(format!("sidebar-srv-w-{}", entry.id));
                rows.push(
                    div()
                        .id(wrap_id)
                        .mx_2()
                        // A little breathing room between monograms in the
                        // collapsed rail (without reading as an over-spaced list).
                        .when(sidebar_collapsed, |this| this.my_1())
                        .relative()
                        // Left accent bar marks the selected server (design).
                        .when(is_current, |this| {
                            this.child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .top(px(6.))
                                    .bottom(px(6.))
                                    .w(px(2.5))
                                    .rounded_sm()
                                    .bg(accent_color),
                            )
                        })
                        .child(item)
                        .tooltip(move |window, cx| Tooltip::new(tooltip_text.clone()).build(window, cx))
                        .into_any_element(),
                );
            }
        }

        v_flex()
            .id("sidebar-redis-servers")
            .size_full()
            .py_2()
            .overflow_y_scrollbar()
            .children(rows)
    }
}

impl Render for ZedisSidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .id("sidebar-container")
            .justify_start()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(div().flex_1().size_full().child(self.render_server_list(window, cx)))
    }
}
