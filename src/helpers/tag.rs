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

use gpui::{Hsla, hsla};

/// Resolve a preset tag color key (`gray` / `blue` / `green` / `amber` / `red`)
/// into an HSLA color suitable for chips and side bars.
///
/// Returns `None` for `None`, empty string, or `"none"`. Unknown keys fall back
/// to gray so that future presets do not blow up older clients.
pub fn resolve_tag_color(key: Option<&str>) -> Option<Hsla> {
    let raw = key?.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("none") {
        return None;
    }
    Some(match raw.to_ascii_lowercase().as_str() {
        "red" => hsla(0.0, 0.65, 0.50, 1.0),
        "amber" => hsla(0.10, 0.75, 0.50, 1.0),
        "green" => hsla(0.32, 0.55, 0.42, 1.0),
        "blue" => hsla(0.58, 0.55, 0.50, 1.0),
        _ => hsla(0.0, 0.0, 0.45, 1.0),
    })
}
