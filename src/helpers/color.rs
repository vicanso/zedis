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
use gpui_component::{ActiveTheme, Colorize};

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
