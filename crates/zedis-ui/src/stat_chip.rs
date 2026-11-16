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

use gpui::{App, SharedString, Window, prelude::*};
use gpui_kit::component::{ActiveTheme, StyledExt, h_flex, label::Label};

/// Compact key-value statistic chip with a muted label and emphasized figure.
#[derive(IntoElement)]
pub struct ZedisStatChip {
    label: SharedString,
    value: SharedString,
}

/// Construct a reusable metric/status chip.
pub fn stat_chip(label: impl Into<SharedString>, value: impl Into<SharedString>) -> ZedisStatChip {
    ZedisStatChip {
        label: label.into(),
        value: value.into(),
    }
}

impl RenderOnce for ZedisStatChip {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        h_flex()
            .gap_1()
            .items_center()
            .px_2()
            .py_0p5()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .child(Label::new(self.label).text_xs().text_color(cx.theme().muted_foreground))
            .child(Label::new(self.value).text_xs().font_semibold())
    }
}
