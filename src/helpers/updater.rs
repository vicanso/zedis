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
//! install it. On macOS [`install_update`] completes the install in place when
//! it can: mount the DMG silently, verify the bundle identifier, copy the
//! bundle over the running one (the old bundle is renamed aside into temp,
//! never deleted while the process running from it is alive), detach — then
//! [`relaunch`] restarts into the new copy. Anything that blocks that path — a
//! bare `cargo run` with no bundle, an unwritable `/Applications`, a foreign
//! bundle on the image — degrades to handing the file to the OS (`.dmg` →
//! Finder drag window, `.msi` → the installer, AppImage/tarball → the desktop
//! handler), the flow that shipped before and works from anywhere. The
//! manifest checksum is the integrity story: ureq writes no quarantine xattr,
//! so Gatekeeper never re-inspects the copy — what the SHA-256 vouched for is
//! what runs.
//!
//! If the manifest is missing (e.g. a release predating it), we fall back to the
//! GitHub Releases API to at least detect a new version; the UI then opens the
//! release page instead of an in-app download.
//!
//! Network + filesystem only; the dialog/toast orchestration lives in `main.rs`.

use super::proxy::app_proxy;
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
    /// Changelog markdown. The manifest only carries a release-page URL, so
    /// this is filled by a best-effort extra GitHub API call (see
    /// `fetch_release_notes`); empty when that call fails.
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
        notes: fetch_release_notes(&latest),
        version: latest,
        current: CURRENT_VERSION.to_string(),
        page_url,
        asset,
    }))
}

