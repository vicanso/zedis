// Copyright 2025 Tree xie.
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

use crate::assets::CustomIconName;
use crate::constants::SIDEBAR_WIDTH;
use crate::states::Route;
use crate::states::ServerEvent;
use crate::states::ZedisAppState;
use crate::states::ZedisGlobalStore;
use crate::states::ZedisServerState;
use crate::states::i18n_sidebar;
use crate::states::save_app_state;
use gpui::Action;
use gpui::Context;
use gpui::Corner;
use gpui::Entity;
use gpui::Pixels;
use gpui::SharedString;
use gpui::Subscription;
use gpui::Window;
use gpui::WindowAppearance;
use gpui::div;
use gpui::prelude::*;
use gpui::px;
use gpui::uniform_list;
use gpui_component::ActiveTheme;
use gpui_component::Icon;
use gpui_component::IconName;
use gpui_component::Theme;
use gpui_component::ThemeMode;
use gpui_component::button::Button;
use gpui_component::button::ButtonVariants;
use gpui_component::label::Label;
use gpui_component::list::ListItem;
use gpui_component::menu::DropdownMenu;
use gpui_component::v_flex;
use schemars::JsonSchema;
use serde::Deserialize;
use tracing::error;
use tracing::info;

// Constants for UI layout
const ICON_PADDING: Pixels = px(8.0);
const ICON_MARGIN: Pixels = px(4.0);
const LABEL_PADDING: Pixels = px(2.0);
const STAR_BUTTON_HEIGHT: f32 = 48.0;
const SETTINGS_BUTTON_HEIGHT: f32 = 44.0;
const SERVER_LIST_ITEM_BORDER_WIDTH: f32 = 3.0;
const SETTINGS_ICON_SIZE: f32 = 18.0;

/// Theme selection actions for the settings menu
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
enum ThemeAction {
    /// Light theme mode
    Light,
    /// Dark theme mode
    Dark,
    /// Follow system theme
    System,
}

/// Locale/language selection actions for the settings menu
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
enum LocaleAction {
    /// English language
    En,
    /// Chinese language
    Zh,
}

/// Update app state in background, persist to disk, and refresh UI
///
/// This helper function abstracts the common pattern for updating global state:
/// 1. Apply mutation to app state
/// 2. Save updated state to disk asynchronously
/// 3. Refresh all windows to apply changes
///
/// Used for theme and locale changes to ensure consistency across the app.
///
/// # Arguments
/// * `cx` - Context for spawning async tasks
/// * `action_name` - Human-readable action name for logging
/// * `mutation` - Callback to modify the app state
#[inline]
fn update_app_state_and_save<F>(cx: &mut Context<ZedisSidebar>, action_name: &'static str, mutation: F)
where
    F: FnOnce(&mut ZedisAppState, &mut Context<ZedisAppState>) + Send + 'static + Clone,
{
    let store = cx.global::<ZedisGlobalStore>().clone();

    cx.spawn(async move |_, cx| {
        // Step 1: Update global state with the mutation
        let current_state = store.update(cx, |state, cx| {
            mutation(state, cx);
            state.clone() // Return clone for async persistence
        });

        // Step 2: Persist to disk in background executor
        if let Ok(state) = current_state {
            cx.background_executor()
                .spawn(async move {
                    if let Err(e) = save_app_state(&state) {
                        error!(error = %e, action = action_name, "Failed to save state");
                    } else {
                        info!(action = action_name, "State saved successfully");
                    }
                })
                .await;
        }

        // Step 3: Refresh windows to apply visual changes (theme/locale)
        cx.update(|cx| cx.refresh_windows()).ok();
    })
    .detach();
}

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

    /// Reference to server state for Redis operations
    server_state: Entity<ZedisServerState>,

    /// Event subscriptions for reactive updates
    _subscriptions: Vec<Subscription>,
}

