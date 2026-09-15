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

use arc_swap::ArcSwap;
use gpui::{App, SharedString, px};
use gpui_kit::component::Theme;
use std::rc::Rc;
use std::sync::{Arc, LazyLock};

/// Bundled and registered at startup via `add_fonts` (see `main.rs` +
/// `assets/fonts/JetBrainsMono-*.ttf`), so it renders identically on every
/// platform with real Regular/Bold faces. The default when the user hasn't
/// chosen their own monospace font.
const DEFAULT_MONO_FONT: &str = "JetBrains Mono";
/// GPUI's portable system-UI-font token (`.AppleSystemUIFont` on macOS, Segoe
/// UI on Windows, the default sans on Linux). The default UI font.
const DEFAULT_UI_FONT: &str = ".SystemUIFont";

/// App-wide rem base (`1rem` / default body size). gpui-component ships 16
/// (CSS convention); 14 sits closer to native desktop body (~13pt on macOS)
/// without going as small as AppKit controls.
///
/// Written onto `Theme::font_size` so `Root` cascades it every frame. Must be
/// re-applied after every `Theme::change` / `apply_config` — those rebuild
/// Theme from stock defaults and reset `font_size` to 16.
pub const DEFAULT_UI_FONT_SIZE: f32 = 14.0;

/// Pin [`DEFAULT_UI_FONT_SIZE`] on the global theme after theme init or switch.
pub fn apply_default_ui_font_size(cx: &mut App) {
    Theme::global_mut(cx).font_size = px(DEFAULT_UI_FONT_SIZE);
}

/// Process-wide monospace family, read by every `.font_family(...)` mono call
/// site (~80 of them) so a settings change reaches all of them without
/// threading state through each. Overridden via [`apply_fonts`].
static MONO_FONT_FAMILY: LazyLock<ArcSwap<String>> =
    LazyLock::new(|| ArcSwap::from_pointee(DEFAULT_MONO_FONT.to_string()));

/// The UI family last applied, so [`reapply_fonts`] can put it back after a
/// theme application without a handle on the app state.
static UI_FONT_FAMILY: LazyLock<ArcSwap<String>> = LazyLock::new(|| ArcSwap::from_pointee(DEFAULT_UI_FONT.to_string()));

pub fn get_mono_font_family() -> String {
    MONO_FONT_FAMILY.load().as_ref().clone()
}

/// Normalize a user-entered family: trim, and treat empty as "use `default`".
/// GPUI's `font_family` is a *single* family name (not a CSS comma stack), so
/// a name is taken verbatim; an unresolvable one falls back at render time.
fn resolve_family<'a>(name: Option<&'a str>, default: &'a str) -> &'a str {
    name.map(str::trim).filter(|s| !s.is_empty()).unwrap_or(default)
}

/// Apply the user's font choices for the whole app:
/// - the monospace global (all `get_mono_font_family()` call sites);
/// - the theme's `font_family` (which gpui-component's `Root` cascades to every
///   element) and `mono_font_family` (used by gpui-component's own widgets);
/// - the theme's light / dark config slots, so the next `Theme::change`
///   keeps the families.
///
/// `None`/empty falls back to the system UI font / bundled JetBrains Mono.
///
/// Every theme application (`Theme::change`, `apply_config`) rebuilds the
/// typography from stock defaults unless the config names a family, which is
/// why the slots are written too: a light / dark switch then keeps the
/// user's fonts. A named theme replaces the live values outright —
/// [`reapply_fonts`] after it. (gpui-component's mono-font probe, a full
/// font enumeration of ~100ms on macOS, runs once inside
/// `gpui_kit::component::init`'s own `Theme::change` and is cached for the
/// process; nothing here can move it.)
pub fn apply_fonts(cx: &mut App, ui_font: Option<&str>, mono_font: Option<&str>) {
    let ui = resolve_family(ui_font, DEFAULT_UI_FONT).to_string();
    let mono = resolve_family(mono_font, DEFAULT_MONO_FONT).to_string();
    MONO_FONT_FAMILY.store(Arc::new(mono.clone()));
    UI_FONT_FAMILY.store(Arc::new(ui.clone()));
    write_families(cx, ui, mono);
}

/// Put the families [`apply_fonts`] last recorded back onto the live theme and
/// its light / dark slots. Call after anything that applies a theme config
/// (`apply_named_theme`, `restore_default_themes`).
pub fn reapply_fonts(cx: &mut App) {
    let ui = UI_FONT_FAMILY.load().as_ref().clone();
    let mono = MONO_FONT_FAMILY.load().as_ref().clone();
    write_families(cx, ui, mono);
}

fn write_families(cx: &mut App, ui: String, mono: String) {
    let ui = SharedString::from(ui);
    let mono = SharedString::from(mono);
    let theme = Theme::global_mut(cx);
    theme.font_family = ui.clone();
    theme.mono_font_family = mono.clone();
    for slot in [&mut theme.light_theme, &mut theme.dark_theme] {
        let mut config = (**slot).clone();
        config.font_family = Some(ui.clone());
        config.mono_font_family = Some(mono.clone());
        *slot = Rc::new(config);
    }
    cx.refresh_windows();
}
