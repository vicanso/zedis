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

use gpui::{App, Hsla, hsla, rgb};
use gpui_kit::component::{ActiveTheme, Colorize};

/// How far a card surface is lifted off the theme background — lightened in
/// dark themes, darkened in light. Note that gpui-component's `lighten` scales
/// lightness multiplicatively (`l * (1.0 + factor)`), so on a dark ground
/// (`l ≈ 0.039`) a factor of `1.0` lifts it by ~0.039 to ~0.078, matching the
/// subtle step of `darken(0.02)` in light themes.
const CARD_LIGHTEN_DARK: f32 = 1.0;
const CARD_DARKEN_LIGHT: f32 = 0.02;

/// Master color palette for cluster slot bar and node load cards (cycled by node index).
const TOPOLOGY_MASTER_PALETTE: [u32; 8] = [
    0x4c_8b_f5, // blue
    0x69_b0_83, // green
    0xe5_a5_4b, // amber
    0xe0_6c_75, // red
    0xc6_78_dd, // purple
    0x56_b6_c2, // cyan
    0xd1_9a_66, // orange
    0xab_b2_bf, // grey
];

/// Shared card surface color: one small step off the theme background. Used by
/// the server cards (Home) and the config-editor cards so the two match.
pub fn card_background(cx: &App) -> Hsla {
    if cx.theme().is_dark() {
        cx.theme().background.lighten(CARD_LIGHTEN_DARK)
    } else {
        cx.theme().background.darken(CARD_DARKEN_LIGHT)
    }
}

/// Recessed trough background for the workspace tab bar, ensuring inactive
/// tabs sit visibly distinct from the active tab's `background` fill even
/// on named themes where `tab_bar` falls back to `background`.
pub fn workspace_tab_bar_background(cx: &App) -> Hsla {
    let theme = cx.theme();
    let bg = theme.background;
    let tab_bar = theme.tab_bar;
    if (tab_bar.l - bg.l).abs() >= 0.03 {
        tab_bar
    } else if theme.is_dark() {
        if bg.l > 0.08 {
            bg.darken(0.22)
        } else {
            card_background(cx)
        }
    } else {
        bg.darken(0.04)
    }
}

/// Healthy link / latency indicator green, tuned for 4.5:1+ contrast in both themes.
pub fn healthy_color(dark: bool) -> Hsla {
    rgb(if dark { 0x69b083 } else { 0x1a7f37 }).into()
}

/// Selected database accent icon / monogram color in the sidebar.
pub fn sidebar_accent_color(dark: bool) -> Hsla {
    rgb(if dark { 0x6b95c4 } else { 0x2563eb }).into()
}

/// Production-environment banner and chip `(background, foreground)` pair.
pub fn prod_banner_colors(dark: bool) -> (Hsla, Hsla) {
    if dark {
        (rgb(0x3f1a2e).into(), rgb(0xf9a8d4).into())
    } else {
        (rgb(0xfdf2f8).into(), rgb(0xbe185d).into())
    }
}

/// Warm amber for expiring-soon (`< 1h`) TTL chips, accessible on both light and dark rows.
pub fn ttl_warning_color(dark: bool) -> Hsla {
    rgb(if dark { 0xcba26a } else { 0x9a6700 }).into()
}

/// Monospace hex-dump address column foreground color.
pub fn hex_address_color(dark: bool) -> Hsla {
    if dark {
        hsla(0.108, 0.66, 0.69, 1.0)
    } else {
        hsla(0.0892, 0.9462, 0.4373, 1.0)
    }
}

/// Standard modal / palette backdrop scrim.
pub fn overlay_backdrop_color() -> Hsla {
    hsla(0., 0., 0., 0.4)
}

/// Canvas surface fill for pixel-grid bitmap and geo plots.
pub fn canvas_surface_background(cx: &App) -> Hsla {
    if cx.theme().is_dark() {
        rgb(0x1a1d21).into()
    } else {
        cx.theme().muted
    }
}

/// Geospatial plot marker `(base, highlight)` colors per theme polarity.
pub fn geo_node_colors(dark: bool) -> (Hsla, Hsla) {
    if dark {
        (rgb(0x4fd1c5).into(), rgb(0x9af5ea).into())
    } else {
        (rgb(0x0d9488).into(), rgb(0x14b8a6).into())
    }
}

/// Deterministic palette color for a cluster master node by index.
pub fn topology_master_color(index: usize) -> Hsla {
    rgb(TOPOLOGY_MASTER_PALETTE[index % TOPOLOGY_MASTER_PALETTE.len()]).into()
}

/// Heat color ramp for cluster load ratios in `0.0..=1.0` (green -> amber -> red).
pub fn topology_heat_color(ratio: f32) -> Hsla {
    let r = ratio.clamp(0.0, 1.0);
    if r < 0.5 {
        let t = r * 2.0;
        let g = 0xb0u8;
        let red = (0x69u8 as f32 + t * (0xe5 - 0x69) as f32) as u8;
        let blue = (0x83u8 as f32 * (1.0 - t)) as u8;
        rgb(u32::from_be_bytes([0, red, g, blue])).into()
    } else {
        let t = (r - 0.5) * 2.0;
        let red = 0xe5u8;
        let green = (0xa5u8 as f32 * (1.0 - t) + 0x6c as f32 * t) as u8;
        let blue = (0x4bu8 as f32 * (1.0 - t) + 0x75 as f32 * t) as u8;
        rgb(u32::from_be_bytes([0, red, green, blue])).into()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_semantic_colors_meet_wcag_aa_contrast() {
        let light_bg: Hsla = rgb(0xffffff).into();
        let dark_bg: Hsla = rgb(0x0a0a0a).into();

        assert!(contrast_ratio(healthy_color(false), light_bg) >= 4.5);
        assert!(contrast_ratio(healthy_color(true), dark_bg) >= 4.5);

        assert!(contrast_ratio(sidebar_accent_color(false), light_bg) >= 4.5);
        assert!(contrast_ratio(sidebar_accent_color(true), dark_bg) >= 4.5);

        assert!(contrast_ratio(ttl_warning_color(false), light_bg) >= 4.5);
        assert!(contrast_ratio(ttl_warning_color(true), dark_bg) >= 4.5);

        for dark in [false, true] {
            let (bg, fg) = prod_banner_colors(dark);
            assert!(
                contrast_ratio(bg, fg) >= 4.5,
                "prod_banner_colors(dark={dark}) failed 4.5:1"
            );
        }
    }
}