impl ZedisSidebar {
    /// Create a new sidebar component with event subscriptions
    ///
    /// Sets up listeners for:
    /// - Server selection changes (updates current selection)
    /// - Server list updates (refreshes displayed servers)
    pub fn new(server_state: Entity<ZedisServerState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = vec![];

        // Subscribe to server events for reactive updates
        subscriptions.push(cx.subscribe(&server_state, |this, _server_state, event, cx| {
            match event {
                ServerEvent::ServerSelected(server_id) => {
                    // Update current selection highlight
                    this.state.server_id = server_id.clone();
                }
                ServerEvent::ServerListUpdated => {
                    // Refresh server list when servers are added/removed/updated
                    this.update_server_names(cx);
                }
                _ => {
                    return;
                }
            }
            cx.notify();
        }));

        // Get current server ID for initial selection
        let state = server_state.read(cx).clone();
        let server_id = state.server_id().to_string().into();

        let mut this = Self {
            server_state,
            state: SidebarState {
                server_id,
                ..Default::default()
            },
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
    fn update_server_names(&mut self, cx: &mut Context<Self>) {
        // Start with home page entry
        let mut server_names = vec![(SharedString::default(), SharedString::default())];

        let server_state = self.server_state.read(cx);
        if let Some(servers) = server_state.servers() {
            server_names.extend(
                servers
                    .iter()
                    .map(|server| (server.id.clone().into(), server.name.clone().into())),
            );
        }
        self.state.server_names = server_names;
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
        let view = cx.entity();
        let servers = self.state.server_names.clone();
        let current_server_id_clone = self.state.server_id.clone();
        let home_label = i18n_sidebar(cx, "home");
        let list_active_color = cx.theme().list_active;
        let list_active_border_color = cx.theme().list_active_border;

        uniform_list("sidebar-redis-servers", servers.len(), move |range, _window, _cx| {
            range
                .map(|index| {
                    let (server_id, server_name) = servers.get(index).cloned().unwrap_or_default();

                    let is_home = server_id.is_empty();
                    let is_current = server_id == current_server_id_clone;

                    // Display "Home" for empty server_name, otherwise use server name
                    let name = if server_name.is_empty() {
                        home_label.clone()
                    } else {
                        server_name.clone()
                    };

                    let view = view.clone();

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

                            view.update(cx, |this, cx| {
                                // Update global route
                                cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                    store.update(cx, |state, cx| {
                                        state.go_to(route, cx);
                                    });
                                });

                                this.server_state.update(cx, |state, cx| {
                                    state.select(server_id.clone(), cx);
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
        let store = cx.global::<ZedisGlobalStore>();

        // Determine currently selected theme mode
        let current_action = match store.theme(cx) {
            Some(ThemeMode::Light) => ThemeAction::Light,
            Some(ThemeMode::Dark) => ThemeAction::Dark,
            _ => ThemeAction::System,
        };

        // Determine currently selected locale
        let locale = store.locale(cx);
        let current_locale = match locale {
            "zh" => LocaleAction::Zh,
            _ => LocaleAction::En,
        };

        let btn = Button::new("zedis-sidebar-setting-btn")
            .ghost()
            .w_full()
            .h(px(SETTINGS_BUTTON_HEIGHT))
            .tooltip(i18n_sidebar(cx, "settings"))
            .child(Icon::new(IconName::Settings).size(px(SETTINGS_ICON_SIZE)))
            .dropdown_menu_with_anchor(Corner::BottomRight, move |menu, window, cx| {
                let theme_text = i18n_sidebar(cx, "theme");
                let lang_text = i18n_sidebar(cx, "lang");

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
            });

        div()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(btn)
            // Theme action handler - applies theme and saves to disk
            .on_action(cx.listener(|_this, e: &ThemeAction, _window, cx| {
                let action = *e;

                // Convert action to theme mode
                let mode = match action {
                    ThemeAction::Light => Some(ThemeMode::Light),
                    ThemeAction::Dark => Some(ThemeMode::Dark),
                    ThemeAction::System => None, // Follow OS theme
                };

                // Determine actual render mode (resolve System to Light/Dark)
                let render_mode = match mode {
                    Some(m) => m,
                    None => match cx.window_appearance() {
                        WindowAppearance::Light => ThemeMode::Light,
                        _ => ThemeMode::Dark,
                    },
                };

                // Apply theme immediately for instant visual feedback
                Theme::change(render_mode, None, cx);

                // Save preference to disk asynchronously
                update_app_state_and_save(cx, "save_theme", move |state, _cx| {
                    state.set_theme(mode);
                });
            }))
            // Locale action handler - changes language and saves to disk
            .on_action(cx.listener(|_this, e: &LocaleAction, _window, cx| {
                let locale = match e {
                    LocaleAction::Zh => "zh",
                    LocaleAction::En => "en",
                };

                // Save locale preference and refresh UI
                update_app_state_and_save(cx, "save_locale", move |state, _cx| {
                    state.set_locale(locale.to_string());
                });
            }))
    }

    /// Render GitHub star button (link to repository)
    fn render_star(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().border_b_1().border_color(cx.theme().border).child(
            Button::new("github")
                .ghost()
                .h(px(STAR_BUTTON_HEIGHT))
                .w_full()
                .tooltip(i18n_sidebar(cx, "star"))
                .icon(Icon::new(IconName::GitHub))
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
        tracing::debug!("Rendering sidebar view");

        v_flex()
            .w(px(SIDEBAR_WIDTH))
            .id("sidebar-container")
            .justify_start()
            .h_full()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(self.render_star(window, cx))
            .child(
                // Server list takes up remaining vertical space
                div().flex_1().size_full().child(self.render_server_list(window, cx)),
            )
            .child(self.render_settings_button(window, cx))
    }
}
