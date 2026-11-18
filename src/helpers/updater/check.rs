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

//! Whether there is a newer release, on this build's channel, and what
//! changed since the running one: the manifest first, the API when there is
//! none, the release list for pre-releases and nightlies.

use super::manifest::{GithubRelease, Manifest, assets_from_api, pick_asset};
use super::*;

/// Always-current manifest, served by GitHub from the latest non-prerelease.
const MANIFEST_URL: &str = "https://github.com/vicanso/zedis/releases/latest/download/latest.json";

/// `/releases/latest` returns the most recent non-prerelease, non-draft release.
const LATEST_RELEASE_API: &str = "https://api.github.com/repos/vicanso/zedis/releases/latest";

/// Browser fallback when no manifest/asset is available.
const RELEASES_PAGE: &str = "https://github.com/vicanso/zedis/releases/latest";

/// The mirror has no per-release page: `/releases/tag/<tag>` and
/// `/releases/<tag>` both redirect to a repository *archive* (measured
/// 2026-09-20 against the live mirror), so the list is what a browser can
/// usefully be pointed at.
const GITEE_RELEASES_PAGE: &str = "https://gitee.com/vicanso/zedis/releases";

/// The release list, newest first, for the pre-release channel: tagged
/// pre-releases and the rolling `nightly` build both live here and never in
/// `/releases/latest`.
const RELEASE_LIST_API: &str = "https://api.github.com/repos/vicanso/zedis/releases?per_page=15";

/// The same list on the mirror, which answers oldest first unless asked.
const GITEE_RELEASE_LIST_API: &str = "https://gitee.com/api/v5/repos/vicanso/zedis/releases?per_page=15&direction=desc";

/// How many releases' notes one prompt carries, newest first. A build
/// further behind than this is shown the latest ten: the prompt is a dialog,
/// not a reader, and the release page has the rest.
pub(super) const MAX_NOTES_RELEASES: usize = 10;

/// The rolling build publish.yml recreates on every push to main.
const NIGHTLY_TAG: &str = "nightly";

/// A nightly published this soon after our own build time is this very
/// build finishing its uploads, not a newer one.
const NIGHTLY_GRACE: jiff::SignedDuration = jiff::SignedDuration::from_mins(15);

pub(super) const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Decide whether the latest release is newer than the running build.
///
/// Prefers the manifest (which yields a verifiable per-arch asset); on any
/// manifest error falls back to the API (version + page only). Returns
/// `Ok(None)` when already up to date. Blocking (`ureq`): **must** run on a
/// background task, never the UI thread.
pub fn fetch_latest_release(include_prerelease: bool) -> Result<Option<UpdateInfo>> {
    // The pre-release channel looks at the whole list first; a listing
    // failure falls through to the stable path rather than to silence.
    if include_prerelease {
        match fetch_from_release_list() {
            Ok(found) => return Ok(found),
            Err(e) => debug!(error = %e, "update check: release list unavailable, using the stable channel"),
        }
    }
    match fetch_from_manifest() {
        Ok(found) => Ok(found),
        Err(e) => {
            debug!(error = %e, "update check: manifest unavailable, falling back to API");
            fetch_from_api()
        }
    }
}

