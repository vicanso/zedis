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
use gpui::{App, FontWeight, IntoElement, RenderOnce, Styled, Window, div, px};
use gpui_component::label::Label;

// Constants for key type badge styling
const KEY_TYPE_FADE_ALPHA: f32 = 0.8; // Background transparency for key type badges
const KEY_TYPE_BORDER_FADE_ALPHA: f32 = 0.5; // Border transparency for key type badges

#[derive(IntoElement)]
pub struct KeyTypeBadge {
    key_type: KeyType,
    /// Plain colored text (no pill chrome) using the spelled-out type name —
    /// the key-tree style from the design. Default `false` keeps the filled
    /// pill (with the compact `as_str` code) used in the editor header.
    plain: bool,
}

impl KeyTypeBadge {
    pub fn new(key_type: KeyType) -> Self {
        Self { key_type, plain: false }
    }

    /// Render as plain colored uppercase text instead of a filled pill.
    pub fn plain(mut self, plain: bool) -> Self {
        self.plain = plain;
        self
    }
}

impl RenderOnce for KeyTypeBadge {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        if self.key_type == KeyType::Unknown {
            return div().into_any_element();
        }

        let color = self.key_type.color();

        if self.plain {
            // Plain colored uppercase text (compact `as_str` codes — STR / STRM
            // / VEC) — the design renders types as a quiet colored label, not a
            // pill.
            return Label::new(self.key_type.as_str())
                .text_size(px(10.))
                .font_weight(FontWeight::SEMIBOLD)
                .flex_none()
                .whitespace_nowrap()
                .text_color(color)
                .into_any_element();
        }

        let mut bg = color;
        bg.fade_out(KEY_TYPE_FADE_ALPHA);
        let mut border = color;
        border.fade_out(KEY_TYPE_BORDER_FADE_ALPHA);

        Label::new(self.key_type.as_str())
            .text_size(px(10.))
            // The width hugs the text instead of being fixed: `flex_none`
            // keeps the badge from being squeezed in the tight title /
            // key-tree rows, `whitespace_nowrap` keeps it on a single line,
            // and the horizontal padding gives every label the same breathing
            // room — so "TS" stays compact while "STRM"/"CHANNEL" get the
            // space they need instead of wrapping.
            .flex_none()
            .whitespace_nowrap()
            .bg(bg)
            .text_color(color)
            .border_1()
            .px_1p5()
            .rounded_sm()
            .border_color(border)
            .into_any_element()
    }
}
