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

use crate::assets::Assets;
use chrono::{Datelike, Local};
use gpui::{
    App, Bounds, Image, ImageFormat, TitlebarOptions, Window, WindowBounds, WindowKind, WindowOptions, prelude::*, px,
    size,
};
use std::process::Command;
use std::sync::Arc;
use zedis_ui::{AboutConfig, AboutLine, AboutLink, ZedisAboutPage};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const GIT_SHA: &str = env!("VERGEN_GIT_SHA");

fn get_basic_gpu_info() -> String {
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = Command::new("system_profiler").arg("SPDisplaysDataType").output() {
            let info = String::from_utf8_lossy(&output.stdout);
            for line in info.lines() {
                if line.contains("Chipset Model:") {
                    return line.replace("Chipset Model:", "").trim().to_string();
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(output) = Command::new("wmic")
            .args(["path", "win32_VideoController", "get", "name"])
            .output()
        {
            let info = String::from_utf8_lossy(&output.stdout);
            let lines: Vec<&str> = info.lines().filter(|l| !l.trim().is_empty()).collect();
            if lines.len() > 1 {
                return lines[1].trim().to_string();
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(output) = Command::new("sh").args(["-c", "lspci | grep -i vga"]).output() {
            let info = String::from_utf8_lossy(&output.stdout);
            if !info.trim().is_empty() {
                if let Some(desc) = info.split(": ").last() {
                    return desc.trim().to_string();
                }
                return info.trim().to_string();
            }
        }
    }

    "Unknown GPU".to_string()
}

fn collect_system_info(window: &Window) -> String {
    let os = os_info::get();
    let scale_factor = window.scale_factor();
    let locale = sys_locale::get_locale().unwrap_or_else(|| "unknown".into());
    let theme = window.appearance();

    let mut lines = vec![
        format!("Version: {VERSION}"),
        format!("Git SHA: {GIT_SHA}"),
        format!("OS: {} {}", os.os_type(), os.version()),
        format!("Arch: {}", os.architecture().unwrap_or("unknown")),
        format!("Locale: {locale}"),
        format!("Scale Factor: {scale_factor}"),
        format!("Theme: {theme:?}"),
    ];
    let gpu_info = get_basic_gpu_info();
    if !gpu_info.is_empty() {
        lines.push(format!("GPU: {gpu_info}"));
    }

    lines.join("\n")
}

fn build_config() -> AboutConfig {
    let year = Local::now().year().to_string();
    let years = if year == "2026" {
        "2026".to_string()
    } else {
        format!("2026 - {year}")
    };

    let logo = Assets::get("icon.png").map(|item| item.data).unwrap_or_default();
    let logo = Arc::new(Image::from_bytes(ImageFormat::Png, logo.to_vec()));

    AboutConfig {
        name: "Zedis".into(),
        logo,
        lines: vec![
            AboutLine::sm("A modern Redis client built with GPUI"),
            AboutLine::sm(format!("Version {VERSION}")),
            AboutLine::xs("Built with Rust & GPUI"),
            AboutLine::xs("Licensed under Apache License 2.0"),
            AboutLine::xs(format!("Git SHA: {GIT_SHA}")),
            AboutLine::xs(format!("© {years} Tree xie. All rights reserved.")),
        ],
        links: vec![
            AboutLink::new("github", "GitHub", "https://github.com/vicanso/zedis"),
            AboutLink::new("docs", "Documentation", "https://github.com/vicanso/zedis#readme"),
            AboutLink::new("issues", "Report Issue", "https://github.com/vicanso/zedis/issues"),
        ],
        system_info_collector: Some(Box::new(|window, _cx| collect_system_info(window))),
    }
}

pub fn open_about_window(cx: &mut App) {
    let width = px(600.);
    let height = px(500.);
    let window_size = size(width, height);

    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, window_size, cx))),
        is_movable: false,
        is_resizable: false,
        titlebar: Some(TitlebarOptions {
            title: Some("About Zedis".into()),
            appears_transparent: true,
            ..Default::default()
        }),
        focus: true,
        kind: WindowKind::Normal,
        ..Default::default()
    };

    let _ = cx.open_window(options, |_, cx| cx.new(|_cx| ZedisAboutPage::new(build_config())));
}
