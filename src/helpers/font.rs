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

use gpui::SharedString;

pub fn get_mono_font_family() -> String {
    // Bundled and registered at startup via `add_fonts` (see `main.rs` +
    // `assets/fonts/JetBrainsMono-*.ttf`), so it renders identically on every
    // platform with real Regular/Bold faces — unlike the OS monospace
    // fonts we used before, whose weight resolution varied.
    "JetBrains Mono".to_string()
}

pub fn get_default_font_family() -> SharedString {
    #[cfg(target_os = "macos")]
    {
        ".AppleSystemUIFont, PingFang SC, Helvetica Neue".into()
    }

    #[cfg(target_os = "windows")]
    {
        // 确保你的 add_fonts 已经把 HarmonyOS 或其他中文字体加载进去了
        "Segoe UI, HarmonyOS Sans SC, Microsoft YaHei UI".into()
    }

    #[cfg(target_os = "linux")]
    {
        "Ubuntu, Noto Sans CJK SC".into()
    }
}
