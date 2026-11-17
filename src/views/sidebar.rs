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

mod menu;

pub(crate) use menu::open_new_group_dialog;

use crate::{
    assets::{Assets, CustomIconName},
    connection::get_servers,
    constants::{EDITOR_KEY_BAR_HEIGHT, STATUS_BAR_HEIGHT},
    helpers::{
        get_mono_font_family, healthy_color, humanize_keystroke, is_quiet_tag, resolve_tag_chip, resolve_tag_color,
        sidebar_accent_color, unique_monograms,
    },
    states::{
        ConnectionHealth, GlobalEvent, Route, ZedisGlobalStore, i18n_servers, i18n_sidebar, update_app_state_and_save,
    },
};
use gpui::{
    AnyElement, App, ClickEvent, Context, ElementId, FontWeight, Hsla, Image, ImageFormat, Modifiers, Pixels,
    SharedString, Subscription, Window, div, img, prelude::*, relative, rems,
};
use gpui_kit::TestSupportExt;
use gpui_kit::base::Button as BaseButton;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Sizable, StyledExt, ThemeStyled,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    menu::ContextMenuExt,
    v_flex,
};
use menu::ServerMenu;
use rust_i18n::t;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::info;

const SIDEBAR_CONTAINER_ID: &str = "sidebar-container";
const SIDEBAR_LIST_ID: &str = "sidebar-redis-servers";
const SIDEBAR_HOME_ROW_ID: &str = "sidebar-home-row";
const SIDEBAR_HOME_ITEM_ID: &str = "sidebar-home-item";
const SIDEBAR_COLLAPSE_TOGGLE_ID: &str = "sidebar-collapse-toggle";
const SIDEBAR_RAIL_TOGGLE_ID: &str = "sidebar-rail-toggle";
const SIDEBAR_EMPTY_ID: &str = "sidebar-empty";

/// Internal state for sidebar component.
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

    /// Currently selected server ID (empty string means home page).
    server_id: SharedString,

    /// The link health of every server that has a workspace tab, by id —
    /// what the row's dot shows. Pushed by the root, which owns the tabs
    /// (`Zedis::sync_sidebar_health`); a server without a tab is absent
    /// and gets no dot.
    health: HashMap<String, ConnectionHealth>,

    /// The connection whose menu is open. Its row drops its tooltip
    /// and holds a quiet open highlight until the menu closes.
    menu_open: Option<SharedString>,
}

#[derive(Clone, Default)]
struct SidebarSection {
    /// Stable collapse key — the trimmed group name, or `"__none__"`
    /// for ungrouped. Matches the key the servers page passes to
    /// `is_server_group_collapsed` / `toggle_server_group_collapsed`.
    key: String,
    /// Raw group string. `None` means ungrouped; resolved to the
    /// localized "Ungrouped" label at render time.
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

/// Sidebar navigation component providing quick access to Home and configured Redis servers.
pub struct ZedisSidebar {
    /// Internal state with cached server list.
    state: SidebarState,

    /// App logo (`icon.png`), decoded lazily by gpui's image cache.
    logo: Arc<Image>,

    /// Light variant (`icon-light.png`) shown on dark themes.
    logo_light: Arc<Image>,

    /// Event subscriptions for reactive updates.
    _subscriptions: Vec<Subscription>,
}

/// Whether the Home control belongs to the sidebar's top band.
#[cfg(not(target_family = "wasm"))]
fn home_lives_in_the_sidebar() -> bool {
    true
}

#[cfg(target_family = "wasm")]
fn home_lives_in_the_sidebar() -> bool {
    false
}

impl ZedisSidebar {
    /// Create a new sidebar component with event subscriptions.
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = vec![];

