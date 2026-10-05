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

//! Window placement and theme application at launch and on change.

use crate::helpers::{apply_default_ui_font_size, reapply_fonts};
use crate::states::ZedisAppState;
use gpui::{App, Bounds, Pixels, WindowAppearance, px, size};
// Only the custom-drawn title bar path uses this (Linux/FreeBSD keep
// server-side decorations — see the cfg at the open_window call).
use gpui_kit::component::{Theme, ThemeMode, ThemeRegistry};
use std::rc::Rc;
use tracing::info;

/// Default window bounds: a 1200×750 window centered on the primary display,
/// shrunk to fit if the primary display is small.
pub(crate) fn default_window_bounds(cx: &mut App) -> Bounds<Pixels> {
    let mut window_size = size(px(1200.), px(750.));
    if let Some(display) = cx.primary_display() {
        let ds = display.bounds().size;
        window_size.width = window_size.width.min(ds.width * 0.85);
        window_size.height = window_size.height.min(ds.height * 0.85);
    }
    Bounds::centered(None, window_size, cx)
}

/// Resolve the bounds to open the window at, validating any saved placement
/// against the *current* display layout (monitors may have been unplugged,
/// resized, or rearranged since last run). Priority:
/// 1. the display we were last on, matched by uuid → restore relative origin;
/// 2. otherwise the absolute saved bounds, snapped onto the display they
///    overlap most (covers old configs / a monitor that's now gone);
/// 3. otherwise center on the primary display.
///
/// In all cases the result is clamped so the window fits and its title bar
/// stays reachable. The flag says whether the window was maximized there,
/// so it can open maximized again over that rectangle.
pub(crate) fn resolve_window_bounds(state: &ZedisAppState, cx: &mut App) -> (Bounds<Pixels>, bool) {
    // Shrink to fit the display, then keep the origin (title bar) on-screen.
    let clamp_to = |mut b: Bounds<Pixels>, screen: Bounds<Pixels>| -> Bounds<Pixels> {
        b.size = b.size.min(&screen.size);
        let max_x = screen.origin.x + screen.size.width - b.size.width;
        let max_y = screen.origin.y + screen.size.height - b.size.height;
        b.origin.x = b.origin.x.clamp(screen.origin.x, max_x);
        b.origin.y = b.origin.y.clamp(screen.origin.y, max_y);
        b
    };

    // Currently-connected displays keyed by uuid, plus the primary's uuid.
    let displays: Vec<(String, Bounds<Pixels>)> = cx
        .displays()
        .into_iter()
        .filter_map(|d| Some((d.uuid().ok()?.to_string(), d.bounds())))
        .collect();
    let primary_uuid = cx.primary_display().and_then(|d| d.uuid().ok()).map(|u| u.to_string());

    // 1) Restore the saved placement for a currently-connected display — the
    //    primary display first, else the most-recently-used connected one — so
    //    each monitor (work / home) keeps its own remembered position.
    let placement = state
        .window_placements()
        .iter()
        .find(|p| primary_uuid.as_deref() == Some(p.display_uuid.as_str()))
        .or_else(|| {
            state
                .window_placements()
                .iter()
                .find(|p| displays.iter().any(|(uuid, _)| uuid == &p.display_uuid))
        });
    if let Some(p) = placement
        && let Some((_, screen)) = displays.iter().find(|(uuid, _)| uuid == &p.display_uuid)
    {
        return (clamp_to(p.bounds + screen.origin, *screen), p.maximized);
    }

    // 2) Fallback: snap the absolute saved bounds onto the display they overlap
    //    most; no overlap (monitor gone / off-screen) -> fall through.
    if let Some(&saved) = state.bounds() {
        let area = |screen: &Bounds<Pixels>| {
            let i = saved.intersect(screen);
            if i.is_empty() {
                0.0
            } else {
                i.size.width.as_f32() * i.size.height.as_f32()
            }
        };
        if let Some((_, screen)) = displays
            .iter()
            .filter(|(_, b)| area(b) > 0.0)
            .max_by(|(_, a), (_, b)| area(a).total_cmp(&area(b)))
        {
            return (clamp_to(saved, *screen), false);
        }
    }

    // 3) Nothing usable → center on the primary display.
    info!("no usable saved window placement; centering on primary display");
    (default_window_bounds(cx), false)
}

/// Apply a registry theme by name (e.g. "Ayu Dark") if present, returning
/// whether it was found. Used at startup and from the title-bar theme menu.
pub(crate) fn apply_named_theme(name: &str, cx: &mut App) -> bool {
    let Some(config) = ThemeRegistry::global(cx).themes().get(name).cloned() else {
        return false;
    };
    Theme::global_mut(cx).apply_config(&config);
    // apply_config resets font_size to stock 16 unless the theme JSON sets it,
    // and the families to stock typography unless it names them.
    apply_default_ui_font_size(cx);
    reapply_fonts(cx);
    true
}

