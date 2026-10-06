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

//! Shared theme-derived colors, so the same surface reads identically across
//! views instead of each recomputing (and drifting from) the value.

use gpui::{App, Hsla};
use gpui_kit::component::{ActiveTheme, Colorize};

/// How far a card surface is lifted off the theme background — lightened in
/// dark themes, darkened in light. Kept subtle so cards read as a gentle
/// elevation rather than a hard panel.
const CARD_LIGHTEN_DARK: f32 = 1.0;
const CARD_DARKEN_LIGHT: f32 = 0.02;

/// Shared card surface color: one small step off the theme background. Used by
/// the server cards (Home) and the config-editor cards so the two match.
pub fn card_background(cx: &App) -> Hsla {
    if cx.theme().is_dark() {
        cx.theme().background.lighten(CARD_LIGHTEN_DARK)
    } else {
        cx.theme().background.darken(CARD_DARKEN_LIGHT)
    }
}

/// WCAG 2.x contrast ratio of two opaque colors — what the tests that hold
/// a text color to 4.5:1 on its ground measure with.
#[cfg(test)]
pub fn contrast_ratio(a: Hsla, b: Hsla) -> f32 {
    let luminance = |color: Hsla| {
        let gpui::Rgba { r, g, b, .. } = gpui::Rgba::from(color);
        let linear = |c: f32| {
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
    };
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}
