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

use crate::helpers::MemuAction;
use crate::{
    assets::CustomIconName,
    connection::get_server,
    states::{GlobalEvent, Route, SelectThemeAction, SettingsAction, ThemeAction, ZedisGlobalStore, i18n_sidebar},
};
use gpui::{App, Context, Corner, SharedString, Subscription, Window, prelude::*};
use gpui_component::{
    Icon, IconName, Sizable, StyledExt, ThemeMode, ThemeRegistry, TitleBar,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    menu::{DropdownMenu, PopupMenu},
};

pub struct ZedisTitleBar {
    _subscriptions: Vec<Subscription>,
}

impl ZedisTitleBar {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Re-render whenever the active server changes (selection,
        // clear, or a rename via the server list) so the centered
        // title stays in sync.
        let global_state = cx.global::<ZedisGlobalStore>().state();
        let subscription = cx.subscribe(&global_state, |_this, _global_state, event, cx| {
            if matches!(
                event,
                GlobalEvent::ServerSelected(..) | GlobalEvent::ServerListUpdated | GlobalEvent::RouteChanged(..)
            ) {
                cx.notify();
            }
        });

        Self {
            _subscriptions: vec![subscription],
        }
    }

    /// Resolve the display name of the currently selected server, if any.
    ///
    /// The selected server is persisted across restarts, but the Home
    /// page is a server-agnostic chooser — showing the previously
    /// selected name there is misleading, so hide it on `Route::Home`.
    fn selected_server_name(cx: &App) -> Option<SharedString> {
        let state = cx.global::<ZedisGlobalStore>().read(cx);
        if state.route() == Route::Home {
            return None;
        }
        let (server_id, _db) = state.selected_server()?.clone();
        get_server(&server_id)
            .ok()
            .map(|server| SharedString::from(server.name))
    }

    fn render_settings_menu(this: PopupMenu, cx: &App) -> PopupMenu {
        let store = cx.global::<ZedisGlobalStore>().read(cx);
        let theme = store.theme();
        // A named theme overrides the mode, so none of Light/Dark/System is
        // checked while one is active.
        let has_named_theme = store.theme_name().is_some();
        let light_checked = !has_named_theme && theme == Some(ThemeMode::Light);
        let dark_checked = !has_named_theme && theme == Some(ThemeMode::Dark);
        let system_checked = !has_named_theme && theme.is_none();

        let this = this.label(i18n_sidebar(cx, "theme"));

        let this = if light_checked {
            this.menu_with_check(i18n_sidebar(cx, "light"), true, Box::new(ThemeAction::Light))
        } else {
            this.menu_element_with_icon(
                Icon::new(IconName::Sun),
                Box::new(ThemeAction::Light),
                move |_window, cx| Label::new(i18n_sidebar(cx, "light")),
            )
        };

        let this = if dark_checked {
            this.menu_with_check(i18n_sidebar(cx, "dark"), true, Box::new(ThemeAction::Dark))
        } else {
            this.menu_element_with_icon(
                Icon::new(IconName::Moon),
                Box::new(ThemeAction::Dark),
                move |_window, cx| Label::new(i18n_sidebar(cx, "dark")),
            )
        };

        let this = if system_checked {
            this.menu_with_check(i18n_sidebar(cx, "system"), true, Box::new(ThemeAction::System))
        } else {
            this.menu_element_with_icon(
                Icon::new(CustomIconName::SunMoon),
                Box::new(ThemeAction::System),
                move |_window, cx| Label::new(i18n_sidebar(cx, "system")),
            )
        };

        // Registered color themes, listed in the same group right after
        // Light/Dark/System (no separator). The registry's built-in default
        // light/dark are skipped — Light/Dark/System already cover them.
        // Selecting one applies it and overrides the mode until a mode is
        // re-picked (which clears the saved theme name).
        let registry = ThemeRegistry::global(cx);
        let default_light = registry.default_light_theme().name.clone();
        let default_dark = registry.default_dark_theme().name.clone();
        let current_theme_name = store.theme_name();
        let mut this = this;
        for config in registry.sorted_themes() {
            let name = config.name.clone();
            if name == default_light || name == default_dark {
                continue;
            }
            if current_theme_name.as_deref() == Some(&*name) {
                this = this.menu_with_check(
                    name.clone(),
                    true,
                    Box::new(SelectThemeAction { name: name.to_string() }),
                );
            } else {
                let action = Box::new(SelectThemeAction { name: name.to_string() });
                this =
                    this.menu_element_with_icon(Icon::new(CustomIconName::SwatchBook), action, move |_window, _cx| {
                        Label::new(name.clone())
                    });
            }
        }

        this.separator()
            .menu_element_with_icon(
                Icon::new(CustomIconName::SwatchBook),
                Box::new(SettingsAction::Protos),
                move |_window, cx| Label::new(i18n_sidebar(cx, "proto_settings")),
            )
            .menu_element_with_icon(
                Icon::new(CustomIconName::Binary),
                Box::new(SettingsAction::Scripts),
                move |_window, cx| Label::new(i18n_sidebar(cx, "script_settings")),
            )
            .menu_element_with_icon(
                Icon::new(IconName::Settings2),
                Box::new(SettingsAction::Editor),
                move |_window, cx| Label::new(i18n_sidebar(cx, "other_settings")),
            )
            .menu_element_with_icon(
                Icon::new(IconName::Info),
                Box::new(MemuAction::About),
                move |_window, cx| Label::new(i18n_sidebar(cx, "about")),
            )
    }
}

impl Render for ZedisTitleBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // right actions container
        let right_actions = h_flex().flex_1().items_center().justify_end().px_2().gap_2().mr_2();

        // Centered title showing the active server name (empty when none).
        let center =
            h_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .child(
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .when_some(Self::selected_server_name(cx), |this, name| {
                            this.child(Icon::new(IconName::HardDrive).small())
                                .child(Label::new(name).font_semibold())
                        }),
                );

        TitleBar::new()
            // left placeholder balances the right actions so `center` stays centered
            .child(h_flex().flex_1())
            .child(center)
            // right actions container
            .child(
                right_actions
                    .child(
                        Button::new("settings")
                            .tooltip(i18n_sidebar(cx, "settings_tooltip"))
                            .icon(IconName::Settings2)
                            .small()
                            .ghost()
                            .dropdown_menu(move |this, _, cx| Self::render_settings_menu(this, cx))
                            .anchor(Corner::TopRight),
                    )
                    .child(
                        Button::new("github")
                            .tooltip(i18n_sidebar(cx, "github_tooltip"))
                            .icon(IconName::Github)
                            .small()
                            .ghost()
                            .on_click(|_, _, cx| cx.open_url("https://github.com/vicanso/zedis")),
                    ),
            )
    }
}