/// Apply the saved theme before the first window opens: the named registry
/// theme when one is saved and still exists, else the Light / Dark mode — the
/// user's, or the OS appearance for System.
///
/// The mode goes through [`restore_default_themes`], which is what gives the
/// default themes the brand primary. Applied bare, a saved Light or Dark
/// started with the registry's neutral primary — black buttons in light, white
/// in dark — until the theme menu was used; a System start was only put right
/// by the appearance observer, which a pinned mode never runs.
pub(crate) fn apply_startup_theme(theme_name: Option<&str>, mode: Option<ThemeMode>, cx: &mut App) {
    if theme_name.is_some_and(|name| apply_named_theme(name, cx)) {
        return;
    }
    // System (no saved mode) is resolved against the OS appearance here,
    // before the window opens, so the very first painted frame already uses
    // the right theme instead of flashing the default one.
    let mode = mode.unwrap_or_else(|| theme_mode_for_appearance(cx.window_appearance()));
    restore_default_themes(cx);
    Theme::change(mode, None, cx);
    // `Theme::change` resets font_size to stock 16; pin the app rem base
    // before the first frame (Root reads theme.font_size).
    apply_default_ui_font_size(cx);
}

/// Restore the registry's default light/dark configs into the global `Theme`.
/// `apply_config` (used to apply a named theme) overwrites the matching
/// `light_theme`/`dark_theme` slot, so picking Light/Dark/System afterwards
/// would just re-apply that named theme unless the slots are reset first.
pub(crate) fn restore_default_themes(cx: &mut App) {
    // Clone the configs out (owned) so we can override the primary before
    // installing them.
    let (mut light, mut dark) = {
        let registry = ThemeRegistry::global(cx);
        (
            (**registry.default_light_theme()).clone(),
            (**registry.default_dark_theme()).clone(),
        )
    };
    // The stock default themes use a neutral (near-black / near-white) primary,
    // so primary buttons are pure high-contrast fills that invert with the mode.
    // Override it to the Zedis brand blue so primary buttons read as the app's
    // color in both modes; white on this blue clears 4.5:1, so the label stays
    // legible. The hover/active shades are also set explicitly — the default
    // configs pin them to neutrals (a near-white hover in dark), so they must be
    // overridden too or the button flashes white; they step the primary down to
    // 85% / 75% brightness.
    for cfg in [&mut light, &mut dark] {
        cfg.colors.primary = Some("#1f6feb".into());
        cfg.colors.primary_foreground = Some("#ffffff".into());
        cfg.colors.primary_hover = Some("#1a5ec8".into());
        cfg.colors.primary_active = Some("#1753b0".into());
    }
    let theme = Theme::global_mut(cx);
    theme.light_theme = Rc::new(light);
    theme.dark_theme = Rc::new(dark);
    // Not applied to the live Theme yet — caller still runs Theme::change.
    // Pin rem base here too so a bare restore (if ever used alone) keeps 14,
    // and write the user's families into the fresh slots (they carry stock
    // typography) so that Theme::change keeps them.
    apply_default_ui_font_size(cx);
    reapply_fonts(cx);
}

/// Open the "update available" dialog on the main window. **Download** opens the
/// release page in the browser; **Skip this version** records the version so the
/// silent startup check won't prompt for it again. Closing without choosing
/// leaves nothing recorded, so the next daily check prompts again.
/// Compact Markdown styling for release notes: the library defaults size
/// headings up to ~28px, which dwarfs the dialog body; shrink them to a
/// gentle hierarchy (mirrors the memory-analysis AI panel styling).
/// Map the OS appearance to a theme mode when the user hasn't pinned one.
/// `VibrantLight` is macOS's translucent *light* appearance — group it with
/// `Light` so only genuinely dark appearances select the dark theme.
pub(crate) fn theme_mode_for_appearance(appearance: WindowAppearance) -> ThemeMode {
    match appearance {
        WindowAppearance::Light | WindowAppearance::VibrantLight => ThemeMode::Light,
        _ => ThemeMode::Dark,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Hsla, Rgba, TestAppContext};
    use gpui_kit::component::ActiveTheme;

    /// `0xRRGGBB` of a theme colour, so two routes to one colour compare equal.
    fn hex(color: Hsla) -> u32 {
        let rgba = Rgba::from(color);
        let channel = |value: f32| (value * 255.0).round() as u32;
        (channel(rgba.r) << 16) | (channel(rgba.g) << 8) | channel(rgba.b)
    }

    /// A pinned Light or Dark starts with the brand primary, as System does.
    #[gpui::test]
    fn a_pinned_mode_starts_with_the_brand_primary(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        const BRAND: u32 = 0x1f6feb;
        cx.update(|cx| {
            // What the registry ships, and what a pinned mode used to start with.
            Theme::change(ThemeMode::Light, None, cx);
            assert_ne!(hex(cx.theme().primary), BRAND, "the stock primary is neutral");
        });
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            cx.update(|cx| {
                apply_startup_theme(None, Some(mode), cx);
                assert_eq!(hex(cx.theme().primary), BRAND);
                assert_eq!(cx.theme().mode.is_dark(), mode.is_dark());
            });
        }
        // A saved name that is no longer in the registry falls back to the mode.
        cx.update(|cx| {
            apply_startup_theme(Some("No such theme"), Some(ThemeMode::Dark), cx);
            assert_eq!(hex(cx.theme().primary), BRAND);
            assert!(cx.theme().mode.is_dark());
        });
    }
}
