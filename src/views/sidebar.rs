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
    assets::{Assets, CustomIconName},
    connection::get_servers,
    constants::{EDITOR_KEY_BAR_HEIGHT, STATUS_BAR_HEIGHT},
    helpers::{
        SidebarServerAction, humanize_keystroke, is_quiet_tag, resolve_tag_chip, resolve_tag_color, unique_monograms,
    },
    states::{
        ConnectionHealth, GlobalEvent, Route, ZedisGlobalStore, i18n_common, i18n_servers, i18n_sidebar,
        update_app_state_and_save,
    },
};
use gpui::{
    AnyElement, App, Context, DismissEvent, Hsla, Image, ImageFormat, SharedString, Subscription, WeakEntity, Window,
    div, img, prelude::*, px, rgb,
};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    list::ListItem,
    menu::{ContextMenuExt, PopupMenu},
    v_flex,
};
use rust_i18n::t;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::info;
use zedis_ui::{ZedisFormField, ZedisFormOptions};

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

    /// The link health of every server that has a workspace tab, by id —
    /// what the row's dot shows. Pushed by the root, which owns the tabs
    /// (`Zedis::sync_sidebar_health`); a server without a tab is absent
    /// and gets no dot.
    health: HashMap<String, ConnectionHealth>,

    /// The connection whose menu is open. Its row draws no tooltip
    /// meanwhile: the pointer is still on the row, and the tooltip a
    /// right-click cleared came back over the menu at the next move.
    menu_open: Option<SharedString>,
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
    /// The environment label (`Prod`, `UAT`, …), empty when untagged.
    tag: SharedString,
    /// The stored colour key the label maps to; `None` when untagged.
    tag_color_key: Option<String>,
}

/// What a connection's menu in the list offers, and about which connection.
struct ServerMenu {
    sidebar: WeakEntity<ZedisSidebar>,
    id: SharedString,
    /// The group it is in; `None` for none.
    group: Option<String>,
    /// Every named group the list shows.
    groups: Vec<String>,
    ungrouped: SharedString,
    /// The link toggle: `Some(true)` offers Disconnect, `Some(false)`
    /// Reconnect, `None` (no workspace tab on it) neither.
    link: Option<bool>,
}

impl ServerMenu {
    fn build(&self, menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
        // Built as it opens: the row lets its tooltip go until it closes.
        let sidebar = self.sidebar.clone();
        let _ = sidebar.update(cx, |sidebar, cx| {
            sidebar.state.menu_open = Some(self.id.clone());
            cx.notify();
        });
        cx.subscribe_self(move |_, _: &DismissEvent, cx| {
            let _ = sidebar.update(cx, |sidebar, cx| {
                sidebar.state.menu_open = None;
                cx.notify();
            });
        })
        .detach();
        let id = self.id.clone();
        let action = |make: fn(SharedString) -> SidebarServerAction| Box::new(make(id.clone()));
        let menu = menu
            .menu(i18n_sidebar(cx, "server_open"), action(SidebarServerAction::Open))
            .menu(
                i18n_sidebar(cx, "server_open_new_tab"),
                action(SidebarServerAction::OpenInNewTab),
            )
            .separator()
            .menu(i18n_sidebar(cx, "server_edit"), action(SidebarServerAction::Edit))
            .menu(i18n_sidebar(cx, "server_clone"), action(SidebarServerAction::Clone))
            .menu(
                i18n_sidebar(cx, "server_diagnose"),
                action(SidebarServerAction::Diagnose),
            );
        // Every group the list shows, out of the one it is in, and a new one
        // named in a dialog (#219) — which is why the submenu is there even
        // before any group exists: creating the first one used to mean
        // opening the connection's form.
        let (id, current, groups, ungrouped, new_group) = (
            self.id.clone(),
            self.group.clone(),
            self.groups.clone(),
            self.ungrouped.clone(),
            i18n_sidebar(cx, "server_new_group"),
        );
        let menu = menu.submenu(
            i18n_sidebar(cx, "server_move_to_group"),
            window,
            cx,
            move |submenu, _, _| {
                let to = |group: &str| {
                    Box::new(SidebarServerAction::MoveToGroup {
                        id: id.clone(),
                        group: group.to_string().into(),
                    })
                };
                let submenu = groups.iter().fold(submenu, |submenu, group| {
                    submenu.menu_with_check(group.clone(), current.as_ref() == Some(group), to(group))
                });
                submenu
                    .menu_with_check(ungrouped.clone(), current.is_none(), to(""))
                    .separator()
                    .menu(
                        new_group.clone(),
                        Box::new(SidebarServerAction::MoveToNewGroup(id.clone())),
                    )
            },
        );
        let menu = menu.separator();
        let menu = match self.link {
            Some(true) => menu.menu(
                i18n_sidebar(cx, "server_disconnect"),
                action(SidebarServerAction::Disconnect),
            ),
            Some(false) => menu.menu(
                i18n_sidebar(cx, "server_reconnect"),
                action(SidebarServerAction::Reconnect),
            ),
            None => menu,
        };
        menu.menu(i18n_sidebar(cx, "server_delete"), action(SidebarServerAction::Delete))
    }
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

