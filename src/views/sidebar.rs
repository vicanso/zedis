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

#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
enum ThemeAction {
    Light,
    Dark,
    System,
}

#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
enum LocaleAction {
    En,
    Zh,
}

/// Updates AppState in the background, persists it, and refreshes the UI.
/// This helper reduces code duplication for Theme and Locale switching.
#[inline]
fn update_app_state_and_save<F>(
    cx: &mut Context<ZedisSidebar>,
    action_name: &'static str,
    mutation: F,
) where
    F: FnOnce(&mut ZedisAppState, &mut Context<ZedisAppState>) + Send + 'static + Clone,
{
    // 1. Get Global Store handle (Clone to move into async block)
    let store = cx.global::<ZedisGlobalStore>().clone();

    cx.spawn(async move |_, cx| {
        // 1. Update Global State
        // Using AsyncContext to update Entity
        let current_state = store.update(cx, |state, cx| {
            // Execute specific mutation logic (fn mutation)
            mutation(state, cx);

            // Return a clone of the current state for persistence
            state.clone()
        });

        // 2. Save to Disk, background task
        if let Ok(state) = current_state {
            cx.background_executor()
                .spawn(async move {
                    if let Err(e) = save_app_state(&state) {
                        error!(error = %e, action = action_name, "save state failed");
                    } else {
                        info!(action = action_name, "save state success");
                    }
                })
                .await;
        }

        // 3. Refresh Windows (Apply Theme/Locale changes globally)
        cx.update(|cx| cx.refresh_windows()).ok();
    })
    .detach();
}

const ICON_PADDING: Pixels = px(8.);
const ICON_MARGIN: Pixels = px(4.);
const LABEL_PADDING: Pixels = px(2.);

#[derive(Default)]
struct SidebarState {
    server_names: Vec<(SharedString, SharedString)>,
    server_id: SharedString,
}

