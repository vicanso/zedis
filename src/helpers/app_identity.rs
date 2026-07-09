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

//! Application name / Wayland app_id / window icon identity.
//!
//! Linux compositors (especially KDE + Wayland) show a generic "Wayland (W)"
//! icon and an empty title when the window never sets `xdg_toplevel` title /
//! `app_id`. See issue #106.

use crate::constants::{APP_NAME, linux_app_id};
use gpui::{SharedString, TitlebarOptions, WindowOptions};
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use std::sync::Arc;

/// Decode the bundled 512×512 PNG once for X11 `_NET_WM_ICON` (GPUI's
/// `WindowOptions::icon` is X11-only; Wayland resolves the icon via
/// `app_id` + the installed `.desktop` / hicolor theme).
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub fn app_window_icon() -> Option<Arc<image::RgbaImage>> {
    static ICON: std::sync::OnceLock<Option<Arc<image::RgbaImage>>> = std::sync::OnceLock::new();
    ICON.get_or_init(|| {
        let bytes = include_bytes!("../../assets/icon.png");
        image::load_from_memory(bytes).ok().map(|img| Arc::new(img.to_rgba8()))
    })
    .clone()
}

/// Fill in Linux identity fields on a [`WindowOptions`] without clobbering
/// caller-supplied title / app_id / icon.
///
/// Call this for every window so task switchers group Zedis windows together
/// and secondary windows don't fall back to the generic Wayland icon.
pub fn with_app_identity(mut options: WindowOptions) -> WindowOptions {
    if options.app_id.is_none() {
        options.app_id = Some(linux_app_id());
    }

    // Ensure the platform titlebar has a non-empty title. On macOS/Windows we
    // draw a custom title bar (`appears_transparent`), but the OS still uses
    // this string in the task switcher / window list. On Linux the server-side
    // decoration shows it directly.
    match &mut options.titlebar {
        Some(titlebar) if titlebar.title.is_none() => {
            titlebar.title = Some(SharedString::from(APP_NAME));
        }
        None => {
            options.titlebar = Some(TitlebarOptions {
                title: Some(SharedString::from(APP_NAME)),
                ..Default::default()
            });
        }
        _ => {}
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    if options.icon.is_none() {
        options.icon = app_window_icon();
    }

    options
}