fn fetch_from_manifest() -> Result<Option<UpdateInfo>> {
    // The mirror answers only where GitHub could not, and costs a request
    // more there because Gitee has no `latest/download` shortcut.
    let (manifest, from_mirror) = match http_get_string(MANIFEST_URL) {
        Ok(text) => (serde_json::from_str::<Manifest>(&text)?, false),
        Err(origin) => {
            debug!(error = %origin, "update check: manifest unreachable, trying the mirror");
            (fetch_from_gitee_manifest()?, true)
        }
    };
    let Some(latest) = newer_version(&manifest.version)? else {
        return Ok(None);
    };
    let asset = pick_asset(&manifest.assets);
    // A page the user cannot reach is worse than no page: when GitHub is
    // what failed, point the browser at the mirror instead.
    let page_url = match (from_mirror, manifest.notes.trim().is_empty()) {
        (true, _) => GITEE_RELEASES_PAGE.to_string(),
        (false, true) => RELEASES_PAGE.to_string(),
        (false, false) => manifest.notes.clone(),
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
/// release-page URL, so the markdown takes one extra API call. Any failure
/// (offline API, rate limit) degrades to an empty string — the prompt then
/// shows version + link only, never an error. Runs at most once per
/// discovered update, well inside the anonymous API quota.
///
/// The call asks for the release *list*, because an update is often more
/// than one release away: a build three versions behind was shown the last
/// one's notes alone, and learned nothing of the two it was also about to
/// install. The single latest release is what is left when the list cannot
/// be had.
fn fetch_release_notes(version: &str) -> String {
    let since_current = || -> Result<String> {
        let (Ok(latest), Some(current)) = (Version::parse(version), current_version()) else {
            return Ok(String::new());
        };
        let (text, _) = http_get_string_mirrored(RELEASE_LIST_API, GITEE_RELEASE_LIST_API)?;
        let releases: Vec<GithubRelease> = serde_json::from_str(&text)?;
        Ok(notes_between(&releases, &current, &latest, false))
    };
    match since_current() {
        Ok(notes) if !notes.is_empty() => return notes,
        Ok(_) => debug!("update check: the release list does not have this version yet"),
        Err(e) => debug!(error = %e, "update check: release list unavailable for the notes"),
    }
    latest_release_notes(version)
}

/// The notes of the latest release alone — what the prompt showed before it
/// read the list, and still shows when the list cannot be had.
fn latest_release_notes(version: &str) -> String {
    let fetch = || -> Result<String> {
        // Both hosts name the same fields (`tag_name`, `body`), so one
        // deserializer serves either answer — Gitee omits what it has no
        // equivalent for and every such field is `#[serde(default)]`.
        let (text, _) = http_get_string_mirrored(LATEST_RELEASE_API, &format!("{GITEE_API}/releases/latest"))?;
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

/// The running build's version, when it parses.
fn current_version() -> Option<Version> {
    Version::parse(CURRENT_VERSION).ok()
}

/// A release's version, read off its tag; the rolling `nightly` has none.
fn release_version(release: &GithubRelease) -> Option<Version> {
    Version::parse(release.tag_name.trim_start_matches('v').trim()).ok()
}

/// What changed between two versions: the notes of every tagged release
/// newer than `current` and no newer than `latest`, newest first, as one
/// markdown text. Drafts are never part of it, pre-releases only on the
/// channel that installs them.
///
/// Empty when the list does not have `latest` itself — the list can lag a
/// release that is still being published, and notes that stop one version
/// short would pass for the whole changelog.
pub(super) fn notes_between(
    releases: &[GithubRelease],
    current: &Version,
    latest: &Version,
    include_prerelease: bool,
) -> String {
    let mut found: Vec<(Version, &str)> = releases
        .iter()
        .filter(|release| !release.draft && (include_prerelease || !release.prerelease))
        .filter_map(|release| {
            let version = release_version(release)?;
            (version > *current && version <= *latest).then_some((version, release.body.trim()))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0));
    if found.first().is_none_or(|(version, _)| version != latest) {
        return String::new();
    }
    found
        .into_iter()
        .take(MAX_NOTES_RELEASES)
        .filter(|(_, body)| !body.is_empty())
        .map(|(version, body)| titled_notes(&version, body))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// One release's notes under a heading that says which release they are.
/// The release workflow's notes open with their own (`## [0.12.3](…) - date`)
/// and are left as they are; anything else — a hand-written body — is given
/// one, or several releases' notes would run together.
fn titled_notes(version: &Version, body: &str) -> String {
    let names_itself = body
        .lines()
        .next()
        .is_some_and(|first| first.contains(&version.to_string()));
    if names_itself {
        body.to_string()
    } else {
        format!("## {version}\n\n{body}")
    }
}

/// The pre-release channel: walk the release list newest-first and take the
/// first non-draft entry that is newer than this build — a tagged release
/// (stable or pre-release, by version) or the `nightly` (by publish time,
/// which only a build older than it can be behind). Nothing newer → `None`.
fn fetch_from_release_list() -> Result<Option<UpdateInfo>> {
    let text = http_get_string(RELEASE_LIST_API)?;
    let releases: Vec<GithubRelease> = serde_json::from_str(&text)?;
    let current = current_version();
    // Every tagged release this build is behind, for the notes of whichever
    // entry is offered: the list is already here, so it costs no request.
    let notes_up_to = |latest: &Version| {
        current
            .as_ref()
            .map(|current| notes_between(&releases, current, latest, true))
            .unwrap_or_default()
    };
    for release in releases.iter().filter(|r| !r.draft) {
        let page_url = if release.html_url.trim().is_empty() {
            RELEASES_PAGE.to_string()
        } else {
            release.html_url.clone()
        };
        if release.tag_name == NIGHTLY_TAG {
            if !nightly_is_newer(&release.published_at, BUILD_TIMESTAMP) {
                continue;
            }
            let published = release.published_at.get(..10).unwrap_or(NIGHTLY_TAG);
            // The rolling build says only when it was built. It also carries
            // every tagged release since this one, so their notes follow.
            let tagged = releases
                .iter()
                .filter(|r| !r.draft)
                .filter_map(release_version)
                .max()
                .map(|newest| notes_up_to(&newest))
                .unwrap_or_default();
            let notes = [release.body.trim(), tagged.as_str()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            return Ok(Some(UpdateInfo {
                version: format!("{NIGHTLY_TAG} {published}"),
                current: current_version_label(),
                page_url,
                notes,
                asset: pick_asset(&assets_from_api(&release.assets)),
            }));
        }
        let Some(latest) = newer_version(&release.tag_name)? else {
            // Newest-first: the first entry that is not newer ends the walk
            // (an unparsable tag is skipped by `newer_version` the same way).
            continue;
        };
        // A tagged release carries a manifest with checksums; the API's
        // asset list is the fallback for one that has none yet.
        let manifest_url = format!(
            "https://github.com/vicanso/zedis/releases/download/{}/latest.json",
            release.tag_name
        );
        let asset = http_get_string(&manifest_url)
            .ok()
            .and_then(|text| serde_json::from_str::<Manifest>(&text).ok())
            .and_then(|manifest| pick_asset(&manifest.assets))
            .or_else(|| pick_asset(&assets_from_api(&release.assets)));
        let combined = release_version(release)
            .map(|version| notes_up_to(&version))
            .unwrap_or_default();
        let notes = if combined.is_empty() {
            release.body.trim().to_string()
        } else {
            combined
        };
        return Ok(Some(UpdateInfo {
            version: latest,
            current: current_version_label(),
            page_url,
            notes,
            asset,
        }));
    }
    Ok(None)
}

/// Whether a nightly published at `published_at` is a later build than
/// the one running (`built_at`, both RFC 3339), beyond the grace window.
pub(super) fn nightly_is_newer(published_at: &str, built_at: &str) -> bool {
    let (Ok(published), Ok(built)) = (
        published_at.trim().parse::<jiff::Timestamp>(),
        built_at.trim().parse::<jiff::Timestamp>(),
    ) else {
        return false;
    };
    published.duration_since(built) > NIGHTLY_GRACE
}

/// What the prompt calls the running build: the version, and for a nightly
/// its build date as well.
fn current_version_label() -> String {
    if is_nightly_build() {
        format!(
            "{CURRENT_VERSION} ({NIGHTLY_TAG} {})",
            BUILD_TIMESTAMP.get(..10).unwrap_or_default()
        )
    } else {
        CURRENT_VERSION.to_string()
    }
}

fn fetch_from_api() -> Result<Option<UpdateInfo>> {
    let (text, from_mirror) = http_get_string_mirrored(LATEST_RELEASE_API, &format!("{GITEE_API}/releases/latest"))?;
    let release: GithubRelease = serde_json::from_str(&text)?;
    if release.draft || release.prerelease {
        return Ok(None);
    }
    let Some(latest) = newer_version(&release.tag_name)? else {
        return Ok(None);
    };
    // The mirror sends no `html_url`, and a GitHub link is no use to whoever
    // just failed to reach GitHub.
    let page_url = match (from_mirror, release.html_url.trim().is_empty()) {
        (true, _) => GITEE_RELEASES_PAGE.to_string(),
        (false, true) => RELEASES_PAGE.to_string(),
        (false, false) => release.html_url,
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
pub(super) fn newer_version(raw: &str) -> Result<Option<String>> {
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

/// The manifest from the mirror: its `latest` release names the tag, and the
/// tag spells the attachment URL. Two requests, only ever on the path where
/// GitHub already failed.
fn fetch_from_gitee_manifest() -> Result<Manifest> {
    let text = http_get_string(&format!("{GITEE_API}/releases/latest"))?;
    let release: GithubRelease = serde_json::from_str(&text)?;
    let text = http_get_string(&gitee_download_url(&release.tag_name, "latest.json"))?;
    Ok(serde_json::from_str(&text)?)
}
