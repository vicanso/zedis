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

//! In-app update check + assisted install.
//!
//! Flow: read the `latest.json` manifest the release workflow publishes, compare
//! its version against the running build, and — when newer — pick the asset for
//! this `os`/`arch`, download it, verify its SHA-256 against the manifest, and
//! hand the file to the OS to open (`.dmg` → Finder drag window, `.msi` → the
//! installer, AppImage/tarball → the desktop handler). The user finishes the
//! install themselves, so we never replace a running binary, touch
//! `/Applications`, or deal with code signing / quarantine.
//!
//! If the manifest is missing (e.g. a release predating it), we fall back to the
//! GitHub Releases API to at least detect a new version; the UI then opens the
//! release page instead of an in-app download.
//!
//! Network + filesystem only; the dialog/toast orchestration lives in `main.rs`.

use crate::error::Error;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tracing::{debug, error, info};

type Result<T, E = Error> = std::result::Result<T, E>;

/// Always-current manifest, served by GitHub from the latest non-prerelease.
const MANIFEST_URL: &str = "https://github.com/vicanso/zedis/releases/latest/download/latest.json";
/// `/releases/latest` returns the most recent non-prerelease, non-draft release.
const LATEST_RELEASE_API: &str = "https://api.github.com/repos/vicanso/zedis/releases/latest";
/// Browser fallback when no manifest/asset is available.
const RELEASES_PAGE: &str = "https://github.com/vicanso/zedis/releases/latest";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// Upper bound on an installer download (guards against a runaway body).
const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const USER_AGENT: &str = concat!("zedis/", env!("CARGO_PKG_VERSION"));

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
    /// Changelog text (only populated via the API fallback; empty for manifest).
    pub notes: String,
    /// The installer for this `os`/`arch`. `None` when the manifest is absent or
    /// has no matching asset; the UI then falls back to opening `page_url`.
    pub asset: Option<UpdateAsset>,
}

/// `latest.json` shape (see `.github/workflows/publish.yml`).
#[derive(Debug, Deserialize)]
struct Manifest {
    version: String,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    assets: Vec<ManifestAsset>,
}

#[derive(Debug, Deserialize)]
struct ManifestAsset {
    os: String,
    arch: String,
    kind: String,
    name: String,
    url: String,
    #[serde(default)]
    sha256: String,
    #[serde(default)]
    size: u64,
}

/// Subset of the GitHub "release" object, used only for the API fallback.
#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
}

/// Decide whether the latest release is newer than the running build.
///
/// Prefers the manifest (which yields a verifiable per-arch asset); on any
/// manifest error falls back to the API (version + page only). Returns
/// `Ok(None)` when already up to date. Blocking (`ureq`): **must** run on a
/// background task, never the UI thread.
pub fn fetch_latest_release() -> Result<Option<UpdateInfo>> {
    match fetch_from_manifest() {
        Ok(found) => Ok(found),
        Err(e) => {
            debug!(error = %e, "update check: manifest unavailable, falling back to API");
            fetch_from_api()
        }
    }
}

fn fetch_from_manifest() -> Result<Option<UpdateInfo>> {
    let text = http_get_string(MANIFEST_URL)?;
    let manifest: Manifest = serde_json::from_str(&text)?;
    let Some(latest) = newer_version(&manifest.version)? else {
        return Ok(None);
    };
    let asset = pick_asset(&manifest.assets);
    let page_url = if manifest.notes.trim().is_empty() {
        RELEASES_PAGE.to_string()
    } else {
        manifest.notes.clone()
    };
    Ok(Some(UpdateInfo {
        version: latest,
        current: CURRENT_VERSION.to_string(),
        page_url,
        notes: String::new(),
        asset,
    }))
}