    /// App logo (bundled icon.png), decoded lazily by gpui's image cache.
    /// Built once here so renders don't re-wrap the bytes every frame.
    logo: Arc<Image>,

    /// Light variant (icon-light.png) shown on dark themes, where the
    /// default mark doesn't contrast enough against the panel background.
    logo_light: Arc<Image>,

    /// Event subscriptions for reactive updates
    _subscriptions: Vec<Subscription>,
}

/// Whether the Home control belongs to the sidebar's top band.
///
/// It does on the desktop, where it has always been. The browser puts it in
/// the title bar instead (`views::title_bar::left_slot`): a tab has no
/// window chrome, so that bar's left edge is empty and a wordmark there is
/// read as "back to the list" — while the sidebar's narrow band is better
/// spent on the connections themselves. Two functions rather than a gate
/// inside the row, so that changing the browser's answer cannot reach the
/// desktop's.
#[cfg(not(target_family = "wasm"))]
fn home_lives_in_the_sidebar() -> bool {
    true
}

#[cfg(target_family = "wasm")]
fn home_lives_in_the_sidebar() -> bool {
    false
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

        let load_logo = |name: &str| {
            let bytes = Assets::get(name).map(|item| item.data).unwrap_or_default();
            Arc::new(Image::from_bytes(ImageFormat::Png, bytes.to_vec()))
        };
        let mut this = Self {
            state: SidebarState::default(),
            logo: load_logo("icon.png"),
            logo_light: load_logo("icon-light.png"),
            _subscriptions: subscriptions,
        };

        info!("Creating new sidebar view");

        // Load initial server list
        this.update_server_names(cx);
        this
    }

    /// The root's view of every open tab's link health, keyed by server id.
    /// Replaces the previous map whole: a server whose tab closed drops out.
    pub fn set_health(&mut self, health: HashMap<String, ConnectionHealth>, cx: &mut Context<Self>) {
        if self.state.health != health {
            self.state.health = health;
            cx.notify();
        }
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
                tag_color_key: server.tag_color.clone(),
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

        let ungrouped_label = i18n_servers(cx, "ungrouped_label");
        let look = list_look(cx);
        let &ListLook {
            sidebar_collapsed,
            divider_color,
            muted_icon_color,
            ..
        } = &look;

        // Snapshot collapse state up front so the click closures
        // don't need to re-borrow `cx`. Server counts are bounded
        // (typically < 50), so a HashSet is overkill — a Vec walk
        // would be fine — but HashSet keeps the lookup site terse.
        let global_store_ref = cx.global::<ZedisGlobalStore>().read(cx);
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
        // Without the Home pill this band holds one chevron and a lot of
        // nothing, which reads as a gap rather than a row — so where Home has
        // moved out, the band goes with it and the rail's toggle moves to the
        // bottom bar that already carries it when collapsed (see `render`).
        if home_lives_in_the_sidebar() {
            rows.push(self.home_band(is_home_current, &look, cx));
        }

        // Collapsed rail: labels unique among every visible name, so a
        // shared prefix (`aliyun-*`) does not collapse to the same two
        // letters. Expanded rows show the full name, so they skip this.
        let mut monograms = if sidebar_collapsed {
            unique_monograms(
                self.state
                    .sections
                    .iter()
                    .flat_map(|section| section.servers.iter().map(|entry| entry.name.as_ref())),
            )
            .into_iter()
        } else {
            Vec::new().into_iter()
        };

        let sidebar = cx.entity().downgrade();
        // Where a connection can be moved to from its menu: every named group,
        // once, in the order the list shows them.
        let mut named_groups: Vec<String> = Vec::new();
        for group in self.state.sections.iter().filter_map(|section| section.group.clone()) {
            if !named_groups.contains(&group) {
                named_groups.push(group);
            }
        }

        // --- Group sections ---
        for (section_idx, section) in self.state.sections.iter().enumerate() {
            let is_collapsed = collapsed_keys.contains(&section.key);
            // Group header — hidden in the collapsed icon rail.
            if !sidebar_collapsed {
                rows.push(group_header(section, is_collapsed, &ungrouped_label, muted_icon_color));
            } else if section_idx > 0 {
                // Collapsed rail: a short divider keeps groups visually separated.
                rows.push(div().mx_2().h_px().my_1().bg(divider_color).into_any_element());
            }

            // Per-group collapse only applies to the expanded sidebar: the icon
            // rail hides group headers, so a skipped group would be an
            // unexplained blank between dividers with no way to re-expand it.
            // The rail always shows every server's monogram.
            if is_collapsed && !sidebar_collapsed {
                continue;
            }

            for server in &section.servers {
                let entry = server.clone();
                let is_current = is_match_route && entry.id == current_server_id;
                // The dot is the link, not the selection: for every server with
                // a workspace tab, and nothing for one without (or one whose
                // first heartbeat is still out).
                let health = self.state.health.get(entry.id.as_ref()).copied().unwrap_or_default();
                // Initials for the collapsed rail so servers stay tellable apart
                // at a glance: two letters for Latin names ("upstash" → "UP"), but
                // a single glyph for CJK so a wide character (缓 / 中) isn't cramped.
                // Assigned once for the whole rail (`unique_monograms`) so
                // `aliyun-cluster` / `aliyun-tls` do not all read as `AL`.
                let monogram: SharedString = if sidebar_collapsed {
                    monograms.next().unwrap_or_else(|| "?".to_string()).into()
                } else {
                    SharedString::default()
                };

                // The row's menu (#204): what the app does with a connection
                // elsewhere, one right-click from the list.
                let menu = ServerMenu {
                    sidebar: sidebar.clone(),
                    id: entry.id.clone(),
                    group: section.group.clone(),
                    groups: named_groups.clone(),
                    ungrouped: ungrouped_label.clone(),
                    link: match health {
                        ConnectionHealth::Connected | ConnectionHealth::Reconnecting => Some(true),
                        ConnectionHealth::Offline => Some(false),
                        ConnectionHealth::Unknown => None,
                    },
                };

                rows.push(self.server_row(entry, is_current, health, monogram, menu, &look));
            }
        }

        v_flex()
            .id("sidebar-redis-servers")
            .size_full()
            // No top padding: the fixed-height Home band above must sit flush
            // at the top so it aligns with the content pane's top bar. Keep the
            // bottom breathing room.
            .pb_2()
            .overflow_y_scrollbar()
            .children(rows)
    }

    /// Fixed-height top band (== the editor's key bar) with the Home pill
    /// vertically centered, so the sidebar's top row lines up with the
    /// content pane's top bar instead of sitting a couple px off.
    fn home_band(&self, is_home_current: bool, look: &ListLook, cx: &App) -> AnyElement {
        let &ListLook {
            sidebar_collapsed,
            active_icon_color,
            divider_color,
            ..
        } = look;
        // App brand name as the top entry — matches the convention
        // used by other places that show the product name ("Zedis"
        // is hard-coded in `main.rs`'s window title, `tray.rs`, and
        // `about.rs`; following the same pattern rather than adding
        // an APP_NAME constant just for this one site).
        let home_label = SharedString::from("ZEDIS");
        let home_item = ListItem::new("sidebar-home-item")
            .w_full()
            .h_full()
            .px_2()
            .rounded_md()
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .w_full()
                    .overflow_hidden()
                    .when(sidebar_collapsed, |this| this.justify_center())
                    // Top slot is Home in both states: the collapsed rail keeps
                    // just the app logo (the expand toggle lives in the fixed
                    // bottom bar), the expanded sidebar adds the heading. The
                    // logo is a bitmap (no theme tinting), so dark themes swap
                    // in the light variant for contrast; picked per render so a
                    // live theme switch updates it immediately.
                    .child(
                        img(if cx.theme().is_dark() {
                            self.logo_light.clone()
                        } else {
                            self.logo.clone()
                        })
                        .flex_none()
                        .size(px(18.)),
                    )
                    .when(!sidebar_collapsed, |this| {
                        this.child(
                            Label::new(home_label.clone())
                                .text_sm()
                                .font_semibold()
                                .text_color(active_icon_color)
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .flex_1()
                                .min_w_0(),
                        )
                    }),
            )
            .on_click(move |e, _window, cx| {
                // Mirror the server rows: a plain click navigates the current
                // tab to Home; the `sidebar_click_new_tab` setting (⌘/Ctrl
                // inverts, ⌘⇧ forces) opens Home in a workspace tab instead. An
                // empty server id is the Home "connection" the tab machinery
                // already understands (project_active_tab / ServerSelected).
                let mods = e.modifiers();
                let secondary = mods.secondary(cx);
                let force_new = secondary && mods.shift;
                let default_new_tab = cx.global::<ZedisGlobalStore>().read(cx).sidebar_click_new_tab();
                let want_new_tab = force_new || (default_new_tab != secondary);
                if is_home_current && !want_new_tab {
                    return;
                }
                cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                    store.update(cx, |state, cx| {
                        if force_new {
                            state.open_server_in_new_tab(String::new(), 0, cx);
                        } else if want_new_tab {
                            state.reveal_or_open_server_tab(String::new(), 0, cx);
                        } else {
                            state.go_to(Route::Home, cx);
                            state.clear_selected_server(cx);
                        }
                    });
                });
            });
        let home_tooltip = i18n_sidebar(cx, "home");
        let collapse_label = i18n_sidebar(cx, "collapse");
        h_flex()
            .id("sidebar-home-row")
            .mx_2()
            .h(EDITOR_KEY_BAR_HEIGHT)
            .items_center()
            .gap_1()
            .child(div().flex_1().min_w_0().h_full().child(home_item))
            // Collapse control at the band's right edge (the sidebar's
            // top-right). Icon-only: a label would fight the ZEDIS
            // heading for this narrow strip — the tooltip carries the
            // word. The expand control (collapsed state) stays in the
            // fixed bottom bar rendered by `render`.
            .when(!sidebar_collapsed, |this| {
                this.child(
                    Button::new("sidebar-collapse-toggle")
                        .ghost()
                        .small()
                        .icon(IconName::ChevronLeft)
                        .tooltip(collapse_label)
                        .on_click(move |_, _window, cx| {
                            update_app_state_and_save(cx, "toggle_sidebar_collapsed", |state, _| {
                                state.toggle_sidebar_collapsed();
                            });
                        }),
                )
            })
            .border_b_1()
            .border_color(divider_color)
            .when(sidebar_collapsed, |this| {
                this.tooltip(move |window, cx| Tooltip::new(home_tooltip.clone()).build(window, cx))
            })
            .into_any_element()
    }

    /// One connection: its icon, name, environment chip and link dot — or,
    /// in the rail, its monogram with the dot on its corner — opening it on
    /// a click, its menu on a right-click, its name and the click's
    /// gestures in its tooltip.
    fn server_row(
        &self,
        entry: SidebarServerEntry,
        is_current: bool,
        health: ConnectionHealth,
        monogram: SharedString,
        menu: ServerMenu,
        look: &ListLook,
    ) -> AnyElement {
        let &ListLook {
            sidebar_collapsed,
            list_active_color,
            muted_icon_color,
            accent_color,
            connected_color,
            reconnecting_color,
            offline_color,
            dark,
            dot_ring_color,
            ref new_tab_hint,
            ref dup_tab_hint,
            ..
        } = look;
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
        let icon_color = if is_current { accent_color } else { muted_icon_color };
        // The environment, where it matters: UAT and Prod get a chip
        // in the list and a corner dot in the rail; Dev and Local get
        // nothing. A mark on every row says nothing, and the badge is
        // spent where knowing which server this is changes what
        // someone does next.
        let quiet = is_quiet_tag(entry.tag_color_key.as_deref());
        let tag_label = entry.tag.clone();
        let tag_chip = (!quiet && !tag_label.is_empty())
            .then(|| resolve_tag_chip(entry.tag_color_key.as_deref(), dark))
            .flatten();
        let tag_dot = (!quiet)
            .then(|| resolve_tag_color(entry.tag_color_key.as_deref()))
            .flatten();
        let health_color = match health {
            ConnectionHealth::Connected => Some(connected_color),
            ConnectionHealth::Reconnecting => Some(reconnecting_color),
            ConnectionHealth::Offline => Some(offline_color),
            ConnectionHealth::Unknown => None,
        };

        let item_id = SharedString::from(format!("sidebar-srv-{}", entry.id));
        let item = ListItem::new(item_id)
            .w_full()
            .h_8()
            // Collapsed rail centers the monogram; expanded indents the
            // icon under the group label's chevron.
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
                    // Collapsed rail: an initials monogram so each server is
                    // distinguishable without expanding. The corner dot is
                    // the link health where there is a tab, else the
                    // environment for UAT / Prod.
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
                                .when_some(health_color.or(tag_dot), |this, color| {
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
                                }),
                        )
                    })
                    // Expanded: the database cylinder, the name, the
                    // environment chip where there is one, and the link dot.
                    // The icon is back by request — a bare column of names
                    // read as a plain list — and carries the accent when the
                    // row is the current server; the environment is the
                    // chip's to say, so the icon wears no dot of its own.
                    .when(!sidebar_collapsed, |this| {
                        this.child(
                            div()
                                .flex_none()
                                .child(Icon::new(CustomIconName::Database).text_color(icon_color)),
                        )
                        .child(
                            Label::new(name)
                                .text_xs()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .flex_1()
                                .min_w_0(),
                        )
                        // Smaller than the status bar's chip: this row is a
                        // list entry, and the label is a mark on it, not a
                        // heading — 10px type on a 15px pill.
                        .when_some(tag_chip, |this, (bg, fg)| {
                            this.child(
                                div()
                                    .flex_none()
                                    .h(px(15.))
                                    .px(px(4.))
                                    .rounded_sm()
                                    .bg(bg)
                                    .text_size(px(10.))
                                    .line_height(px(15.))
                                    .text_color(fg)
                                    .child(tag_label.clone()),
                            )
                        })
                        .when_some(health_color, |this, color| {
                            this.child(div().flex_none().size(px(7.)).rounded_full().bg(color))
                        })
                    }),
            )
            .on_click(move |e, _window, cx| {
                // A no-modifier click either switches the current tab in
                // place or opens a workspace tab, per the
                // `sidebar_click_new_tab` setting. ⌘/Ctrl+click inverts
                // that choice for this click; ⌘⇧+click always forces a
                // fresh duplicate tab. The in-place case on the current
                // server is a no-op (as a plain re-click always was).
                let mods = e.modifiers();
                let secondary = mods.secondary(cx);
                let force_new = secondary && mods.shift;
                let default_new_tab = cx.global::<ZedisGlobalStore>().read(cx).sidebar_click_new_tab();
                let want_new_tab = force_new || (default_new_tab != secondary);
                if is_current && !want_new_tab {
                    return;
                }
                cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                    store.update(cx, |state, cx| {
                        let id = server_id.to_string();
                        let db = state.open_db_for(&id);
                        if force_new {
                            state.open_server_in_new_tab(id, db, cx);
                        } else if want_new_tab {
                            state.reveal_or_open_server_tab(id, db, cx);
                        } else {
                            state.connect_server(id, db, cx);
                        }
                    });
                });
            });

        // ListItem doesn't impl InteractiveElement, so the
        // tooltip lives on a thin stateful wrapper.
        let wrap_id = SharedString::from(format!("sidebar-srv-w-{}", entry.id));
        div()
            .id(wrap_id)
            .mx_2()
            // A little breathing room between monograms in the
            // collapsed rail (without reading as an over-spaced list).
            .when(sidebar_collapsed, |this| this.my_1())
            // The selected server is marked by its row's fill and its
            // icon's accent. It also wore a bar down its left edge,
            // a second mark for what the fill already said.
            .child(item)
            .when(self.state.menu_open.as_ref() != Some(&entry.id), |this| {
                this.tooltip({
                    let name = tooltip_text.clone();
                    let hint = new_tab_hint.clone();
                    let dup_hint = dup_tab_hint.clone();
                    move |window, cx| {
                        let name = name.clone();
                        let hint = hint.clone();
                        let dup_hint = dup_hint.clone();
                        Tooltip::element(move |_window, cx| {
                            let muted = cx.theme().muted_foreground;
                            v_flex()
                                .gap_0p5()
                                .child(name.clone())
                                .child(div().text_xs().text_color(muted).child(hint.clone()))
                                .child(div().text_xs().text_color(muted).child(dup_hint.clone()))
                        })
                        .build(window, cx)
                    }
                })
            })
            // Last: it wraps the row, and what it returns takes no tooltip.
            .context_menu(move |popup, window, cx| menu.build(popup, window, cx))
            .into_any_element()
    }
}

