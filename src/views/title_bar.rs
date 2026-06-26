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

use crate::helpers::{MemuAction, PaletteAction, ShortcutsAction, UpdateAction};
use crate::{
    assets::CustomIconName,
    connection::get_server,
    states::{
        GlobalEvent, LocaleAction, Route, SelectThemeAction, SettingsAction, ThemeAction, ZedisGlobalStore,
        i18n_sidebar,
    },
};
use gpui::{Anchor, App, Context, SharedString, Subscription, Window, prelude::*};
use gpui_component::{
    Icon, IconName, Sizable, StyledExt, ThemeMode, ThemeRegistry, TitleBar,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    menu::{DropdownMenu, PopupMenu, PopupMenuItem},
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

    fn render_settings_menu(this: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
        // Theme: appearance mode (light / dark / follow-system) followed by the
        // registered color themes, all in one submenu so the top-level menu
        // stays short. None of light/dark/system is checked while a named theme
        // overrides the mode; the registry's built-in default light/dark are
        // skipped because light/dark already select them.
        let this = this
            // Header: app name + version, like Zed's account menu. Disabled so
            // it reads as a title rather than a clickable row.
            .item(
                PopupMenuItem::element(|_window, _cx| {
                    Label::new(concat!("Zedis v", env!("CARGO_PKG_VERSION"))).font_semibold()
                })
                .disabled(true),
            )
            .separator()
            .label(i18n_sidebar(cx, "preferences"))
            .submenu_with_icon(
                Some(Icon::new(CustomIconName::SwatchBook)),
                i18n_sidebar(cx, "theme"),
                window,
                cx,
                |submenu, _window, cx| {
                    let store = cx.global::<ZedisGlobalStore>().read(cx);
                    let has_named_theme = store.theme_name().is_some();
                    let theme = store.theme();
                    let current = store.theme_name();
                    let light_checked = !has_named_theme && theme == Some(ThemeMode::Light);
                    let dark_checked = !has_named_theme && theme == Some(ThemeMode::Dark);
                    let system_checked = !has_named_theme && theme.is_none();
                    let mut submenu = submenu
                        .menu_element_with_check(light_checked, Box::new(ThemeAction::Light), |_, cx| {
                            Label::new(i18n_sidebar(cx, "light"))
                        })
                        .menu_element_with_check(dark_checked, Box::new(ThemeAction::Dark), |_, cx| {
                            Label::new(i18n_sidebar(cx, "dark"))
                        })
                        .menu_element_with_check(system_checked, Box::new(ThemeAction::System), |_, cx| {
                            Label::new(i18n_sidebar(cx, "system"))
                        })
                        .separator();
                    let registry = ThemeRegistry::global(cx);
                    let default_light = registry.default_light_theme().name.clone();
                    let default_dark = registry.default_dark_theme().name.clone();
                    for config in registry.sorted_themes() {
                        let name = config.name.clone();
                        if name == default_light || name == default_dark {
                            continue;
                        }
                        let checked = current.as_deref() == Some(&*name);
                        let action = Box::new(SelectThemeAction { name: name.to_string() });
                        submenu =
                            submenu.menu_element_with_check(checked, action, move |_, _cx| Label::new(name.clone()));
                    }
                    submenu
                },
            )
            // Language: pick the UI locale. Native names so each stays
            // recognizable regardless of the current language; active one checked.
            .submenu_with_icon(
                Some(Icon::new(CustomIconName::Languages)),
                i18n_sidebar(cx, "language"),
                window,
                cx,
                |submenu, _window, cx| {
                    let current = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
                    let mut submenu = submenu;
                    for (action, code, name) in [
                        (LocaleAction::En, "en", "English"),
                        (LocaleAction::Zh, "zh", "中文"),
                        (LocaleAction::Ru, "ru", "Русский"),
                        (LocaleAction::Ja, "ja", "日本語"),
                        (LocaleAction::Pt, "pt", "Português"),
                        (LocaleAction::Es, "es", "Español"),
                        (LocaleAction::De, "de", "Deutsch"),
                        (LocaleAction::Fr, "fr", "Français"),
                    ] {
                        let checked = current == code;
                        submenu =
                            submenu.menu_element_with_check(checked, Box::new(action), move |_, _cx| Label::new(name));
                    }
                    submenu
                },
            );

        this.separator()
            // App actions, shown with their shortcuts (Zed-style flat items).
            .menu_with_icon(
                i18n_sidebar(cx, "command_palette"),
                Icon::new(CustomIconName::Command),
                Box::new(PaletteAction::Toggle),
            )
            .menu_with_icon(
                i18n_sidebar(cx, "keyboard_shortcuts"),
                Icon::new(CustomIconName::Keyboard),
                Box::new(ShortcutsAction::Toggle),
            )
            .separator()
            // Settings: the configuration sub-views, grouped into one submenu so
            // the top-level menu stays short.
            .submenu_with_icon(
                Some(Icon::new(IconName::Settings2)),
                i18n_sidebar(cx, "settings"),
                window,
                cx,
                |submenu, _window, _cx| {
                    submenu
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
                },
            )
            .menu_with_icon(
                i18n_sidebar(cx, "check_updates"),
                Icon::new(CustomIconName::RefreshCw),
                Box::new(UpdateAction::Check),
            )
            .menu_with_icon(
                i18n_sidebar(cx, "open_logs"),
                Icon::new(CustomIconName::HardDrive),
                Box::new(MemuAction::OpenLogs),
            )
            .menu_with_icon(
                i18n_sidebar(cx, "about"),
                Icon::new(IconName::Info),
                Box::new(MemuAction::About),
            )
            .separator()
            .menu_with_icon(
                i18n_sidebar(cx, "quit"),
                Icon::new(CustomIconName::Power),
                Box::new(MemuAction::Quit),
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
                            .dropdown_menu(Self::render_settings_menu)
                            .anchor(Anchor::TopRight),
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
