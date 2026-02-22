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
    helpers::is_development,
    states::{
        FontSize, FontSizeAction, GlobalEvent, LocaleAction, Route, SettingsAction, ThemeAction, ZedisGlobalStore,
        i18n_sidebar,
    },
};
use gpui::{Context, Corner, Pixels, SharedString, Subscription, Window, div, prelude::*, px, uniform_list};
use gpui_component::{
    ActiveTheme, Icon, IconName, ThemeMode,
    button::{Button, ButtonVariants},
    label::Label,
    list::ListItem,
    menu::DropdownMenu,
    v_flex,
};
use tracing::info;

// Constants for UI layout
const ICON_PADDING: Pixels = px(8.0);
const ICON_MARGIN: Pixels = px(4.0);
const LABEL_PADDING: Pixels = px(2.0);
const STAR_BUTTON_HEIGHT: f32 = 48.0;
const SETTINGS_BUTTON_HEIGHT: f32 = 44.0;
const SERVER_LIST_ITEM_BORDER_WIDTH: f32 = 3.0;
const SETTINGS_ICON_SIZE: f32 = 18.0;

/// Internal state for sidebar component
///
/// Caches server list to avoid repeated queries and tracks current selection.
#[derive(Default)]
struct SidebarState {
    /// List of (server_id, server_name) tuples for display
    /// First entry is always (empty, empty) representing the home page
    server_names: Vec<(SharedString, SharedString)>,

    /// Currently selected server ID (empty string means home page)
    server_id: SharedString,
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

    /// Update cached server list from server state
    ///
    /// Rebuilds the server_names list with:
    /// - First entry: (empty, empty) for home page
    /// - Remaining entries: (server_id, server_name) for each configured server
    fn update_server_names(&mut self, _cx: &mut Context<Self>) {
        // Start with home page entry
        let mut server_names = vec![(SharedString::default(), SharedString::default())];

        if let Ok(servers) = get_servers() {
            server_names.extend(
                servers
                    .iter()
                    .map(|server| (server.id.clone().into(), server.name.clone().into())),
            );
            self.state.server_names = server_names;
        }
    }