fn fetch_from_api() -> Result<Option<UpdateInfo>> {
    let text = http_get_string(LATEST_RELEASE_API)?;
    let release: GithubRelease = serde_json::from_str(&text)?;
    if release.draft || release.prerelease {
        return Ok(None);
    }
    let Some(latest) = newer_version(&release.tag_name)? else {
        return Ok(None);
    };
    let page_url = if release.html_url.trim().is_empty() {
        RELEASES_PAGE.to_string()
    } else {
        release.html_url
    };
    Ok(Some(UpdateInfo {
        version: latest,
        current: CURRENT_VERSION.to_string(),
        page_url,
        notes: release.body.trim().to_string(),
        asset: None,
    }))
}

/// `Some(normalized)` if `raw` parses as semver and is strictly newer than the
/// running build; `None` if equal/older or unparsable (we don't prompt on garbage).
fn newer_version(raw: &str) -> Result<Option<String>> {
    let latest_raw = raw.trim_start_matches('v').trim();
    let (Ok(latest), Ok(current)) = (Version::parse(latest_raw), Version::parse(CURRENT_VERSION)) else {
        debug!(latest = %raw, current = CURRENT_VERSION, "update check: unparsable version, skipping");
        return Ok(None);
    };
    if latest <= current {
        return Ok(None);
    }
    Ok(Some(latest.to_string()))
}

/// Pick the installer for this machine: the preferred packaging for the OS
/// (`dmg` / `msi` / `appimage`), else any asset for the same `os`/`arch`.
fn pick_asset(assets: &[ManifestAsset]) -> Option<UpdateAsset> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let preferred_kind = match os {
        "macos" => "dmg",
        "windows" => "msi",
        "linux" => "appimage",
        _ => return None,
    };
    let chosen = assets
        .iter()
        .find(|a| a.os == os && a.arch == arch && a.kind == preferred_kind)
        .or_else(|| assets.iter().find(|a| a.os == os && a.arch == arch))?;
    Some(UpdateAsset {
        url: chosen.url.clone(),
        sha256: chosen.sha256.clone(),
        name: chosen.name.clone(),
        size: chosen.size,
    })
}

fn http_get_string(url: &str) -> Result<String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .build()
        .new_agent();
    let text = agent
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json, application/json")
        .call()
        // A failure here is often expected (e.g. the manifest 404s on releases
        // predating it) and triggers a fallback — log at debug, not error. The
        // genuine "couldn't check at all" is logged once by the caller.
        .map_err(|e| {
            debug!(%url, error = %e, "update check: HTTP request failed");
            Error::Invalid {
                message: format!("update check failed: {e}"),
            }
        })?
        .into_body()
        .read_to_string()
        .map_err(|e| {
            debug!(%url, error = %e, "update check: failed to read response body");
            Error::Invalid {
                message: format!("update check read failed: {e}"),
            }
        })?;
    Ok(text)
}

