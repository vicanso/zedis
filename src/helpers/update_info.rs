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

//! What an update *is*, apart from how it is fetched and installed.
//!
//! These three types are what the title-bar chip, the update dialog and the
//! app state hold; `updater.rs` — the blocking HTTP, the checksum, the
//! installer hand-off — is desktop only and not compiled for the browser at
//! all (ADR 9). Keeping the types here means the UI that *shows* an update
//! compiles everywhere, while nothing in a tab can ever go and get one.

/// The installer asset matching this machine's `os`/`arch`, with the checksum to
/// verify it after download.
#[derive(Debug, Clone)]
pub struct UpdateAsset {
    pub url: String,
    pub sha256: String,
    pub name: String,
    pub size: u64,
}

/// A release that is newer than the one currently running.
#[derive(Debug, Clone)]
pub struct UpdateInfo {
    /// Latest version, normalized without a leading `v` (e.g. `0.5.0`).
    pub version: String,
    /// The running version (e.g. `0.4.4`).
    pub current: String,
    /// Release page to open in a browser — used as the changelog link and as the
    /// fallback "download" target when no verified asset is available.
    pub page_url: String,
    /// Changelog markdown. The manifest only carries a release-page URL, so
    /// this is filled by a best-effort extra GitHub API call (see
    /// `fetch_release_notes`); empty when that call fails.
    pub notes: String,
    /// The installer for this `os`/`arch`. `None` when the manifest is absent or
    /// has no matching asset; the UI then falls back to opening `page_url`.
    pub asset: Option<UpdateAsset>,
}

/// What [`install_update`] did with the verified installer.
pub enum Delivery {
    /// The fresh bundle was copied over the running one — a relaunch
    /// ([`relaunch`]) completes the update. Only the macOS in-place path
    /// constructs this, so the variant (like its match arm) is compiled
    /// out elsewhere.
    #[cfg(target_os = "macos")]
    Replaced,
    /// The installer was handed to the OS (Finder drag window / msiexec /
    /// desktop handler) — the user finishes the install.
    HandedToOs,
}