pub struct ZedisSidebar {
    state: SidebarState,
    server_state: Entity<ZedisServerState>,
    _subscriptions: Vec<Subscription>,
}
impl ZedisSidebar {
    pub fn new(
        _window: &mut Window,
        cx: &mut Context<Self>,
        server_state: Entity<ZedisServerState>,
    ) -> Self {
        let mut subscriptions = vec![];
        subscriptions.push(
            cx.subscribe(&server_state, |this, _server_state, event, cx| {
                match event {
                    ServerEvent::SelectServer(server_id) => {
                        this.state.server_id = server_id.clone();
                    }
                    ServerEvent::UpdateServers => {
                        this.update_server_names(cx);
                    }
                    _ => {
                        return;
                    }
                }
                cx.notify();
            }),
        );
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

        this.update_server_names(cx);
        this
    }
    /// Update the server names
    fn update_server_names(&mut self, cx: &mut Context<Self>) {
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
    /// Render the server list
    fn render_server_list(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let servers = self.state.server_names.clone();
        let current_server_id_clone = self.state.server_id.clone();
        let home_label = i18n_sidebar(cx, "home");
        let list_active_color = cx.theme().list_active;
        let list_active_border_color = cx.theme().list_active_border;
        uniform_list(
            "sidebar-redis-servers",
            servers.len(),
            move |range, _window, _cx| {
                range
                    .map(|index| {
                        let (server_id, server_name) =
                            servers.get(index).cloned().unwrap_or_default();
                        let is_home = server_id.is_empty();
                        let is_current = server_id == current_server_id_clone;
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
                            .border_r_3()
                            .when(is_current, |this| {
                                this.border_color(list_active_border_color)
                            })
                            .child(
                                v_flex()
                                    .items_center()
                                    .child(Icon::new(IconName::LayoutDashboard))
                                    .child(Label::new(name).text_ellipsis().text_xs()),
                            )
                            .on_click(move |_, _window, cx| {
                                if is_current {
                                    return;
                                }
                                let route = if is_home { Route::Home } else { Route::Editor };
                                view.update(cx, |this, cx| {
                                    cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                        store.update(cx, |state, _cx| {
                                            state.go_to(route);
                                        });
                                        cx.notify();
                                    });
                                    let query_mode = cx
                                        .global::<ZedisGlobalStore>()
                                        .query_mode(server_id.as_str(), cx);
                                    this.server_state.update(cx, |state, cx| {
                                        state.select(server_id.clone(), query_mode, cx);
                                    });
                                });
                            })
                    })
                    .collect()
            },
        )
        .size_full()
    }
    /// Render the settings button
    fn render_settings_button(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let store = cx.global::<ZedisGlobalStore>();

        let current_action = match store.theme(cx) {
            Some(ThemeMode::Light) => ThemeAction::Light,
            Some(ThemeMode::Dark) => ThemeAction::Dark,
            _ => ThemeAction::System,
        };
        let locale = store.locale(cx);
        let current_locale = match locale {
            "zh" => LocaleAction::Zh,
            _ => LocaleAction::En,
        };

        let btn = Button::new("zedis-sidebar-setting-btn")
            .ghost()
            .w_full()
            .h(px(44.))
            .tooltip(i18n_sidebar(cx, "settings"))
            .child(Icon::new(IconName::Settings).size(px(18.)))
            .dropdown_menu_with_anchor(Corner::BottomRight, move |menu, window, cx| {
                let theme_text = i18n_sidebar(cx, "theme");
                let lang_text = i18n_sidebar(cx, "lang");
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
                                |_window, cx| {
                                    Label::new(i18n_sidebar(cx, "light"))
                                        .text_xs()
                                        .p(LABEL_PADDING)
                                },
                            )
                            .menu_element_with_check(
                                current_action == ThemeAction::Dark,
                                Box::new(ThemeAction::Dark),
                                |_window, cx| {
                                    Label::new(i18n_sidebar(cx, "dark"))
                                        .text_xs()
                                        .p(LABEL_PADDING)
                                },
                            )
                            .menu_element_with_check(
                                current_action == ThemeAction::System,
                                Box::new(ThemeAction::System),
                                |_window, cx| {
                                    Label::new(i18n_sidebar(cx, "system"))
                                        .text_xs()
                                        .p(LABEL_PADDING)
                                },
                            )
                    },
                )
                .submenu_with_icon(
                    Some(
                        Icon::new(CustomIconName::Languages)
                            .px(ICON_PADDING)
                            .mr(ICON_MARGIN),
                    ),
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
            .on_action(cx.listener(|_this, e: &ThemeAction, _window, cx| {
                let action = *e;

                // 1. Apply Theme change immediately (visual feedback)
                let mode = match action {
                    ThemeAction::Light => Some(ThemeMode::Light),
                    ThemeAction::Dark => Some(ThemeMode::Dark),
                    ThemeAction::System => None,
                };

                // Calculate the actual mode used for rendering
                let render_mode = match mode {
                    Some(m) => m,
                    None => match cx.window_appearance() {
                        WindowAppearance::Light => ThemeMode::Light,
                        _ => ThemeMode::Dark,
                    },
                };
                Theme::change(render_mode, None, cx);

                update_app_state_and_save(cx, "save_theme", move |state, _cx| {
                    state.set_theme(mode);
                });
            }))
            .on_action(cx.listener(|_this, e: &LocaleAction, _window, cx| {
                let locale = match e {
                    LocaleAction::Zh => "zh",
                    LocaleAction::En => "en",
                };

                update_app_state_and_save(cx, "save_locale", move |state, _cx| {
                    state.set_locale(locale.to_string());
                });
            }))
    }
    /// Render the star button
    fn render_star(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().border_b_1().border_color(cx.theme().border).child(
            Button::new("github")
                .ghost()
                .h(px(48.))
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        tracing::debug!("render sidebar view");
        v_flex()
            .w(px(80.))
            .id("sidebar-container")
            .justify_start()
            .h_full()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(self.render_star(window, cx))
            .child(
                div()
                    .flex_1()
                    .size_full()
                    .child(self.render_server_list(window, cx)),
            )
            .child(self.render_settings_button(window, cx))
    }
}