/// What every row of the list is drawn with, worked out once a render.
struct ListLook {
    /// The whole sidebar as an icon rail.
    sidebar_collapsed: bool,
    /// The tab gestures, added to every connection's tooltip.
    new_tab_hint: SharedString,
    dup_tab_hint: SharedString,
    list_active_color: Hsla,
    muted_icon_color: Hsla,
    active_icon_color: Hsla,
    divider_color: Hsla,
    accent_color: Hsla,
    connected_color: Hsla,
    reconnecting_color: Hsla,
    offline_color: Hsla,
    dark: bool,
    dot_ring_color: Hsla,
}

fn list_look(cx: &App) -> ListLook {
    // Discoverability hints appended to every server row's tooltip: the
    // workspace-tab gestures (⌘/Ctrl+click to reveal-or-open, add Shift to
    // force a duplicate tab) are otherwise invisible. `%{key}` is filled
    // with the platform keystroke (⌘ / ⌘ ⇧ on macOS, Ctrl/Ctrl+Shift else).
    let store = cx.global::<ZedisGlobalStore>().read(cx);
    let locale = store.locale().to_string();
    // When the setting makes a plain click open a tab, ⌘/Ctrl+click inverts
    // to switching in place — so the modifier hint flips accordingly.
    let new_tab_hint: SharedString = if store.sidebar_click_new_tab() {
        t!(
            "sidebar.open_in_current_tab_hint",
            key = humanize_keystroke("secondary"),
            locale = locale
        )
    } else {
        t!(
            "sidebar.open_in_new_tab_hint",
            key = humanize_keystroke("secondary"),
            locale = locale
        )
    }
    .to_string()
    .into();
    let dup_tab_hint: SharedString = t!(
        "sidebar.open_in_new_tab_dup_hint",
        key = humanize_keystroke("secondary-shift"),
        locale = locale
    )
    .to_string()
    .into();
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
    // Icon accent (#6b95c4, same in both themes) for the selected server's
    // database icon / monogram.
    let accent_color: Hsla = rgb(0x6b95c4).into();
    // Green status dot on the selected server's row (#69b083 — same green as
    // the status-bar "Connected" indicator).
    let connected_color: Hsla = rgb(0x69b083).into();
    // The other two link states, in the theme's own words.
    let reconnecting_color = cx.theme().warning;
    let offline_color = cx.theme().danger;
    let dark = cx.theme().is_dark();
    // Ring colour around the tag dot so it reads as a crisp badge over the
    // icon instead of bleeding into the glyph — matches the sidebar panel
    // background (see main.rs).
    let dot_ring_color = cx.theme().background;
    let sidebar_collapsed = store.sidebar_collapsed();
    ListLook {
        sidebar_collapsed,
        new_tab_hint,
        dup_tab_hint,
        list_active_color,
        muted_icon_color,
        active_icon_color,
        divider_color,
        accent_color,
        connected_color,
        reconnecting_color,
        offline_color,
        dark,
        dot_ring_color,
    }
}

