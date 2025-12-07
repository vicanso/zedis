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
use chrono::Datelike;
use chrono::Local;
use gpui::App;
use gpui::Bounds;
use gpui::TitlebarOptions;
use gpui::Window;
use gpui::WindowBounds;
use gpui::WindowKind;
use gpui::WindowOptions;
use gpui::prelude::*;
use gpui::px;
use gpui::size;
use gpui_component::ActiveTheme;
use gpui_component::Icon;
use gpui_component::h_flex;
use gpui_component::label::Label;
use gpui_component::v_flex;

struct About;

const VERSION: &str = env!("CARGO_PKG_VERSION");

impl Render for About {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let year = Local::now().year().to_string();
        v_flex()
            .size_full()
            .flex_col()
            .items_center()
            .justify_center()
            .bg(cx.theme().background)
            // LOGO
            .child(
                h_flex().items_center().justify_center().child(
                    Icon::new(CustomIconName::Zap)
                        .size(px(64.))
                        .text_color(cx.theme().primary),
                ),
            )
            .child(Label::new("Zedis").text_xl())
            .child(
                Label::new(format!("Version {VERSION}"))
                    .text_sm()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                Label::new(format!("© 2025 - {year} Tree xie. All rights reserved."))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground),
            )
    }
}

pub fn open_about_window(cx: &mut App) {
    let width = px(300.);
    let height = px(200.);
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
