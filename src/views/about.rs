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

use crate::assets::Assets;
use chrono::{Datelike, Local};
use gpui::{
    App, Bounds, Image, ImageFormat, TitlebarOptions, Window, WindowBounds, WindowKind, WindowOptions, img, prelude::*,
    px, size,
};
use gpui_component::{ActiveTheme, Sizable, StyledExt, button::Button, h_flex, label::Label, v_flex};
use std::sync::Arc;

struct About;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const GIT_SHA: &str = env!("VERGEN_GIT_SHA");

impl Render for About {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let year = Local::now().year().to_string();
        let logo = Assets::get("icon.png").map(|item| item.data).unwrap_or_default();
        let logo = Arc::new(Image::from_bytes(ImageFormat::Png, logo.to_vec()));
        let logo_size = px(96.);
        let years = if year == "2026" {
            "2026".to_string()
        } else {
            format!("2026 - {year}")
        };
        v_flex()
            .size_full()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .bg(cx.theme().background)
            // LOGO
            .child(
                h_flex()
                    .items_center()
                    .justify_center()
                    .child(img(logo.clone()).w(logo_size).h(logo_size)),
            )
            // App Name
            .child(
                Label::new("Zedis")
                    .text_xl()
                    .font_semibold()
                    .text_color(cx.theme().primary),
            )
            // Description
            .child(
                Label::new("A modern Redis client built with GPUI")
                    .text_sm()
                    .text_color(cx.theme().muted_foreground),
            )
            // Version
            .child(
                Label::new(format!("Version {VERSION}"))
                    .text_sm()
                    .text_color(cx.theme().muted_foreground),
            )
            // Technology Stack
            .child(
                Label::new("Built with Rust & GPUI")
                    .text_xs()
                    .text_color(cx.theme().muted_foreground),
            )
            // License
            .child(
                Label::new("Licensed under Apache License 2.0")
                    .text_xs()
                    .text_color(cx.theme().muted_foreground),
            )
            // Git SHA
            .child(
                Label::new(format!("Git SHA: {GIT_SHA}"))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground),
            )
            // Copyright
            .child(
                Label::new(format!("© {years} Tree xie. All rights reserved."))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground),
            )
            // Links
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .mt_4()
                    .child(
                        Button::new("github")
                            .label("GitHub")
                            .small()
                            .on_click(move |_, _window, cx| {
                                cx.open_url("https://github.com/vicanso/zedis");
                            }),
                    )
                    .child(
                        Button::new("docs")
                            .label("Documentation")
                            .small()
                            .on_click(move |_, _window, cx| {
                                cx.open_url("https://github.com/vicanso/zedis#readme");
                            }),
                    )
                    .child(
                        Button::new("issues")
                            .label("Report Issue")
                            .small()
                            .on_click(move |_, _window, cx| {
                                cx.open_url("https://github.com/vicanso/zedis/issues");
                            }),
                    ),
            )
    }
}

pub fn open_about_window(cx: &mut App) {
    let width = px(600.);
    let height = px(500.);
    let window_size = size(width, height);

    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, window_size, cx))),
        is_movable: false,
        is_resizable: false,

        titlebar: Some(TitlebarOptions {
            title: Some("About Zedis".into()),
            appears_transparent: true,
            ..Default::default()
        }),
        focus: true,
        kind: WindowKind::Normal,
        ..Default::default()
    };

    let _ = cx.open_window(options, |_, cx| cx.new(|_cx| About));
}
