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
    states::{SettingsAction, ThemeAction, ZedisGlobalStore, i18n_sidebar},
};
use gpui::{App, Context, Corner, Window, prelude::*};
use gpui_component::{
    Icon, IconName, Sizable, ThemeMode, TitleBar,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    menu::{DropdownMenu, PopupMenu},
};

pub struct ZedisTitleBar;

impl ZedisTitleBar {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self
    }

    fn render_settings_menu(this: PopupMenu, cx: &App) -> PopupMenu {
        let store = cx.global::<ZedisGlobalStore>().read(cx);
        let theme = store.theme();
        let light_checked = theme == Some(ThemeMode::Light);
        let dark_checked = theme == Some(ThemeMode::Dark);
        let system_checked = theme.is_none();

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
        let right_actions = h_flex().items_center().justify_end().px_2().gap_2().mr_2();

        TitleBar::new()
            // left placeholder
            .child(h_flex().flex_1())
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