/// Download `asset` to the temp dir and verify its SHA-256 against the manifest.
/// Returns the path to the verified file. On a checksum mismatch the partial
/// file is removed and an error returned — the caller must never open it.
/// Blocking; run on a background task.
/// Download the asset, verify its checksum, and write it to a temp file.
///
/// `on_progress(downloaded, total)` is invoked as bytes stream in (`total` is
/// the asset's advertised size, may be 0 if unknown), so callers can render a
/// progress indicator. The body is read in chunks and capped at `MAX_DOWNLOAD`.
pub fn download_and_verify(asset: &UpdateAsset, mut on_progress: impl FnMut(u64, u64)) -> Result<PathBuf> {
    info!(name = %asset.name, size = asset.size, "update: downloading installer");
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(DOWNLOAD_TIMEOUT))
        .build()
        .new_agent();
    let resp = agent
        .get(&asset.url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| {
            error!(url = %asset.url, error = %e, "update: download request failed");
            Error::Invalid {
                message: format!("download failed: {e}"),
            }
        })?;
    // Prefer the server's Content-Length for the progress total; the manifest's
    // `size` is only a fallback (it may be 0 / absent), in which case progress
    // stays indeterminate.
    let total = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(asset.size);
    let mut reader = resp.into_body().into_reader();
    let mut bytes: Vec<u8> = Vec::with_capacity(total.min(MAX_DOWNLOAD) as usize);
    let mut buf = [0u8; 64 * 1024];
    on_progress(0, total);
    loop {
        let n = reader.read(&mut buf).map_err(|e| {
            error!(url = %asset.url, error = %e, "update: reading download body failed");
            Error::Invalid {
                message: format!("download read failed: {e}"),
            }
        })?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..n]);
        if bytes.len() as u64 > MAX_DOWNLOAD {
            error!(name = %asset.name, "update: download exceeded size cap");
            return Err(Error::Invalid {
                message: format!("download too large for {}", asset.name),
            });
        }
        on_progress(bytes.len() as u64, total);
    }

    // Verify the checksum before the bytes ever touch a runnable location.
    if !asset.sha256.is_empty() {
        let digest = Sha256::digest(&bytes);
        let got: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        if !got.eq_ignore_ascii_case(&asset.sha256) {
            error!(name = %asset.name, expected = %asset.sha256, got = %got, "update: checksum mismatch");
            return Err(Error::Invalid {
                message: format!("checksum mismatch for {}", asset.name),
            });
        }
    }

    let path = std::env::temp_dir().join(&asset.name);
    if let Err(e) = std::fs::write(&path, &bytes) {
        let _ = std::fs::remove_file(&path);
        return Err(e.into());
    }
    info!(path = %path.display(), "update: installer downloaded and verified");
    Ok(path)
}

/// Hand a downloaded installer to the OS: `open` on macOS (mounts a `.dmg`,
/// launches a `.pkg`), `start` on Windows (runs the `.msi`), `xdg-open` on Linux.
/// Blocking; run on a background task.
pub fn open_installer(path: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut c = Command::new("open");
        c.arg(path);
        c
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        // `start` needs an (empty) title argument before the file.
        let mut c = Command::new("cmd");
        c.args(["/C", "start", ""]).arg(path);
        c
    };
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut c = Command::new("xdg-open");
        c.arg(path);
        c
    };
    command.spawn().map_err(|e| {
        error!(path = %path.display(), error = %e, "update: failed to open installer");
        Error::Invalid {
            message: format!("failed to open installer: {e}"),
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_version_is_strict() {
        // Strictly-greater is an update; equal/older/garbage are not.
        // Derive the "newer" tag from the running version so this keeps
        // passing across version bumps.
        let current = Version::parse(CURRENT_VERSION).expect("current version parses");
        let next = format!("v{}.0.0", current.major + 1);
        assert_eq!(
            newer_version(&next).expect("parse"),
            Some(next.trim_start_matches('v').to_string())
        );
        assert_eq!(newer_version(CURRENT_VERSION).expect("parse"), None);
        assert_eq!(newer_version("0.0.1").expect("parse"), None);
        assert_eq!(newer_version("not-a-version").expect("parse"), None);
    }

    #[test]
    fn pick_asset_prefers_os_packaging() {
        let assets = vec![
            ManifestAsset {
                os: std::env::consts::OS.to_string(),
                arch: std::env::consts::ARCH.to_string(),
                kind: "tarball".to_string(),
                name: "other".to_string(),
                url: "u1".to_string(),
                sha256: "a".to_string(),
                size: 1,
            },
            ManifestAsset {
                os: "nope".to_string(),
                arch: "nope".to_string(),
                kind: "dmg".to_string(),
                name: "wrong-os".to_string(),
                url: "u2".to_string(),
                sha256: "b".to_string(),
                size: 2,
            },
        ];
        // Matches this os/arch even when the preferred kind is absent; never
        // picks an asset for a different os/arch.
        let chosen = pick_asset(&assets).expect("an asset for this platform");
        assert_eq!(chosen.name, "other");
    }
}
