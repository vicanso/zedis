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
use gpui::{App, SharedString};
use gpui_component::Theme;
use std::sync::{Arc, LazyLock};

/// Bundled and registered at startup via `add_fonts` (see `main.rs` +
/// `assets/fonts/JetBrainsMono-*.ttf`), so it renders identically on every
/// platform with real Regular/Bold faces. The default when the user hasn't
/// chosen their own monospace font.
const DEFAULT_MONO_FONT: &str = "JetBrains Mono";
/// GPUI's portable system-UI-font token (`.AppleSystemUIFont` on macOS, Segoe
/// UI on Windows, the default sans on Linux). The default UI font.
const DEFAULT_UI_FONT: &str = ".SystemUIFont";

/// Process-wide monospace family, read by every `.font_family(...)` mono call
/// site (~80 of them) so a settings change reaches all of them without
/// threading state through each. Overridden via [`apply_fonts`].
static MONO_FONT_FAMILY: LazyLock<ArcSwap<String>> =
    LazyLock::new(|| ArcSwap::from_pointee(DEFAULT_MONO_FONT.to_string()));

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
///   element) and `mono_font_family` (used by gpui-component's own widgets).
///
/// `None`/empty falls back to the system UI font / bundled JetBrains Mono.
/// Bundled theme configs never set these fields, so the values survive theme
/// and light/dark switches — apply once at startup and again on each change.
pub fn apply_fonts(cx: &mut App, ui_font: Option<&str>, mono_font: Option<&str>) {
    let ui = resolve_family(ui_font, DEFAULT_UI_FONT).to_string();
    let mono = resolve_family(mono_font, DEFAULT_MONO_FONT).to_string();
    MONO_FONT_FAMILY.store(Arc::new(mono.clone()));
    {
        let theme = Theme::global_mut(cx);
        theme.font_family = SharedString::from(ui);
        theme.mono_font_family = SharedString::from(mono);
    }
    cx.refresh_windows();
}