/// A group's header: its chevron and name, its count while it is closed,
/// and a click that opens or closes it.
fn group_header(
    section: &SidebarSection,
    is_collapsed: bool,
    ungrouped_label: &SharedString,
    muted_icon_color: Hsla,
) -> AnyElement {
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
    h_flex()
        .id(header_id)
        .mx_2()
        // Ends where the rows' content ends (`pr_2` on the
        // item), so the count and a row's dot share a spine.
        .pr_2()
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
        // The count only when the rows are hidden: open, they
        // can be counted; closed, it says what the header holds.
        .when(is_collapsed, |this| {
            this.child(Label::new(count_label).text_xs().text_color(muted_icon_color))
        })
        .on_click(move |_, _window, cx| {
            let key = toggle_key.clone();
            update_app_state_and_save(cx, "toggle_server_group_collapsed", move |state, _| {
                state.toggle_server_group_collapsed(&key);
            });
        })
        .into_any_element()
}

impl Render for ZedisSidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Expanded: the collapse control sits at the top-right of the Home
        // band (see `render_server_list`). Collapsed: the rail is too narrow
        // to fit a control beside the logo, so the expand toggle keeps a
        // fixed (non-scrolling) bottom bar — present on every route so the
        // rail can always be expanded again. Between the two, one toggle is
        // always visible in either state.
        //
        // Where the Home band is gone (the browser, which keeps Home in the
        // title bar) there is no top-right to sit in, so the bottom bar
        // carries both directions and is the toggle's one fixed place.
        let sidebar_collapsed = cx.global::<ZedisGlobalStore>().read(cx).sidebar_collapsed();
        let toggle_at_the_bottom = sidebar_collapsed || !home_lives_in_the_sidebar();
        let collapse_label = i18n_sidebar(cx, "collapse");
        let border = cx.theme().border;
        v_flex()
            .size_full()
            .id("sidebar-container")
            .justify_start()
            .border_r_1()
            .border_color(border)
            .child(
                div()
                    .flex_1()
                    .size_full()
                    .min_h_0()
                    .child(self.render_server_list(window, cx)),
            )
            .when(toggle_at_the_bottom, |this| {
                this.child(
                    h_flex()
                        .flex_none()
                        .w_full()
                        // Same height as the status bar to its right (which ends at
                        // the sidebar's edge), so the toggle sits on that bar's line.
                        // No top border: the sidebar is one continuous panel, and a
                        // rule here would only cut it in two.
                        .h(STATUS_BAR_HEIGHT)
                        .items_center()
                        .justify_center()
                        .child(
                            // The collapsed rail is too narrow for text — icon-only.
                            // The chevron points the way the click moves the rail.
                            Button::new("sidebar-rail-toggle")
                                .ghost()
                                .icon(if sidebar_collapsed {
                                    IconName::ChevronRight
                                } else {
                                    IconName::ChevronLeft
                                })
                                .when(!sidebar_collapsed, |this| this.tooltip(collapse_label.clone()))
                                .on_click(move |_, _window, cx| {
                                    update_app_state_and_save(cx, "toggle_sidebar_collapsed", |state, _| {
                                        state.toggle_sidebar_collapsed();
                                    });
                                }),
                        ),
                )
            })
    }
}

