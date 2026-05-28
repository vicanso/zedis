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

use crate::db::TagColor;
use gpui::{Hsla, hsla};
use gpui_component::ActiveTheme;

/// Resolve a preset tag color key (`gray` / `blue` / `green` / `amber` / `red`)
/// into an HSLA color suitable for chips and side bars.
///
/// Returns `None` for `None`, empty string, or `"none"`. Unknown keys fall back
/// to gray so that future presets do not blow up older clients.
pub fn resolve_tag_color(key: Option<&str>) -> Option<Hsla> {
    let raw = key?.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("none") {
        return None;
    }
    Some(match raw.to_ascii_lowercase().as_str() {
        "red" => hsla(0.0, 0.65, 0.50, 1.0),
        "amber" => hsla(0.10, 0.75, 0.50, 1.0),
        "green" => hsla(0.32, 0.55, 0.42, 1.0),
        "blue" => hsla(0.58, 0.55, 0.50, 1.0),
        _ => hsla(0.0, 0.0, 0.45, 1.0),
    })
}

/// Resolve a key-tag `TagColor` to a concrete HSLA, preferring the
/// active theme's primary palette so dark/light modes look intentional.
/// Orange and purple don't have direct theme accessors, so they fall
/// back to fixed mid-luminance HSLA tuned to read decently against
/// both backgrounds — the swatches are small (~22 px) so a single
/// shared value is fine without per-mode branching.
pub fn theme_color_for_tag(color: TagColor, cx: &gpui::App) -> Hsla {
    let theme = cx.theme();
    match color {
        TagColor::Red => theme.red,
        TagColor::Orange => hsla(0.08, 0.85, 0.55, 1.0),
        TagColor::Yellow => theme.yellow,
        TagColor::Green => theme.green,
        TagColor::Blue => theme.blue,
        TagColor::Purple => hsla(0.78, 0.55, 0.55, 1.0),
    }
}
