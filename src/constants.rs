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
use gpui::{Pixels, px};

/// User-facing application name (window title, menus, About).
pub const APP_NAME: &str = "Zedis";

/// Freedesktop / Wayland `app_id` for AppImage and tarball installs.
///
/// Must match the desktop file id (`zedis.desktop` → `zedis`) and the
/// `Icon=` / `StartupWMClass` fields so KDE/GNOME can resolve the name and
/// icon. Flatpak overrides this at runtime via `$FLATPAK_ID`
/// (`io.github.vicanso.zedis`) — see [`linux_app_id`].
pub const APP_ID: &str = "zedis";

/// Wayland/X11 application id for the running process.
///
/// Flatpak exports `FLATPAK_ID` matching the manifest `app-id`
/// (`io.github.vicanso.zedis`); elsewhere we use [`APP_ID`] so
/// AppImage/`zedis.desktop` icon lookup works.
pub fn linux_app_id() -> String {
    std::env::var("FLATPAK_ID")
        .ok()
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| APP_ID.to_string())
}

pub const SIDEBAR_WIDTH: Pixels = px(180.0);
pub const SIDEBAR_COLLAPSED_WIDTH: Pixels = px(52.0);
pub const KEY_TREE_MIN_WIDTH: Pixels = px(330.0);
pub const KEY_TREE_MAX_WIDTH: Pixels = px(800.0);
pub const STATUS_BAR_HEIGHT: Pixels = px(35.0);
pub const EDITOR_KEY_BAR_HEIGHT: Pixels = px(40.0);