/// Name a group and move `server_id` into it — the sidebar menu's
/// Move to Group › New Group… (#219). One field, the connection form's own
/// wording for it; a name that is already a group simply moves there, and
/// surrounding spaces are not part of a name (`set_server_group` trims).
pub(crate) fn open_new_group_dialog(server_id: SharedString, window: &mut Window, cx: &mut App) {
    ZedisFormOptions::new(vec![
        ZedisFormField::new(NEW_GROUP_FIELD, i18n_servers(cx, "group"))
            .placeholder(i18n_servers(cx, "group_placeholder"))
            .required()
            .focus(),
    ])
    .title(i18n_sidebar(cx, "new_group_title"))
    .confirm_label(i18n_sidebar(cx, "new_group_confirm"))
    .cancel_label(i18n_common(cx, "cancel"))
    .on_dialog_submit(move |values, _window, cx| {
        let name = values
            .get(NEW_GROUP_FIELD)
            .map(|name| name.trim().to_string())
            .unwrap_or_default();
        if name.is_empty() {
            return false;
        }
        cx.global::<ZedisGlobalStore>()
            .clone()
            .update(cx, |state, cx| state.set_server_group(&server_id, Some(name), cx));
        true
    })
    .dialog_width(px(420.))
    .open_dialog(window, cx);
}

