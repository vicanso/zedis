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

use crate::states::Route;
use crate::states::ZedisGlobalStore;
use crate::states::ZedisServerState;
use crate::states::i18n_sidebar;
use crate::states::save_app_state;
use gpui::Action;
use gpui::Axis;
use gpui::Corner;
use gpui::Entity;
use gpui::Window;
use gpui::WindowAppearance;
use gpui::div;
use gpui::prelude::*;
use gpui::px;
use gpui_component::ActiveTheme;
use gpui_component::Icon;
use gpui_component::IconName;
use gpui_component::StyledExt;
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

pub struct ZedisSidebar {
    server_state: Entity<ZedisServerState>,
}
impl ZedisSidebar {
    pub fn new(
        _window: &mut Window,
        _cx: &mut Context<Self>,
        server_state: Entity<ZedisServerState>,
    ) -> Self {
        Self { server_state }
    }
    fn render_server_list(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut server_names = vec![String::new()];
        let server_state = self.server_state.read(cx);
        let current_server = server_state.server();
        if let Some(servers) = server_state.servers() {
            server_names.extend(servers.iter().map(|server| server.name.clone()));
        }
        let server_elements: Vec<_> = server_names
            .into_iter()
            .enumerate()
            .map(|(index, server_name)| {
                let is_home = server_name.is_empty();
                // let server_name = server_name.clone();
                let is_current = server_name == current_server;
                let name = if server_name.is_empty() {
                    i18n_sidebar(cx, "home")
                } else {
                    server_name.clone().into()
                };
                ListItem::new(("sidebar-redis-server", index))
                    .when(is_current, |this| this.bg(cx.theme().list_active))
                    .py_4()
                    .border_r_3()
                    .when(is_current, |this| {
                        this.border_color(cx.theme().list_active_border)
                    })
                    .child(
                        v_flex()
                            .items_center()
                            .child(Icon::new(IconName::LayoutDashboard))
                            .child(Label::new(name).text_ellipsis().text_xs()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if is_current {
                            return;
                        }
                        let route = if is_home { Route::Home } else { Route::Editor };
                        cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                            store.update(cx, |state, _cx| {
                                state.go_to(route);
                            });
                            cx.notify();
                        });
                        this.server_state.update(cx, |state, cx| {
                            state.select(server_name.clone().into(), cx);
                        });
                    }))
            })
            .collect();
        v_flex().flex_1().children(server_elements)
    }
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
            .h(px(60.))
            .tooltip(i18n_sidebar(cx, "settings"))
            .child(Icon::new(IconName::Settings).size(px(18.)))
            .dropdown_menu_with_anchor(Corner::BottomRight, move |menu, window, cx| {
                let theme_text = i18n_sidebar(cx, "theme");
                let lang_text = i18n_sidebar(cx, "lang");
                menu.submenu(theme_text, window, cx, move |submenu, _window, _cx| {
                    submenu
                        .menu_element_with_check(
                            current_action == ThemeAction::Light,
                            Box::new(ThemeAction::Light),
                            |_window, cx| Label::new(i18n_sidebar(cx, "light")).text_xs(),
                        )
                        .menu_element_with_check(
                            current_action == ThemeAction::Dark,
                            Box::new(ThemeAction::Dark),
                            |_window, cx| Label::new(i18n_sidebar(cx, "dark")).text_xs(),
                        )
                        .menu_element_with_check(
                            current_action == ThemeAction::System,
                            Box::new(ThemeAction::System),
                            |_window, cx| Label::new(i18n_sidebar(cx, "system")).text_xs(),
                        )
                })
                .submenu(lang_text, window, cx, move |submenu, _window, _cx| {
                    submenu
                        .menu_element_with_check(
                            current_locale == LocaleAction::Zh,
                            Box::new(LocaleAction::Zh),
                            |_window, _cx| Label::new("中文").text_xs(),
                        )
                        .menu_element_with_check(
                            current_locale == LocaleAction::En,
                            Box::new(LocaleAction::En),
                            |_window, _cx| Label::new("English").text_xs(),
                        )
                })
            });

        div()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(btn)
            .on_action(cx.listener(|_this, e: &ThemeAction, _window, cx| {
                let mode = match e {
                    ThemeAction::Light => {
                        Theme::change(ThemeMode::Light, None, cx);
                        Some(ThemeMode::Light)
                    }
                    ThemeAction::Dark => {
                        Theme::change(ThemeMode::Dark, None, cx);
                        Some(ThemeMode::Dark)
                    }
                    ThemeAction::System => {
                        let appearance = cx.window_appearance();
                        let mode = match appearance {
                            WindowAppearance::Light => ThemeMode::Light,
                            _ => ThemeMode::Dark,
                        };

                        Theme::change(mode, None, cx);
                        None
                    }
                };
                let mut value = cx.global::<ZedisGlobalStore>().value(cx);
                value.set_theme(mode);
                let store = cx.global::<ZedisGlobalStore>().clone();
                cx.spawn(async move |_, cx| {
                    let _ = store.update(cx, |state, _cx| {
                        state.set_theme(mode);
                    });
                    cx.background_spawn(async move {
                        if let Err(e) = save_app_state(&value) {
                            error!(error = %e, "save theme fail",);
                        } else {
                            info!("save theme success");
                        }
                    })
                    .await;
                })
                .detach();
                cx.refresh_windows();
            }))
            .on_action(cx.listener(|_this, e: &LocaleAction, _window, cx| {
                let locale = match e {
                    LocaleAction::Zh => "zh",
                    LocaleAction::En => "en",
                };
                // println!("locale: {}", locale);
                let mut value = cx.global::<ZedisGlobalStore>().value(cx);
                value.set_locale(locale.to_string());
                let store = cx.global::<ZedisGlobalStore>().clone();
                cx.spawn(async move |_, cx| {
                    let _ = store.update(cx, |state, _cx| {
                        state.set_locale(locale.to_string());
                    });
                    cx.background_spawn(async move {
                        if let Err(e) = save_app_state(&value) {
                            error!(error = %e, "save locale fail",);
                        } else {
                            info!("save locale success");
                        }
                    })
                    .await;
                })
                .detach();
                cx.refresh_windows();
            }))
    }
    fn render_star(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().border_b_1().border_color(cx.theme().border).child(
            Button::new("github")
                .ghost()
                .h(px(50.))
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
        v_flex()
            .w(px(80.))
            .id("sidebar-container")
            .justify_start()
            .h_full()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(self.render_star(window, cx))
            .child(
                div().w_full().flex_1().min_h_0().child(
                    div()
                        .child(self.render_server_list(window, cx))
                        .scrollable(Axis::Vertical),
                ),
            )
            .child(self.render_settings_button(window, cx))
    }
}
