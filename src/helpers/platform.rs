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

//! What to call the machine this is running on.
//!
//! Three places want it — the crash context, the About page and the
//! diagnostics bundle — and each used to reach for `os_info` directly. That
//! crate reads `/etc/os-release`, `sw_vers` and the Windows registry, none of
//! which a browser tab has, so the reach is behind one function now.
//!
//! The web answer is deliberately vague. A page *could* be asked about
//! `navigator.userAgent`, but that string is the browser's fingerprint, and
//! putting it in a crash report or a diagnostics bundle would ship a user's
//! identity with a bug report (ADR 7 lists what leaves the machine, and this
//! would be a new entry). The build already knows it is the web build.

/// Enough to say which machine a report came from.
pub struct PlatformInfo {
    /// `"Macos"`, `"Windows"`, `"Web"`, …
    pub os_type: String,
    /// The OS release, or empty where there is nothing meaningful to give.
    pub version: String,
    /// `"arm64"`, `"x86_64"`, `"wasm32"`, …
    pub architecture: String,
}

impl PlatformInfo {
    /// `"<os>-<version>"`, or just the OS when there is no version.
    pub fn os_label(&self) -> String {
        if self.version.is_empty() {
            self.os_type.clone()
        } else {
            format!("{}-{}", self.os_type, self.version)
        }
    }
}

#[cfg(not(target_family = "wasm"))]
pub fn platform_info() -> PlatformInfo {
    let info = os_info::get();
    PlatformInfo {
        os_type: info.os_type().to_string(),
        version: info.version().to_string(),
        architecture: info.architecture().unwrap_or_default().to_string(),
    }
}

#[cfg(target_family = "wasm")]
pub fn platform_info() -> PlatformInfo {
    PlatformInfo {
        os_type: "Web".to_string(),
        version: String::new(),
        // `wasm32` — the one true fact available without asking the browser
        // about itself.
        architecture: std::env::consts::ARCH.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_can_always_name_the_platform() {
        let info = platform_info();
        assert!(!info.os_type.is_empty(), "a report with no OS is not a report");
        assert!(!info.architecture.is_empty());
    }

    #[test]
    fn the_label_drops_an_empty_version_instead_of_trailing_a_dash() {
        let bare = PlatformInfo {
            os_type: "Web".into(),
            version: String::new(),
            architecture: "wasm32".into(),
        };
        assert_eq!(bare.os_label(), "Web");
        let full = PlatformInfo {
            os_type: "Macos".into(),
            version: "15.0".into(),
            architecture: "arm64".into(),
        };
        assert_eq!(full.os_label(), "Macos-15.0");
    }
}
