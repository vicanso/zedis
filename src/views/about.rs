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
    App, Bounds, ClipboardItem, Image, ImageFormat, TitlebarOptions, Window, WindowBounds, WindowKind, WindowOptions,
    img, prelude::*, px, size,
};
use gpui_component::{
    ActiveTheme, Sizable, StyledExt, button::Button, h_flex, label::Label, scroll::ScrollableElement, v_flex,
};
use std::process::Command;
use std::sync::Arc;

pub fn get_basic_gpu_info() -> String {
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

struct About {
    system_info: Option<String>,
}

const VERSION: &str = env!("CARGO_PKG_VERSION");
const GIT_SHA: &str = env!("VERGEN_GIT_SHA");

impl About {
    fn collect_system_info(window: &Window, _cx: &Context<Self>) -> String {
        let os = os_info::get();
        let scale_factor = window.scale_factor();
        let locale = sys_locale::get_locale().unwrap_or_else(|| "unknown".into());
        let theme = window.appearance();

        let mut lines = vec![
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
}

impl Render for About {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let year = Local::now().year().to_string();
        let logo = Assets::get("icon.png").map(|item| item.data).unwrap_or_default();
        let logo = Arc::new(Image::from_bytes(ImageFormat::Png, logo.to_vec()));
        let logo_size = px(96.);
        let years = if year == "2026" {
            "2026".to_string()
        } else {
            format!("2026 - {year}")
        };
        v_flex()
            .size_full()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .bg(cx.theme().background)
            // LOGO
            .child(
                h_flex()
                    .items_center()
                    .justify_center()
                    .child(img(logo.clone()).w(logo_size).h(logo_size)),
            )
            // App Name
            .child(
                Label::new("Zedis")
                    .text_xl()
                    .font_semibold()
                    .text_color(cx.theme().primary),
            )
            // Description
            .child(
                Label::new("A modern Redis client built with GPUI")
                    .text_sm()
                    .text_color(cx.theme().muted_foreground),
            )
            // Version
            .child(
                Label::new(format!("Version {VERSION}"))
                    .text_sm()
                    .text_color(cx.theme().muted_foreground),
            )
            // Technology Stack
            .child(
                Label::new("Built with Rust & GPUI")
                    .text_xs()
                    .text_color(cx.theme().muted_foreground),
            )
            // License
            .child(
                Label::new("Licensed under Apache License 2.0")
                    .text_xs()
                    .text_color(cx.theme().muted_foreground),
            )
            // Git SHA
            .child(
                Label::new(format!("Git SHA: {GIT_SHA}"))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground),
            )
            // Copyright
            .child(
                Label::new(format!("© {years} Tree xie. All rights reserved."))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground),
            )
            // Links
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .mt_4()
                    .child(
                        Button::new("github")
                            .label("GitHub")
                            .small()
                            .on_click(move |_, _window, cx| {
                                cx.open_url("https://github.com/vicanso/zedis");
                            }),
                    )
                    .child(
                        Button::new("docs")
                            .label("Documentation")
                            .small()
                            .on_click(move |_, _window, cx| {
                                cx.open_url("https://github.com/vicanso/zedis#readme");
                            }),
                    )
                    .child(
                        Button::new("issues")
                            .label("Report Issue")
                            .small()
                            .on_click(move |_, _window, cx| {
                                cx.open_url("https://github.com/vicanso/zedis/issues");
                            }),
                    )
                    .child(
                        Button::new("sysinfo")
                            .label("System Info")
                            .small()
                            .on_click(cx.listener(|this, _, window, cx| {
                                let info = About::collect_system_info(window, cx);
                                this.system_info = Some(info);
                                cx.notify();
                            })),
                    ),
            )
            .when_some(self.system_info.clone(), |this, info| {
                let info_for_copy = info.clone();
                this.child(
                    v_flex()
                        .my_2()
                        .px_4()
                        .py_2()
                        .w(px(400.))
                        .rounded_md()
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().secondary)
                        .gap_1()
                        .relative()
                        .child(
                            h_flex().justify_end().absolute().right_2().top_2().child(
                                Button::new("copy-sysinfo")
                                    .label("Copy")
                                    .xsmall()
                                    .on_click(move |_, _window, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(info_for_copy.clone()));
                                    }),
                            ),
                        )
                        .children(info.lines().map(|line| {
                            Label::new(line.to_string())
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                        })),
                )
            })
            .overflow_y_scrollbar()
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

    let _ = cx.open_window(options, |_, cx| cx.new(|_cx| About { system_info: None }));
}