    /// Render the scrollable server list
    ///
    /// Shows:
    /// - Home page item (always first)
    /// - All configured server items
    ///
    /// Current selection is highlighted with background color and border.
    /// Clicking an item navigates to that server or home page.
    fn render_server_list(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let servers = self.state.server_names.clone();
        let current_server_id_clone = self.state.server_id.clone();
        let is_match_route = matches!(
            cx.global::<ZedisGlobalStore>().read(cx).route(),
            Route::Home | Route::Editor
        );

        let home_label = i18n_sidebar(cx, "home");
        let list_active_color = cx.theme().list_active;
        let list_active_border_color = cx.theme().list_active_border;

        uniform_list("sidebar-redis-servers", servers.len(), move |range, _window, _cx| {
            range
                .map(|index| {
                    let (server_id, server_name) = servers.get(index).cloned().unwrap_or_default();

                    let is_home = server_id.is_empty();
                    let is_current = is_match_route && server_id == current_server_id_clone;

                    // Display "Home" for empty server_name, otherwise use server name
                    let name = if server_name.is_empty() {
                        home_label.clone()
                    } else {
                        server_name.clone()
                    };

                    ListItem::new(("sidebar-redis-server", index))
                        .w_full()
                        .when(is_current, |this| this.bg(list_active_color))
                        .py_4()
                        .border_r(px(SERVER_LIST_ITEM_BORDER_WIDTH))
                        .when(is_current, |this| this.border_color(list_active_border_color))
                        .child(
                            v_flex()
                                .items_center()
                                .child(Icon::new(IconName::LayoutDashboard))
                                .child(Label::new(name).text_ellipsis().text_xs()),
                        )
                        .on_click(move |_, _window, cx| {
                            // Don't do anything if already selected
                            if is_current {
                                return;
                            }

                            // Determine target route based on home/server
                            let route = if is_home { Route::Home } else { Route::Editor };

                            // Update global route
                            cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                store.update(cx, |state, cx| {
                                    state.go_to(route, cx);
                                    state.set_selected_server((server_id.to_string(), 0), cx);
                                });
                            });
                        })
                })
                .collect()
        })
        .size_full()
    }

    /// Render settings button with dropdown menu
    ///
    /// The dropdown contains two submenus:
    /// 1. Theme selection (Light/Dark/System)
    /// 2. Language selection (English/Chinese)
    ///
    /// Changes are saved to disk and applied immediately across all windows.
    fn render_settings_button(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let store = cx.global::<ZedisGlobalStore>().read(cx);

        // Determine currently selected theme mode
        let current_action = match store.theme() {
            Some(ThemeMode::Light) => ThemeAction::Light,
            Some(ThemeMode::Dark) => ThemeAction::Dark,
            _ => ThemeAction::System,
        };

        // Determine currently selected locale
        let locale = store.locale();
        let current_locale = match locale {
            "zh" => LocaleAction::Zh,
            _ => LocaleAction::En,
        };
        let current_font_size = store.font_size();

        let btn = Button::new("zedis-sidebar-setting-btn")
            .ghost()
            .w_full()
            .h(px(SETTINGS_BUTTON_HEIGHT))
            .tooltip(i18n_sidebar(cx, "settings"))
            .child(Icon::new(IconName::Settings).size(px(SETTINGS_ICON_SIZE)))
            .dropdown_menu_with_anchor(Corner::BottomRight, move |menu, window, cx| {
                let theme_text = i18n_sidebar(cx, "theme");
                let lang_text = i18n_sidebar(cx, "lang");
                let font_size_text = i18n_sidebar(cx, "font_size");

                // Theme submenu with light/dark/system options
                menu.submenu_with_icon(
                    Some(Icon::new(IconName::Sun).px(ICON_PADDING).mr(ICON_MARGIN)),
                    theme_text,
                    window,
                    cx,
                    move |submenu, _window, _cx| {
                        submenu
                            .menu_element_with_check(
                                current_action == ThemeAction::Light,
                                Box::new(ThemeAction::Light),
                                |_window, cx| Label::new(i18n_sidebar(cx, "light")).text_xs().p(LABEL_PADDING),
                            )
                            .menu_element_with_check(
                                current_action == ThemeAction::Dark,
                                Box::new(ThemeAction::Dark),
                                |_window, cx| Label::new(i18n_sidebar(cx, "dark")).text_xs().p(LABEL_PADDING),
                            )
                            .menu_element_with_check(
                                current_action == ThemeAction::System,
                                Box::new(ThemeAction::System),
                                |_window, cx| Label::new(i18n_sidebar(cx, "system")).text_xs().p(LABEL_PADDING),
                            )
                    },
                )
                // Language submenu with Chinese/English options
                .submenu_with_icon(
                    Some(Icon::new(CustomIconName::Languages).px(ICON_PADDING).mr(ICON_MARGIN)),
                    lang_text,
                    window,
                    cx,
                    move |submenu, _window, _cx| {
                        submenu
                            .menu_element_with_check(
                                current_locale == LocaleAction::Zh,
                                Box::new(LocaleAction::Zh),
                                |_window, _cx| Label::new("中文").text_xs().p(LABEL_PADDING),
                            )
                            .menu_element_with_check(
                                current_locale == LocaleAction::En,
                                Box::new(LocaleAction::En),
                                |_window, _cx| Label::new("English").text_xs().p(LABEL_PADDING),
                            )
                    },
                )
                .submenu_with_icon(
                    Some(Icon::new(CustomIconName::ALargeSmall).px(ICON_PADDING).mr(ICON_MARGIN)),
                    font_size_text,
                    window,
                    cx,
                    move |submenu, _window, _cx| {
                        submenu
                            .menu_element_with_check(
                                current_font_size == FontSize::Large,
                                Box::new(FontSizeAction::Large),
                                move |_window, cx| {
                                    let text = i18n_sidebar(cx, "font_size_large");
                                    Label::new(text).text_xs().p(LABEL_PADDING)
                                },
                            )
                            .menu_element_with_check(
                                current_font_size == FontSize::Medium,
                                Box::new(FontSizeAction::Medium),
                                move |_window, cx| {
                                    let text = i18n_sidebar(cx, "font_size_medium");
                                    Label::new(text).text_xs().p(LABEL_PADDING)
                                },
                            )
                            .menu_element_with_check(
                                current_font_size == FontSize::Small,
                                Box::new(FontSizeAction::Small),
                                move |_window, cx| {
                                    let text = i18n_sidebar(cx, "font_size_small");
                                    Label::new(text).text_xs().p(LABEL_PADDING)
                                },
                            )
                    },
                )
                .menu_element_with_icon(
                    Icon::new(IconName::Settings2),
                    Box::new(SettingsAction::Editor),
                    move |_window, cx| Label::new(i18n_sidebar(cx, "other_settings")).p(LABEL_PADDING),
                )
            });
        div().border_t_1().border_color(cx.theme().border).child(btn)
    }

    /// Render GitHub star button (link to repository)
    fn render_star(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().border_b_1().border_color(cx.theme().border).child(
            Button::new("github")
                .ghost()
                .h(px(STAR_BUTTON_HEIGHT))
                .w_full()
                .tooltip(i18n_sidebar(cx, "star"))
                .child(
                    v_flex()
                        .items_center()
                        .justify_center()
                        .child(Icon::new(IconName::GitHub))
                        .child(Label::new("ZEDIS").text_xs()),
                )
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.open_url("https://github.com/vicanso/zedis");
                })),
        )
    }
}

impl Render for ZedisSidebar {
    /// Main render method - displays vertical sidebar with navigation and settings
    ///
    /// Layout structure (top to bottom):
    /// 1. GitHub star button
    /// 2. Server list (scrollable, takes remaining space)
    /// 3. Settings button (theme & language)
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let show_settings_button = is_development();

        v_flex()
            .size_full()
            .id("sidebar-container")
            .justify_start()
            .border_r_1()
            .border_color(cx.theme().border)
            .when(show_settings_button, |this| this.child(self.render_star(window, cx)))
            .child(
                // Server list takes up remaining vertical space
                div().flex_1().size_full().child(self.render_server_list(window, cx)),
            )
            .when(show_settings_button, |this| {
                this.child(self.render_settings_button(window, cx))
            })
    }
}
