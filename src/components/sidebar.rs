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

use gpui::App;
use gpui::Context;
use gpui::InteractiveElement;
use gpui::ParentElement;
use gpui::Render;
use gpui::RenderOnce;
use gpui::Styled;
use gpui::Window;
use gpui::div;
use gpui::px;
use gpui_component::ActiveTheme;
use gpui_component::Sizable;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::v_flex;
use gpui_macros::IntoElement;

#[derive(IntoElement)]
pub struct ZedisSidebar {}
impl ZedisSidebar {
    pub fn new(window: &mut Window, cx: &mut App) -> Self {
        Self {}
    }
}
impl RenderOnce for ZedisSidebar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl gpui::IntoElement {
        div()
            .id("sidebar-container")
            .border_color(cx.theme().border)
            .size(px(48.))
            .border_r_1()
            .h_full()
            .child(
                v_flex()
                    .h_full()
                    .justify_end()
                    .child(Button::new("line-column").ghost().xsmall().label("input")),
            )
    }
}
