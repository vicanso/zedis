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

use gpui::{
    ClipboardItem, Image, Render, SharedString, Window, img, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Sizable, StyledExt, button::Button, h_flex, label::Label, scroll::ScrollableElement, v_flex,
};
use std::sync::Arc;

/// A link entry for the About page (label + URL).
pub struct AboutLink {
    pub id: SharedString,
    pub label: SharedString,
    pub url: SharedString,
}

impl AboutLink {
    pub fn new(
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        url: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            url: url.into(),
        }
    }
}

/// A text line displayed on the About page.
pub struct AboutLine {
    pub text: SharedString,
    pub size: AboutTextSize,
}

pub enum AboutTextSize {
    Sm,
    Xs,
}

impl AboutLine {
    pub fn sm(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            size: AboutTextSize::Sm,
        }
    }
    pub fn xs(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            size: AboutTextSize::Xs,
        }
    }
}

type SystemInfoCollector = Box<dyn Fn(&Window, &gpui::Context<ZedisAboutPage>) -> String>;

/// Configuration for the About page.
pub struct AboutConfig {
    pub name: SharedString,
    pub logo: Arc<Image>,
    pub lines: Vec<AboutLine>,
    pub links: Vec<AboutLink>,
    pub system_info_collector: Option<SystemInfoCollector>,
}

/// A reusable About page component.
///
/// Displays an app logo, name, descriptive lines, action links,
/// and an optional system information panel.
pub struct ZedisAboutPage {
    config: AboutConfig,
    system_info: Option<String>,
}

impl ZedisAboutPage {
    pub fn new(config: AboutConfig) -> Self {
        Self {
            config,
            system_info: None,
        }
    }
}

impl Render for ZedisAboutPage {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let logo_size = px(96.);

        let mut page = v_flex()
            .size_full()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .bg(cx.theme().background)
            // Logo
            .child(
                h_flex()
                    .items_center()
                    .justify_center()
                    .child(img(self.config.logo.clone()).w(logo_size).h(logo_size)),
            )
            // App name
            .child(
                Label::new(self.config.name.clone())
                    .text_xl()
                    .font_semibold()
                    .text_color(cx.theme().primary),
            );

        // Info lines
        for line in &self.config.lines {
            let label = Label::new(line.text.clone()).text_color(cx.theme().muted_foreground);
            let label = match line.size {
                AboutTextSize::Sm => label.text_sm(),
                AboutTextSize::Xs => label.text_xs(),
            };
            page = page.child(label);
        }

        // Links
        let mut links_row = h_flex().gap_3().items_center().mt_4();
        for link in &self.config.links {
            let url = link.url.clone();
            links_row = links_row.child(
                Button::new(link.id.clone())
                    .label(link.label.clone())
                    .small()
                    .on_click(move |_, _window, cx| {
                        cx.open_url(&url);
                    }),
            );
        }
        if self.config.system_info_collector.is_some() {
            links_row = links_row.child(
                Button::new("sysinfo")
                    .label("System Info")
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let Some(collector) = &this.config.system_info_collector {
                            this.system_info = Some(collector(window, cx));
                            cx.notify();
                        }
                    })),
            );
        }
        page = page.child(links_row);

        // System info panel
        if let Some(info) = &self.system_info {
            let info_for_copy = info.clone();
            page = page.child(
                v_flex()
                    .my_2()
                    .px_4()
                    .py_2()
                    .w(px(400.))
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().secondary)
                    .gap_1()
                    .relative()
                    .child(
                        h_flex().justify_end().absolute().right_2().top_2().child(
                            Button::new("copy-sysinfo")
                                .label("Copy")
                                .xsmall()
                                .on_click(move |_, _window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        info_for_copy.clone(),
                                    ));
                                }),
                        ),
                    )
                    .children(info.lines().map(|line| {
                        Label::new(line.to_string())
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                    })),
            );
        }

        page.overflow_y_scrollbar()
    }
}
