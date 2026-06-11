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

use crate::states::KeyType;
use gpui::{App, IntoElement, RenderOnce, Styled, Window, div, px};
use gpui_component::label::Label;

// Constants for key type badge styling
const KEY_TYPE_FADE_ALPHA: f32 = 0.8; // Background transparency for key type badges
const KEY_TYPE_BORDER_FADE_ALPHA: f32 = 0.5; // Border transparency for key type badges

#[derive(IntoElement)]
pub struct KeyTypeBadge {
    key_type: KeyType,
}

impl KeyTypeBadge {
    pub fn new(key_type: KeyType) -> Self {
        Self { key_type }
    }
}

impl RenderOnce for KeyTypeBadge {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        if self.key_type == KeyType::Unknown {
            return div().into_any_element();
        }

        let color = self.key_type.color();
        let mut bg = color;
        bg.fade_out(KEY_TYPE_FADE_ALPHA);
        let mut border = color;
        border.fade_out(KEY_TYPE_BORDER_FADE_ALPHA);

        Label::new(self.key_type.as_str())
            .text_size(px(10.))
            .w(px(36.))
            .text_center()
            .bg(bg)
            .text_color(color)
            .border_1()
            .px_1()
            .rounded_sm()
            .border_color(border)
            .into_any_element()
    }
}