/// Best-effort changelog for the update prompt: latest.json only carries a
/// release-page URL, so the markdown body takes one extra GitHub API call.
/// Any failure (offline API, rate limit) degrades to an empty string — the
/// prompt then shows version + link only, never an error. Runs at most once
/// per discovered update, well inside the anonymous API quota.
fn fetch_release_notes(version: &str) -> String {
    let fetch = || -> Result<String> {
        let text = http_get_string(LATEST_RELEASE_API)?;
        let release: GithubRelease = serde_json::from_str(&text)?;
        // The API's "latest" can briefly disagree with the manifest (CDN
        // caching, mid-publish) — only trust the body when both name the
        // same version, otherwise the prompt would show the wrong changelog.
        if release.tag_name.trim_start_matches('v').trim() != version {
            return Ok(String::new());
        }
        Ok(release.body.trim().to_string())
    };
    match fetch() {
        Ok(notes) => notes,
        Err(e) => {
            debug!(error = %e, "update check: release notes unavailable");
            String::new()
        }
    }
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
        // Env-var proxy plus the OS system proxy — a Dock-launched app has
        // no shell environment, so without this a proxied network (where
        // github.com is often unreachable directly) never gets updates.
        .proxy(app_proxy())
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
        .proxy(app_proxy())
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

/// Whether finishing the install needs Zedis to quit — the answer differs per
/// platform because "installing" means something different on each:
///
/// * **macOS** (`.dmg`): the user drags the new `Zedis.app` over the running one
///   in `/Applications`. The live process has the old bundle's pages mapped, so
///   replacing it underneath can fault it (bad code signature / `SIGBUS`).
/// * **Windows** (`.msi`): msiexec cannot replace a running `zedis.exe`; it
///   raises the "files in use" prompt (or demands a reboot) instead.
/// * **Linux** (AppImage / tarball): not an installer at all — nothing needs the
///   process gone, and quitting would strand the user with no new version.
pub const fn installer_requires_quit() -> bool {
    cfg!(not(target_os = "linux"))
}

/// Bring the installer's own UI forward, right before Zedis quits.
///
/// On macOS the `.dmg` is handed to LaunchServices, which mounts it and has
/// **Finder** open the drag-to-Applications window. Quitting gives focus to
/// whichever app was active before Zedis (a terminal, an editor…) rather than to
/// Finder, so the window the user is supposed to act on ends up buried behind
/// everything. `open -a Finder` activates it through LaunchServices — no
/// AppleScript, so no "wants to control Finder" permission prompt.
///
/// Windows' msiexec raises its own foreground window, and Linux never quits
/// here, so both are no-ops.
pub fn focus_installer_ui() {
    #[cfg(target_os = "macos")]
    if let Err(e) = Command::new("open").args(["-a", "Finder"]).spawn() {
        debug!(error = %e, "update: could not activate Finder for the installer window");
    }
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

    let open_failed = |e: std::io::Error| {
        error!(path = %path.display(), error = %e, "update: failed to open installer");
        Error::Invalid {
            message: format!("failed to open installer: {e}"),
        }
    };

    // macOS / Windows: *wait* for the launcher to return, because the caller
    // quits right after (see `installer_requires_quit`) and must not race it.
    // Neither waits for the install itself — `open` returns once LaunchServices
    // has the disk image, `cmd /C start` once msiexec is launched.
    #[cfg(not(target_os = "linux"))]
    {
        let status = command.status().map_err(open_failed)?;
        if !status.success() {
            error!(path = %path.display(), %status, "update: installer launcher exited non-zero");
            return Err(Error::Invalid {
                message: format!("failed to open installer: {status}"),
            });
        }
    }
    // Linux: `xdg-open` can block until the handler it picked exits (some
    // desktop fallbacks do), and we never quit here — so fire and forget.
    #[cfg(target_os = "linux")]
    command.spawn().map_err(open_failed)?;

    Ok(())
}

// ---- in-place install (macOS) ------------------------------------------

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

/// Install the verified file. macOS installs in place when possible (see
/// the module docs); every other outcome and platform degrades to
/// [`open_installer`]. Blocking (hdiutil and ditto take seconds) — run on
/// a background task.
pub fn install_update(installer: &Path) -> Result<Delivery> {
    #[cfg(target_os = "macos")]
    {
        match running_bundle() {
            Some(target) => match install_over(&target, installer) {
                Ok(()) => return Ok(Delivery::Replaced),
                Err(e) => {
                    tracing::warn!(error = %e, "update: in-place install fell back to the drag window");
                }
            },
            None => info!("update: no running bundle (bare binary); opening the image to install"),
        }
    }
    open_installer(installer)?;
    Ok(Delivery::HandedToOs)
}

/// Quit-and-restart after a [`Delivery::Replaced`] install. The restart is
/// handed to a detached `sh` that waits for this pid to exit and then
/// `open`s the bundle — the path rides in `$0`, so no quoting happens
/// inside the script. The caller quits right after; the shell outlives us
/// as launchd's orphan, so it is never a zombie of ours.
#[cfg(target_os = "macos")]
pub fn relaunch() {
    let Some(bundle) = running_bundle() else {
        return;
    };
    let script = format!(
        "while /bin/kill -0 {pid} 2>/dev/null; do /bin/sleep 0.1; done; /usr/bin/open \"$0\"",
        pid = std::process::id()
    );
    match Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .arg(bundle.as_os_str())
        .spawn()
    {
        Ok(_) => info!(bundle = %bundle.display(), "update: restart requested to finish the update"),
        Err(e) => tracing::warn!(error = %e, "update: could not spawn the relauncher"),
    }
}

/// The identity gate for the in-place swap — must match
/// `[package.metadata.bundle] identifier` in Cargo.toml (a test guards
/// the pair).
#[cfg(target_os = "macos")]
const BUNDLE_ID: &str = "com.bigtree.zedis";

/// The bundle's name, on the DMG and on disk.
#[cfg(target_os = "macos")]
const BUNDLE_NAME: &str = "Zedis.app";

/// The bundle this process runs from — `…/Zedis.app` for the installed
/// app, `None` under bare `cargo run`. `current_exe` reports the path
/// recorded at exec time, so after an in-place install it names the *new*
/// copy at the same location — exactly what a relaunch wants, and why
/// `replace_bundle` may move the file it points at.
#[cfg(target_os = "macos")]
fn running_bundle() -> Option<PathBuf> {
    bundle_root_of(&std::env::current_exe().ok()?)
}

/// `…/Foo.app/Contents/MacOS/foo` → `…/Foo.app`.
#[cfg(target_os = "macos")]
fn bundle_root_of(exe: &Path) -> Option<PathBuf> {
    let root = exe.parent()?.parent()?.parent()?;
    (root.extension().is_some_and(|ext| ext == "app")).then(|| root.to_path_buf())
}

/// Mount, replace `target`, detach. The volume is detached on every exit —
/// a failed copy must not leave the image mounted on top of the failure it
/// just reported.
#[cfg(target_os = "macos")]
fn install_over(target: &Path, dmg: &Path) -> Result<()> {
    let volume = attach(dmg)?;
    let result = replace_bundle(target, &volume);
    detach(&volume);
    result
}

/// Swap `target` for the bundle on the mounted volume. The old bundle is
/// renamed aside into the temp directory, never deleted: the running
/// process keeps every file it might still fault in, and the OS prunes
/// temp on its own schedule. The rename doubles as the permission gate —
/// an unwritable `/Applications` fails it before anything has moved. A
/// failed copy renames the old bundle straight back.
#[cfg(target_os = "macos")]
fn replace_bundle(target: &Path, volume: &Path) -> Result<()> {
    let fresh = volume.join(BUNDLE_NAME);
    if bundle_plist_value(&fresh, "CFBundleIdentifier").as_deref() != Some(BUNDLE_ID) {
        return Err(Error::Invalid {
            message: "the mounted image does not carry our bundle".to_string(),
        });
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let aside = std::env::temp_dir().join(format!("zedis-previous-{}-{stamp}.app", std::process::id()));
    std::fs::rename(target, &aside).map_err(|e| Error::Invalid {
        message: format!("could not move the old bundle aside: {e}"),
    })?;
    let copied = Command::new("/usr/bin/ditto")
        .arg(fresh.as_os_str())
        .arg(target.as_os_str())
        .output();
    let failure = match &copied {
        Ok(out) if out.status.success() => {
            info!(target = %target.display(), "update: installed in place");
            return Ok(());
        }
        Ok(out) => String::from_utf8_lossy(&out.stderr).trim().to_string(),
        Err(e) => e.to_string(),
    };
    if let Err(e) = std::fs::rename(&aside, target) {
        error!(aside = %aside.display(), error = %e, "update: could not restore the old bundle");
    }
    Err(Error::Invalid {
        message: format!("ditto failed: {failure}"),
    })
}

/// Mount the image without a Finder window and return its mount point,
/// parsed from hdiutil's own plist output. `-noverify` because the
/// image's bytes were already vouched for: `download_and_verify` hashed
/// the whole file against the manifest, and hdiutil's default pass
/// re-reads the entire image to answer the same question.
#[cfg(target_os = "macos")]
fn attach(dmg: &Path) -> Result<PathBuf> {
    let out = Command::new("hdiutil")
        .args(["attach", "-nobrowse", "-noverify", "-plist"])
        .arg(dmg.as_os_str())
        .output()
        .map_err(|e| Error::Invalid { message: e.to_string() })?;
    if !out.status.success() {
        return Err(Error::Invalid {
            message: format!("hdiutil attach: {}", String::from_utf8_lossy(&out.stderr).trim()),
        });
    }
    mount_point_from_plist(&String::from_utf8_lossy(&out.stdout)).ok_or_else(|| Error::Invalid {
        message: "no mount point in hdiutil's output".to_string(),
    })
}

/// The `mount-point` string out of `hdiutil attach -plist` — the one
/// value needed, scanned without a plist parser. The volume name is ours
/// and ASCII ("Zedis Installer"), so XML entity escapes cannot occur in
/// the value.
#[cfg(target_os = "macos")]
fn mount_point_from_plist(xml: &str) -> Option<PathBuf> {
    let after = xml.split("<key>mount-point</key>").nth(1)?;
    let start = after.find("<string>")? + "<string>".len();
    let end = start + after[start..].find("</string>")?;
    Some(PathBuf::from(&after[start..end]))
}

/// Detach the installer volume, with one retry after a beat — hdiutil
/// answers "resource busy" while Spotlight is still indexing the fresh
/// mount. A volume that stays stuck is left with a warning; the user can
/// eject it from Finder.
#[cfg(target_os = "macos")]
fn detach(volume: &Path) {
    for attempt in 0..2 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_secs(1));
        }
        let detached = Command::new("hdiutil")
            .arg("detach")
            .arg(volume.as_os_str())
            .output()
            .is_ok_and(|out| out.status.success());
        if detached {
            return;
        }
    }
    tracing::warn!(volume = %volume.display(), "update: installer image left mounted");
}