        let global_state = cx.global::<ZedisGlobalStore>().state();
        subscriptions.push(cx.subscribe(&global_state, |this, _global_state, event, cx| {
            match event {
                GlobalEvent::ServerListUpdated => {
                    this.update_server_names(cx);
                }
                GlobalEvent::ServerSelected(server_id, _) => {
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

        this.update_server_names(cx);
        this
    }

    /// Replace the link health map for all open tabs, keyed by server id.
    pub fn set_health(&mut self, health: HashMap<String, ConnectionHealth>, cx: &mut Context<Self>) {
        if self.state.health != health {
            self.state.health = health;
            cx.notify();
        }
    }

    /// Track which server row currently owns an open right-click context menu.
    pub(super) fn set_menu_open(&mut self, menu_open: Option<SharedString>, cx: &mut Context<Self>) {
        if self.state.menu_open != menu_open {
            self.state.menu_open = menu_open;
            cx.notify();
        }
    }

    /// Rebuild the cached `sections` from the current server config.
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

    /// Render the scrollable server list including Home and grouped connection rows.
    fn render_server_list(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let current_server_id = self.state.server_id.clone();
        // Only the Protos / Scripts managers drop server-row selection.
        let is_match_route = !matches!(
            cx.global::<ZedisGlobalStore>().read(cx).route(),
            Route::Protos | Route::Scripts,
        );

        let ungrouped_label = i18n_servers(cx, "ungrouped_label");
        let look = list_look(cx);
        let sidebar_collapsed = look.sidebar_collapsed;

        let global_store_ref = cx.global::<ZedisGlobalStore>().read(cx);
        let collapsed_keys: HashSet<String> = self
            .state
            .sections
            .iter()
            .filter(|s| global_store_ref.is_server_group_collapsed(&s.key))
            .map(|s| s.key.clone())
            .collect();

        let mut rows: Vec<AnyElement> = Vec::new();
        let is_home_current = is_match_route && current_server_id.is_empty();
        if home_lives_in_the_sidebar() {
            rows.push(self.home_band(is_home_current, &look, window, cx));
        }

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
        let mut named_groups: Vec<String> = Vec::new();
        for group in self.state.sections.iter().filter_map(|section| section.group.clone()) {
            if !named_groups.contains(&group) {
                named_groups.push(group);
            }
        }

        for (section_idx, section) in self.state.sections.iter().enumerate() {
            let is_collapsed = collapsed_keys.contains(&section.key);
            if !sidebar_collapsed {
                rows.push(group_header(section, is_collapsed, &ungrouped_label, &look, window, cx));
            } else if section_idx > 0 {
                rows.push(div().mx_2().h_px().my_1().bg(look.divider_color).into_any_element());
            }

            if is_collapsed && !sidebar_collapsed {
                continue;
            }

            for server in &section.servers {
                let entry = server.clone();
                let is_current = is_match_route && entry.id == current_server_id;
                let health = self.state.health.get(entry.id.as_ref()).copied().unwrap_or_default();
                let monogram: SharedString = if sidebar_collapsed {
                    monograms.next().unwrap_or_else(|| "?".to_string()).into()
                } else {
                    SharedString::default()
                };
                let server_id = entry.id.clone();
                let is_menu_open = self.state.menu_open.as_ref() == Some(&server_id);
                let menu = ServerMenu {
                    sidebar: sidebar.clone(),
                    id: server_id.clone(),
                    group: section.group.clone(),
                    groups: named_groups.clone(),
                    ungrouped: ungrouped_label.clone(),
                    link: match health {
                        ConnectionHealth::Connected | ConnectionHealth::Reconnecting => Some(true),
                        ConnectionHealth::Offline => Some(false),
                        ConnectionHealth::Unknown => None,
                    },
                };

                rows.push(server_row(
                    ServerRowInput {
                        entry,
                        is_current,
                        is_menu_open,
                        health,
                        monogram,
                        menu,
                        look: &look,
                    },
                    window,
                    cx,
                ));
            }
        }

        if self.state.sections.is_empty() && !sidebar_collapsed {
            rows.push(
                div()
                    .id(SIDEBAR_EMPTY_ID)
                    .test_support()
                    .mx_2()
                    .mt_3()
                    .px_2()
                    .child(
                        Label::new(i18n_servers(cx, "empty_title"))
                            .text_xs()
                            .text_color(look.muted_fg),
                    )
                    .into_any_element(),
            );
        }

        v_flex()
            .id(SIDEBAR_LIST_ID)
            .size_full()
            .pb_2()
            .overflow_y_scrollbar()
            .children(rows)
    }

    /// Fixed-height top band aligned with the editor key bar containing Home and Collapse.
    fn home_band(&self, is_home_current: bool, look: &ListLook, window: &mut Window, cx: &mut App) -> AnyElement {
        let sidebar_collapsed = look.sidebar_collapsed;
        let home_tooltip = i18n_sidebar(cx, "home");
        let collapse_label = i18n_sidebar(cx, "collapse");
        let mark = if cx.theme().is_dark() {
            self.logo_light.clone()
        } else {
            self.logo.clone()
        };
        let label_color = if is_home_current { look.selected_fg } else { look.row_fg };

        let home_button = sidebar_focus_button(
            SIDEBAR_HOME_ITEM_ID,
            home_tooltip.clone(),
            is_home_current,
            false,
            look,
            window,
            cx,
        )
        .aria_selected(is_home_current)
        .on_click(move |e: &ClickEvent, _window, cx| navigate_home(is_home_current, e.modifiers(), cx));

        let home_control = if sidebar_collapsed {
            let tip = home_tooltip.clone();
            div().flex_1().flex().items_center().justify_center().child(
                home_button
                    .size_7()
                    .justify_center()
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .child(img(mark).flex_none().size(rems(1.125))),
            )
        } else {
            div().flex_1().min_w_0().flex().items_center().child(
                home_button.w_full().h_7().px_2().justify_start().child(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .w_full()
                        .min_w_0()
                        .overflow_hidden()
                        .child(img(mark).flex_none().size(rems(1.125)))
                        .child(
                            Label::new(SharedString::from("ZEDIS"))
                                .text_sm()
                                .font_semibold()
                                .text_color(label_color)
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .flex_1()
                                .min_w_0(),
                        ),
                ),
            )
        };

        h_flex()
            .id(SIDEBAR_HOME_ROW_ID)
            .mx_2()
            .h(EDITOR_KEY_BAR_HEIGHT)
            .items_center()
            .gap_1()
            .child(home_control)
            .when(!sidebar_collapsed, |this| {
                this.child(
                    Button::new(SIDEBAR_COLLAPSE_TOGGLE_ID)
                        .ghost()
                        .small()
                        .icon(IconName::ChevronLeft)
                        .tooltip(collapse_label)
                        .on_click(move |_, _window, cx| toggle_sidebar_collapsed(cx)),
                )
            })
            .border_b_1()
            .border_color(look.divider_color)
            .into_any_element()
    }
}

struct ServerRowInput<'a> {
    entry: SidebarServerEntry,
    is_current: bool,
    is_menu_open: bool,
    health: ConnectionHealth,
    monogram: SharedString,
    menu: ServerMenu,
    look: &'a ListLook,
}

struct ResolvedServerRow<'a> {
    item_id: SharedString,
    server_id: SharedString,
    name: SharedString,
    tag: SharedString,
    tooltip_text: SharedString,
    monogram: SharedString,
    is_current: bool,
    is_menu_open: bool,
    tag_chip: Option<(Hsla, Hsla)>,
    tag_dot: Option<Hsla>,
    health_color: Option<Hsla>,
    look: &'a ListLook,
}

/// Render a single connection row (expanded pill or collapsed monogram + status dot).
fn server_row(input: ServerRowInput<'_>, window: &mut Window, cx: &mut App) -> AnyElement {
    let ServerRowInput {
        entry,
        is_current,
        is_menu_open,
        health,
        monogram,
        menu,
        look,
    } = input;
    let tooltip_text: SharedString = if entry.tag.is_empty() {
        entry.name.clone()
    } else {
        SharedString::from(format!("{} · {}", entry.name, entry.tag))
    };
    let quiet = is_quiet_tag(entry.tag_color_key.as_deref());
    let tag_chip = (!quiet && !entry.tag.is_empty())
        .then(|| resolve_tag_chip(entry.tag_color_key.as_deref(), look.dark))
        .flatten();
    let tag_dot = (!quiet)
        .then(|| resolve_tag_color(entry.tag_color_key.as_deref()))
        .flatten();
    let health_color = match health {
        ConnectionHealth::Connected => Some(look.connected_color),
        ConnectionHealth::Reconnecting => Some(look.reconnecting_color),
        ConnectionHealth::Offline => Some(look.offline_color),
        ConnectionHealth::Unknown => None,
    };

    let item_id = SharedString::from(format!("sidebar-srv-{}", entry.id));
    let wrap_id = SharedString::from(format!("sidebar-srv-w-{}", entry.id));
    let resolved = ResolvedServerRow {
        item_id,
        server_id: entry.id,
        name: entry.name,
        tag: entry.tag,
        tooltip_text: tooltip_text.clone(),
        monogram,
        is_current,
        is_menu_open,
        tag_chip,
        tag_dot,
        health_color,
        look,
    };
    let server_id = resolved.server_id.clone();
    let button = if look.sidebar_collapsed {
        rail_monogram_button(&resolved, window, cx)
    } else {
        expanded_server_button(&resolved, window, cx)
    }
    .on_click(move |e: &ClickEvent, _window, cx| navigate_server(&server_id, is_current, e.modifiers(), cx));

    let wrap = if look.sidebar_collapsed {
        h_flex().id(wrap_id).w_full().my_0p5().justify_center()
    } else {
        div().id(wrap_id).mx_2()
    };

    wrap.child(button)
        .when(!is_menu_open, |this| {
            let name = tooltip_text;
            let hint = look.new_tab_hint.clone();
            let dup_hint = look.dup_tab_hint.clone();
            this.tooltip(move |window, cx| {
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
            })
        })
        .context_menu(move |popup, window, cx| menu.build(popup, window, cx))
        .into_any_element()
}

/// Build a focusable sidebar row button with concentric radius, surface states, and focus ring.
fn sidebar_focus_button(
    id: impl Into<ElementId>,
    label: SharedString,
    selected: bool,
    open: bool,
    look: &ListLook,
    window: &mut Window,
    cx: &mut App,
) -> BaseButton {
    let id: ElementId = id.into();
    let focus_key: SharedString = format!("{id}-focus").into();
    let focus_handle = window
        .use_keyed_state(focus_key, cx, |_, cx| cx.focus_handle())
        .read(cx)
        .clone();
    let is_focused = focus_handle.is_focused(window);
    let ListLook {
        selected_bg,
        hover_bg,
        pressed_bg,
        radius,
        ..
    } = *look;

    BaseButton::new(id)
        .accessibility_label(label)
        .track_focus(&focus_handle)
        .relative()
        .rounded(radius)
        .when(selected, |this| this.bg(selected_bg))
        .when(!selected && open, |this| this.bg(hover_bg))
        // Register hover/active unconditionally so state transitions never leave stale hover fills.
        .hover(move |s| if selected { s } else { s.bg(hover_bg) })
        .active(move |s| if selected { s } else { s.bg(pressed_bg) })
        .when(is_focused, |this| this.focus_ring_style(window, cx))
}

fn rail_monogram_button(row: &ResolvedServerRow<'_>, window: &mut Window, cx: &mut App) -> BaseButton {
    let look = row.look;
    let icon_color = if row.is_current {
        look.accent_color
    } else {
        look.muted_fg
    };

    sidebar_focus_button(
        row.item_id.clone(),
        row.tooltip_text.clone(),
        row.is_current,
        row.is_menu_open,
        look,
        window,
        cx,
    )
    .aria_selected(row.is_current)
    .size_7()
    .flex_none()
    .justify_center()
    .child(
        Label::new(row.monogram.clone())
            .text_xs()
            .font_family(get_mono_font_family())
            .font_weight(FontWeight::BOLD)
            .text_color(icon_color),
    )
    .when_some(row.health_color.or(row.tag_dot), |this, color| {
        this.child(
            div()
                .absolute()
                .bottom_0()
                .right_0()
                .size(rems(0.5625))
                .rounded_full_style(cx)
                .bg(color)
                .border_2()
                .border_color(look.dot_ring_color),
        )
    })
}

fn expanded_server_button(row: &ResolvedServerRow<'_>, window: &mut Window, cx: &mut App) -> BaseButton {
    let look = row.look;
    let icon_color = if row.is_current {
        look.accent_color
    } else {
        look.muted_fg
    };
    let name_color = if row.is_current { look.selected_fg } else { look.row_fg };

    sidebar_focus_button(
        row.item_id.clone(),
        row.tooltip_text.clone(),
        row.is_current,
        row.is_menu_open,
        look,
        window,
        cx,
    )
    .aria_selected(row.is_current)
    .w_full()
    .h_8()
    .pl_3p5()
    .pr_2()
    .justify_start()
    .child(
        h_flex()
            .items_center()
            .gap_1p5()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .child(
                div()
                    .flex_none()
                    .child(Icon::new(CustomIconName::Database).small().text_color(icon_color)),
            )
            .child(
                Label::new(row.name.clone())
                    .text_xs()
                    .when(row.is_current, |l| l.font_semibold())
                    .text_color(name_color)
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .flex_1()
                    .min_w_0(),
            )
            .when_some(row.tag_chip, |this, (bg, fg)| {
                this.child(
                    div()
                        .flex_none()
                        .px_1()
                        .py_px()
                        .rounded(look.radius * 0.5)
                        .bg(bg)
                        .child(
                            Label::new(row.tag.clone())
                                .text_xs()
                                .font_semibold()
                                .line_height(relative(1.15))
                                .text_color(fg),
                        ),
                )
            })
            .when_some(row.health_color, |this, color| {
                this.child(div().flex_none().size(rems(0.4375)).rounded_full_style(cx).bg(color))
            }),
    )
}

fn navigate_home(is_home_current: bool, mods: Modifiers, cx: &mut App) {
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
}

fn navigate_server(server_id: &SharedString, is_current: bool, mods: Modifiers, cx: &mut App) {
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
}

fn toggle_sidebar_collapsed(cx: &mut App) {
    update_app_state_and_save(cx, "toggle_sidebar_collapsed", |state, _| {
        state.toggle_sidebar_collapsed();
    });
}

/// Shared visual tokens computed once per sidebar render pass.
struct ListLook {
    sidebar_collapsed: bool,
    new_tab_hint: SharedString,
    dup_tab_hint: SharedString,
    selected_bg: Hsla,
    selected_fg: Hsla,
    hover_bg: Hsla,
    pressed_bg: Hsla,
    row_fg: Hsla,
    muted_fg: Hsla,
    divider_color: Hsla,
    accent_color: Hsla,
    connected_color: Hsla,
    reconnecting_color: Hsla,
    offline_color: Hsla,
    dot_ring_color: Hsla,
    dark: bool,
    radius: Pixels,
}

fn list_look(cx: &App) -> ListLook {
    let store = cx.global::<ZedisGlobalStore>().read(cx);
    let locale = store.locale().to_string();
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

    let theme = cx.theme();
    let dark = theme.is_dark();
    let selected_bg = theme.sidebar_accent;
    ListLook {
        sidebar_collapsed: store.sidebar_collapsed(),
        new_tab_hint,
        dup_tab_hint,
        selected_bg,
        selected_fg: theme.sidebar_accent_foreground,
        hover_bg: theme.list_hover,
        pressed_bg: selected_bg.opacity(0.8),
        row_fg: theme.sidebar_foreground,
        muted_fg: theme.muted_foreground,
        divider_color: theme.sidebar_border,
        accent_color: sidebar_accent_color(dark),
        connected_color: healthy_color(dark),
        reconnecting_color: theme.warning,
        offline_color: theme.danger,
        dot_ring_color: theme.sidebar,
        dark,
        radius: theme.radius,
    }
}

/// Group section header aligned with the Home brand spine, keyboard-accessible.
fn group_header(
    section: &SidebarSection,
    is_collapsed: bool,
    ungrouped_label: &SharedString,
    look: &ListLook,
    window: &mut Window,
    cx: &mut App,
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
    let mono: SharedString = get_mono_font_family().into();

    sidebar_focus_button(header_id, header_label.clone(), false, false, look, window, cx)
        .aria_expanded(!is_collapsed)
        .mx_2()
        .mt_1()
        .px_2()
        .h_6()
        .gap_1p5()
        .justify_start()
        .child(Icon::new(chevron).xsmall().text_color(look.muted_fg))
        .child(
            Label::new(header_label)
                .text_xs()
                .text_color(look.muted_fg)
                .whitespace_nowrap()
                .text_ellipsis()
                .flex_1()
                .min_w_0(),
        )
        .when(is_collapsed, |this| {
            this.child(
                Label::new(count_label)
                    .text_xs()
                    .font_family(mono)
                    .text_color(look.muted_fg),
            )
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
        let sidebar_collapsed = cx.global::<ZedisGlobalStore>().read(cx).sidebar_collapsed();
        let toggle_at_the_bottom = sidebar_collapsed || !home_lives_in_the_sidebar();
        let toggle_tooltip = if sidebar_collapsed {
            i18n_sidebar(cx, "expand")
        } else {
            i18n_sidebar(cx, "collapse")
        };
        let theme = cx.theme();
        let sidebar_bg = theme.sidebar;
        let sidebar_border = theme.sidebar_border;

        v_flex()
            .size_full()
            .id(SIDEBAR_CONTAINER_ID)
            .test_support()
            .justify_start()
            .bg(sidebar_bg)
            .border_r_1()
            .border_color(sidebar_border)
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
                        .h(STATUS_BAR_HEIGHT)
                        .items_center()
                        .justify_center()
                        .child(
                            Button::new(SIDEBAR_RAIL_TOGGLE_ID)
                                .ghost()
                                .small()
                                .icon(if sidebar_collapsed {
                                    IconName::ChevronRight
                                } else {
                                    IconName::ChevronLeft
                                })
                                .tooltip(toggle_tooltip)
                                .on_click(move |_, _window, cx| toggle_sidebar_collapsed(cx)),
                        ),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::states::ServerView;
    use crate::test_support::init_ui;
    use gpui::{AnyWindowHandle, Entity, MouseButton, TestAppContext, point, px, size};
    use gpui_kit::component::Root;
    use gpui_kit::test::{TestEventExt, TestWindowExt};
    use std::cell::RefCell;
    use std::rc::Rc;

    type Sent = Rc<RefCell<Vec<crate::helpers::SidebarServerAction>>>;

    /// The sidebar inside a view that captures every `SidebarServerAction` so
    /// menu dispatch is verified without mutating real server configs.
    struct SidebarHarness {
        sidebar: Entity<ZedisSidebar>,
        sent: Sent,
    }

    impl Render for SidebarHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let sent = self.sent.clone();
            div()
                .size_full()
                .capture_action(move |action: &crate::helpers::SidebarServerAction, _, cx| {
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
        open_sidebar_with_size(cx, px(280.), px(640.))
    }

    fn open_sidebar_with_size(
        cx: &mut TestAppContext,
        width: Pixels,
        height: Pixels,
    ) -> (AnyWindowHandle, Entity<ZedisSidebar>, Sent) {
        let sent = Sent::default();
        let mut sidebar = None;
        let handle = cx.open_window(size(width, height), |window, cx| {
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

    fn menu_items(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        sent: &Sent,
        row: &'static str,
    ) -> Vec<Option<crate::helpers::SidebarServerAction>> {
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

    fn assert_inset_for_focus_ring(window: &gpui::Window, id: &'static str, container: gpui::Bounds<Pixels>) {
        let bounds = window.find(id).bounds();
        let ring = px(3.);
        assert!(
            bounds.left() - container.left() >= ring,
            "{id} left edge clips focus ring: {bounds:?} vs {container:?}"
        );
        assert!(
            container.right() - bounds.right() >= ring,
            "{id} right edge clips focus ring: {bounds:?} vs {container:?}"
        );
        assert!(
            bounds.top() - container.top() >= ring,
            "{id} top edge clips focus ring: {bounds:?} vs {container:?}"
        );
        assert!(
            container.bottom() - bounds.bottom() >= ring,
            "{id} bottom edge clips focus ring: {bounds:?} vs {container:?}"
        );
    }

    #[gpui::test]
    fn the_list_draws_home_its_rows_and_closes_a_group(cx: &mut TestAppContext) {
        init_ui(cx);
        let (handle, sidebar, _) = open_sidebar(cx);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            for id in [
                SIDEBAR_HOME_ITEM_ID,
                SIDEBAR_COLLAPSE_TOGGLE_ID,
                "sidebar-srv-a",
                "sidebar-srv-b",
                "sidebar-srv-c",
            ] {
                assert!(window.try_find(id).is_some(), "{id}");
            }
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
            assert!(window.try_find(SIDEBAR_COLLAPSE_TOGGLE_ID).is_none());
            for id in [SIDEBAR_HOME_ITEM_ID, "sidebar-srv-a", "sidebar-srv-b", "sidebar-srv-c"] {
                assert!(window.try_find(id).is_some(), "{id} in the rail");
            }
        })
        .expect("the window is open");
    }

    #[gpui::test]
    fn a_row_opens_its_connection_and_its_menu_follows_the_link(cx: &mut TestAppContext) {
        use crate::helpers::SidebarServerAction as A;
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
            window.click_with_modifiers(SIDEBAR_HOME_ITEM_ID, secondary, cx);
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

    #[gpui::test]
    fn home_and_server_rows_reflect_selection_and_group_expanded_state(cx: &mut TestAppContext) {
        init_ui(cx);
        let (handle, _, _) = open_sidebar(cx);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find(SIDEBAR_HOME_ITEM_ID).selected(), Some(true));
            assert_ne!(window.find("sidebar-srv-a").selected(), Some(true));
            assert_eq!(window.find("sidebar-grp-h-prod").expanded(), Some(true));
            window.click("sidebar-srv-a", cx);
        })
        .expect("the window is open");
        cx.run_until_parked();

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find(SIDEBAR_HOME_ITEM_ID).selected(), Some(false));
            assert_eq!(window.find("sidebar-srv-a").selected(), Some(true));
            window.click("sidebar-grp-h-prod", cx);
        })
        .expect("the window is open");
        cx.run_until_parked();

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("sidebar-grp-h-prod").expanded(), Some(false));
            assert!(window.try_find("sidebar-srv-a").is_none());
        })
        .expect("the window is open");
    }