/// The new group dialog's one field.
const NEW_GROUP_FIELD: &str = "group";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::states::ServerView;
    use crate::test_support::init_ui;
    use gpui::{AnyWindowHandle, Entity, Modifiers, MouseButton, TestAppContext, point, size};
    use gpui_kit::component::Root;
    use gpui_kit::test::{TestEventExt, TestWindowExt};
    use std::cell::RefCell;
    use std::rc::Rc;

    type Sent = Rc<RefCell<Vec<SidebarServerAction>>>;

    /// The sidebar inside a view that takes every `SidebarServerAction` on
    /// its way down and keeps it: what a menu item sends is known, and
    /// nothing it would do is done.
    struct SidebarHarness {
        sidebar: Entity<ZedisSidebar>,
        sent: Sent,
    }

    impl Render for SidebarHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let sent = self.sent.clone();
            div()
                .size_full()
                .capture_action(move |action: &SidebarServerAction, _, cx| {
                    sent.borrow_mut().push(action.clone());
                    cx.stop_propagation();
                })
                .child(self.sidebar.clone())
        }
    }

    fn entry(id: &str, tag: &str, color: Option<&str>) -> SidebarServerEntry {
        SidebarServerEntry {
            id: id.into(),
            name: id.into(),
            tag: tag.into(),
            tag_color_key: color.map(str::to_string),
        }
    }

    /// Set straight on the sidebar rather than through the server list,
    /// which is the process's: `prod` holding `a` and `b`, and `c` in none.
    fn sections() -> Vec<SidebarSection> {
        vec![
            SidebarSection {
                key: "prod".into(),
                group: Some("prod".into()),
                servers: vec![entry("a", "Prod", Some("red")), entry("b", "", None)],
            },
            SidebarSection {
                key: "__none__".into(),
                group: None,
                servers: vec![entry("c", "", None)],
            },
        ]
    }

    fn open_sidebar(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<ZedisSidebar>, Sent) {
        let sent = Sent::default();
        let mut sidebar = None;
        let handle = cx.open_window(size(px(280.), px(640.)), |window, cx| {
            let view = cx.new(|cx| ZedisSidebar::new(window, cx));
            sidebar = Some(view.clone());
            let harness = cx.new(|_| SidebarHarness {
                sidebar: view,
                sent: sent.clone(),
            });
            Root::new(harness, window, cx)
        });
        let sidebar = sidebar.expect("the window built the sidebar");
        sidebar.update(cx, |sidebar, cx| {
            sidebar.state.sections = sections();
            cx.notify();
        });
        (handle.into(), sidebar, sent)
    }

    /// What `row`'s right-click menu offers, top to bottom: each item
    /// clicked in a menu opened afresh, `None` for one that sends nothing —
    /// a separator, or a submenu's entry.
    fn menu_items(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        sent: &Sent,
        row: &'static str,
    ) -> Vec<Option<SidebarServerAction>> {
        let mut items = Vec::new();
        for item in 0usize..30 {
            sent.borrow_mut().clear();
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.right_click(row, cx);
            })
            .expect("the window is open");
            cx.run_until_parked();
            let offered = cx
                .update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    let offered = window.within("popup-menu").try_find(item).is_some();
                    if offered {
                        window.within("popup-menu").click(item, cx);
                    }
                    offered
                })
                .expect("the window is open");
            cx.run_until_parked();
            // A click on the empty list below the rows closes what is open.
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let empty = point(px(140.), px(600.));
                window.pointer_down(empty, MouseButton::Left, cx);
                window.pointer_up(empty, MouseButton::Left, cx);
            })
            .expect("the window is open");
            cx.run_until_parked();
            if !offered {
                return items;
            }
            let sent = sent.borrow();
            assert!(sent.len() <= 1, "item {item} sent {sent:?}");
            items.push(sent.first().cloned());
        }
        panic!("{row}'s menu kept offering items");
    }

    /// Home and its collapse control, a row per connection, a group's
    /// header closing the group; and the rail, which has no collapse control
    /// at the top and shows every connection, a closed group's too.
    #[gpui::test]
    fn the_list_draws_home_its_rows_and_closes_a_group(cx: &mut TestAppContext) {
        init_ui(cx);
        let (handle, sidebar, _) = open_sidebar(cx);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            for id in [
                "sidebar-home-item",
                "sidebar-collapse-toggle",
                "sidebar-srv-a",
                "sidebar-srv-b",
                "sidebar-srv-c",
            ] {
                assert!(window.try_find(id).is_some(), "{id}");
            }
            // The header is the line just above its group's first row.
            let first = window.find("sidebar-srv-a").bounds();
            let header = point(first.center().x, first.top() - px(12.));
            window.pointer_down(header, MouseButton::Left, cx);
            window.pointer_up(header, MouseButton::Left, cx);
        })
        .expect("the window is open");
        cx.run_until_parked();
        sidebar.update(cx, |_, cx| cx.notify());
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("sidebar-srv-a").is_none(), "a closed group's row");
            assert!(window.try_find("sidebar-srv-b").is_none(), "a closed group's row");
            assert!(window.try_find("sidebar-srv-c").is_some());
        })
        .expect("the window is open");

        cx.update(|cx| {
            cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
                state.toggle_sidebar_collapsed();
                cx.notify();
            })
        });
        sidebar.update(cx, |_, cx| cx.notify());
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("sidebar-collapse-toggle").is_none());
            for id in ["sidebar-home-item", "sidebar-srv-a", "sidebar-srv-b", "sidebar-srv-c"] {
                assert!(window.try_find(id).is_some(), "{id} in the rail");
            }
        })
        .expect("the window is open");
    }

    /// A click connects the tab in place; ⌘/Ctrl opens the connection in a
    /// tab and ⌘/Ctrl ⇧ always in a new one, Home too. The menu is the
    /// connection's, and its link item follows the link: Disconnect while it
    /// is up or coming back, Reconnect once it is down, neither without a tab.
    #[gpui::test]
    fn a_row_opens_its_connection_and_its_menu_follows_the_link(cx: &mut TestAppContext) {
        use SidebarServerAction as A;
        init_ui(cx);
        let (handle, sidebar, sent) = open_sidebar(cx);
        let opened: Rc<RefCell<Vec<(String, bool)>>> = Rc::default();
        cx.update(|cx| {
            let global = cx.global::<ZedisGlobalStore>().state();
            let opened = opened.clone();
            cx.subscribe(&global, move |_, event, _| {
                if let GlobalEvent::ServerOpenInNewTab(id, _, duplicate) = event {
                    opened.borrow_mut().push((id.to_string(), *duplicate));
                }
            })
            .detach();
        });

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("sidebar-srv-b", cx);
        })
        .expect("the window is open");
        cx.run_until_parked();
        cx.read(|cx| {
            let state = cx.global::<ZedisGlobalStore>().read(cx);
            assert_eq!(state.selected_server(), Some(&("b".to_string(), 0)));
            assert!(matches!(
                state.route(),
                Route::Server { id, db: 0, view: ServerView::Editor } if id.as_ref() == "b"
            ));
        });

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let secondary = Modifiers::secondary_key(cx);
            let shift_secondary = Modifiers {
                shift: true,
                ..secondary
            };
            window.click_with_modifiers("sidebar-srv-c", secondary, cx);
            window.click_with_modifiers("sidebar-srv-c", shift_secondary, cx);
            window.click_with_modifiers("sidebar-home-item", secondary, cx);
        })
        .expect("the window is open");
        cx.run_until_parked();
        assert_eq!(
            *opened.borrow(),
            [
                ("c".to_string(), false),
                ("c".to_string(), true),
                (String::new(), false)
            ]
        );

        let id = |id: &str| SharedString::from(id);
        let menu = |link: Option<A>| {
            let mut items = vec![
                Some(A::Open(id("a"))),
                Some(A::OpenInNewTab(id("a"))),
                None,
                Some(A::Edit(id("a"))),
                Some(A::Clone(id("a"))),
                Some(A::Diagnose(id("a"))),
                None,
                None,
            ];
            items.extend(link.map(Some));
            items.push(Some(A::Delete(id("a"))));
            items
        };
        assert_eq!(menu_items(cx, handle, &sent, "sidebar-srv-a"), menu(None));
        for (health, link) in [
            (ConnectionHealth::Connected, A::Disconnect(id("a"))),
            (ConnectionHealth::Reconnecting, A::Disconnect(id("a"))),
            (ConnectionHealth::Offline, A::Reconnect(id("a"))),
        ] {
            sidebar.update(cx, |sidebar, cx| {
                sidebar.set_health(HashMap::from([("a".to_string(), health)]), cx);
            });
            assert_eq!(menu_items(cx, handle, &sent, "sidebar-srv-a"), menu(Some(link)));
        }
    }
}