/// One string key out of a bundle's Info.plist, via `defaults read`
/// (which handles both XML and binary plists). `None` for a missing
/// bundle, key, or a failed spawn — every caller treats those alike.
#[cfg(target_os = "macos")]
fn bundle_plist_value(app: &Path, key: &str) -> Option<String> {
    let out = Command::new("defaults")
        .arg("read")
        // Sans extension — `defaults` appends ".plist" itself.
        .arg(app.join("Contents/Info").as_os_str())
        .arg(key)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8(out.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
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

    /// The in-place swap ejects/replaces on the strength of this
    /// identifier — if the bundle id ever moves in Cargo.toml, the const
    /// must move with it or the swap goes blind (a mismatch only makes it
    /// fall back to the drag window, never install the wrong bundle).
    #[cfg(target_os = "macos")]
    #[test]
    fn the_install_identifier_matches_the_manifest() {
        let manifest = include_str!("../../Cargo.toml");
        assert!(manifest.contains(&format!("identifier = \"{BUNDLE_ID}\"")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mount_point_comes_out_of_hdiutil_plist_output() {
        // Trimmed real output: the disk entity has no mount-point key,
        // the filesystem entity carries it.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>system-entities</key><array>
  <dict>
    <key>content-hint</key><string>GUID_partition_scheme</string>
    <key>dev-entry</key><string>/dev/disk5</string>
  </dict>
  <dict>
    <key>content-hint</key><string>Apple_HFS</string>
    <key>dev-entry</key><string>/dev/disk5s1</string>
    <key>mount-point</key>
    <string>/Volumes/Zedis Installer</string>
  </dict>
</array></dict></plist>"#;
        assert_eq!(
            mount_point_from_plist(xml),
            Some(PathBuf::from("/Volumes/Zedis Installer"))
        );
        assert_eq!(mount_point_from_plist("<plist></plist>"), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn bundle_root_is_the_app_directory_or_nothing() {
        assert_eq!(
            bundle_root_of(Path::new("/Applications/Zedis.app/Contents/MacOS/Zedis")),
            Some(PathBuf::from("/Applications/Zedis.app"))
        );
        // Bare `cargo run` has no bundle to replace.
        assert_eq!(bundle_root_of(Path::new("/Users/x/proj/target/debug/zedis")), None);
    }

    /// Build a DMG whose payload is `Zedis.app` carrying `id` as its
    /// bundle identifier, under `dir`. Real hdiutil, ~a second.
    #[cfg(target_os = "macos")]
    fn fixture_dmg(dir: &Path, id: &str, marker: &[u8], volname: &str) -> PathBuf {
        let contents = dir.join("payload").join(BUNDLE_NAME).join("Contents");
        std::fs::create_dir_all(contents.join("MacOS")).expect("mkdir");
        std::fs::write(contents.join("MacOS/Zedis"), marker).expect("write binary");
        std::fs::write(
            contents.join("Info.plist"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>{id}</string>
</dict></plist>
"#
            ),
        )
        .expect("write plist");
        let dmg = dir.join("update.dmg");
        let created = Command::new("hdiutil")
            .args(["create", "-srcfolder"])
            .arg(dir.join("payload").as_os_str())
            .args(["-volname", volname, "-format", "UDZO", "-quiet"])
            .arg(dmg.as_os_str())
            .output()
            .expect("hdiutil create runs");
        assert!(created.status.success(), "hdiutil create failed");
        dmg
    }

    /// An old bundle standing where the install will land.
    #[cfg(target_os = "macos")]
    fn fixture_target(dir: &Path) -> PathBuf {
        let target = dir.join("Applications").join(BUNDLE_NAME);
        std::fs::create_dir_all(target.join("Contents/MacOS")).expect("mkdir target");
        std::fs::write(target.join("Contents/MacOS/Zedis"), b"old build").expect("write old");
        target
    }

    /// The whole in-place path against a real image: mount, verify the
    /// bundle id, rename the old bundle aside (kept, not deleted), copy
    /// the new one in, detach the volume.
    #[cfg(target_os = "macos")]
    #[test]
    fn in_place_install_swaps_the_bundle_and_detaches() {
        let dir = std::env::temp_dir().join(format!("zedis-inplace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let volname = "zedis-inplace-test";
        let dmg = fixture_dmg(&dir, BUNDLE_ID, b"new build", volname);
        let target = fixture_target(&dir);

        install_over(&target, &dmg).expect("in-place install");

        assert_eq!(
            std::fs::read(target.join("Contents/MacOS/Zedis")).expect("read new"),
            b"new build"
        );
        assert!(
            !Path::new("/Volumes").join(volname).exists(),
            "the volume must be detached"
        );
        // The old bundle was parked in temp, not destroyed — the running
        // process may still fault pages in from it.
        let prefix = format!("zedis-previous-{}-", std::process::id());
        let parked: Vec<PathBuf> = std::fs::read_dir(std::env::temp_dir())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(&prefix))
            })
            .collect();
        assert!(
            parked
                .iter()
                .any(|p| std::fs::read(p.join("Contents/MacOS/Zedis")).is_ok_and(|bytes| bytes == b"old build")),
            "the old bundle must survive in temp"
        );
        for p in parked {
            let _ = std::fs::remove_dir_all(p);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The identity gate fires before anything moves — and the volume
    /// still gets detached on the failure exit.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_foreign_bundle_is_refused_before_anything_moves() {
        let dir = std::env::temp_dir().join(format!("zedis-foreign-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let volname = "zedis-foreign-test";
        let dmg = fixture_dmg(&dir, "com.example.stranger", b"impostor", volname);
        let target = fixture_target(&dir);

        let refused = install_over(&target, &dmg);

        assert!(refused.is_err(), "a foreign identifier must be refused");
        assert_eq!(
            std::fs::read(target.join("Contents/MacOS/Zedis")).expect("read old"),
            b"old build",
            "the standing install must be untouched"
        );
        assert!(
            !Path::new("/Volumes").join(volname).exists(),
            "the volume must be detached even on refusal"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
