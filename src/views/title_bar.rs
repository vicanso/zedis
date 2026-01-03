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

use crate::states::{
    FONT_SIZE_LARGE, FONT_SIZE_MEDIUM, FONT_SIZE_SMALL, FontSizeAction, LocaleAction, SettingsAction, ThemeAction,
    ZedisGlobalStore, i18n_sidebar,
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
        let (font_size, locale, theme) = (store.font_size(), store.locale(), store.theme());

        this
            // font size menu
            .label(i18n_sidebar(cx, "font_size"))
            .menu_with_check(
                i18n_sidebar(cx, "font_size_large"),
                font_size == FONT_SIZE_LARGE,
                Box::new(FontSizeAction::Large),
            )
            .menu_with_check(
                i18n_sidebar(cx, "font_size_medium"),
                font_size == FONT_SIZE_MEDIUM,
                Box::new(FontSizeAction::Medium),
            )
            .menu_with_check(
                i18n_sidebar(cx, "font_size_small"),
                font_size == FONT_SIZE_SMALL,
                Box::new(FontSizeAction::Small),
            )
            .separator()
            // language menu
            .label(i18n_sidebar(cx, "lang"))
            .menu_with_check("中文", locale == "zh", Box::new(LocaleAction::Zh))
            .menu_with_check("English", locale == "en", Box::new(LocaleAction::En))
            .separator()
            // theme menu
            .label(i18n_sidebar(cx, "theme"))
            .menu_with_check(
                i18n_sidebar(cx, "light"),
                theme == Some(ThemeMode::Light),
                Box::new(ThemeAction::Light),
            )
            .menu_with_check(
                i18n_sidebar(cx, "dark"),
                theme == Some(ThemeMode::Dark),
                Box::new(ThemeAction::Dark),
            )
            .menu_with_check(
                i18n_sidebar(cx, "system"),
                theme.is_none(),
                Box::new(ThemeAction::System),
            )
            .separator()
            .menu_element_with_icon(
                Icon::new(IconName::Settings2),
                Box::new(SettingsAction::Editor),
                move |_window, cx| Label::new(i18n_sidebar(cx, "other_settings")),
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
                            .icon(IconName::GitHub)
                            .small()
                            .ghost()
                            .on_click(|_, _, cx| cx.open_url("https://github.com/vicanso/zedis")),
                    ),
            )
    }
}
