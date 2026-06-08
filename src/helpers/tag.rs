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
use gpui::{Hsla, hsla, rgb};
use gpui_component::ActiveTheme;

/// Canonical environment key for a stored tag color preset. Normalizes
/// case/whitespace, maps legacy palette keys (`green` / `red` / `blue` /
/// `amber` / `gray`) onto the current environment palette, and folds
/// unknown keys to `slate`. Returns `None` for none/empty so callers can
/// short-circuit "no tag".
fn canonical_tag_key(key: Option<&str>) -> Option<&'static str> {
    let raw = key?.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("none") {
        return None;
    }
    Some(match raw.to_ascii_lowercase().as_str() {
        "sky" | "blue" => "sky",        // Local
        "teal" | "green" => "teal",     // Dev
        "purple" | "amber" => "purple", // UAT / staging
        "magenta" | "red" => "magenta", // Prod (high-risk)
        _ => "slate",                   // Archive / gray / unknown
    })
}

/// Resolve a preset tag color key to a single vivid HSLA — used for the
/// small sidebar dot and the status-bar chip, where one color reads
/// fine on both light and dark backgrounds. Chips that need contrast
/// (cards) use [`resolve_tag_chip`] instead.
///
/// Returns `None` for none/empty. Legacy keys alias to the nearest new
/// color; unknown keys fall back to slate.
pub fn resolve_tag_color(key: Option<&str>) -> Option<Hsla> {
    let hex = match canonical_tag_key(key)? {
        "sky" => 0x60a5fa,
        "teal" => 0x48cae4,
        "purple" => 0xb886fb,
        "magenta" => 0xf472b6,
        _ => 0x94a3b8,
    };
    Some(rgb(hex).into())
}

/// Resolve a tag color key to a `(background, foreground)` HSLA pair for
/// chip rendering, picking light- or dark-mode values per `dark`. The
/// distinct *backgrounds* (not just text) are what keep adjacent
/// environments (Dev/Local, UAT/Prod) visually separable.
pub fn resolve_tag_chip(key: Option<&str>, dark: bool) -> Option<(Hsla, Hsla)> {
    // (light_bg, light_fg, dark_bg, dark_fg)
    let (lbg, lfg, dbg, dfg): (u32, u32, u32, u32) = match canonical_tag_key(key)? {
        "magenta" => (0xfdf2f8, 0xbe185d, 0x68113f, 0xf472b6),
        "purple" => (0xf3e8ff, 0x7e22ce, 0x3c225f, 0xb886fb),
        "teal" => (0xccfbf1, 0x0f766e, 0x0f4c5c, 0x48cae4),
        "sky" => (0xeff6ff, 0x1d4ed8, 0x1e3a8a, 0x60a5fa),
        _ => (0xf1f5f9, 0x475569, 0x334155, 0x94a3b8), // slate
    };
    let (bg, fg) = if dark { (dbg, dfg) } else { (lbg, lfg) };
    Some((rgb(bg).into(), rgb(fg).into()))
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