    #[gpui::test]
    fn rail_toggle_carries_expand_tooltip_and_focus_rings_fit_unclipped(cx: &mut TestAppContext) {
        init_ui(cx);
        let (handle, _, _) = open_sidebar_with_size(cx, px(180.), px(640.));
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let container = window.find(SIDEBAR_CONTAINER_ID).bounds();
            for id in [
                SIDEBAR_HOME_ITEM_ID,
                SIDEBAR_COLLAPSE_TOGGLE_ID,
                "sidebar-grp-h-prod",
                "sidebar-srv-a",
                "sidebar-srv-b",
                "sidebar-srv-c",
            ] {
                assert_inset_for_focus_ring(window, id, container);
            }
        })
        .expect("the window is open");

        let (rail_handle, rail_sidebar, _) = open_sidebar_with_size(cx, px(52.), px(640.));
        cx.update(|cx| {
            cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
                state.toggle_sidebar_collapsed();
                cx.notify();
            });
        });
        rail_sidebar.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        cx.update_window(rail_handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.find(SIDEBAR_RAIL_TOGGLE_ID).description().is_some(),
                "collapsed rail expand button must expose an expand tooltip"
            );
            let container = window.find(SIDEBAR_CONTAINER_ID).bounds();
            for id in [
                SIDEBAR_HOME_ITEM_ID,
                "sidebar-srv-a",
                "sidebar-srv-b",
                "sidebar-srv-c",
                SIDEBAR_RAIL_TOGGLE_ID,
            ] {
                assert_inset_for_focus_ring(window, id, container);
            }
        })
        .expect("the window is open");
    }

    #[gpui::test]
    fn keyboard_activation_and_empty_state(cx: &mut TestAppContext) {
        init_ui(cx);
        let (handle, sidebar, _) = open_sidebar(cx);

        // Empty list renders quiet empty hint only when expanded.
        sidebar.update(cx, |sidebar, cx| {
            sidebar.state.sections.clear();
            cx.notify();
        });
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find(SIDEBAR_EMPTY_ID).visible());
        })
        .expect("the window is open");

        sidebar.update(cx, |sidebar, cx| {
            sidebar.state.sections = sections();
            cx.notify();
        });
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(SIDEBAR_EMPTY_ID).is_none());
            // Focus the first control inside `Root` so `Tab` traversal starts from the focus chain.
            window.click(SIDEBAR_HOME_ITEM_ID, cx);
            window.render_frame(cx);
            assert_eq!(window.find(SIDEBAR_HOME_ITEM_ID).focused(), Some(true));
        })
        .expect("the window is open");
        cx.run_until_parked();

        for _ in 0..8 {
            let hit = cx
                .update_window(handle, |_, window, cx| {
                    window.press("tab", cx);
                    window.find("sidebar-srv-b").focused() == Some(true)
                })
                .expect("the window is open");
            cx.run_until_parked();
            if hit {
                break;
            }
        }
        cx.update_window(handle, |_, window, cx| {
            assert_eq!(window.find("sidebar-srv-b").focused(), Some(true));
            window.press("enter", cx);
        })
        .expect("the window is open");
        cx.run_until_parked();

        cx.read(|cx| {
            let state = cx.global::<ZedisGlobalStore>().read(cx);
            assert_eq!(state.selected_server(), Some(&("b".to_string(), 0)));
        });
    }
}
