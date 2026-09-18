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

//! `fs` for a build that has no file system.
//!
//! A page has no config directory, no home directory and no way to write a
//! file, and everything that would use one — the server list, the local
//! database, the diagnostics bundle, the script viewer — is the bridge's job
//! on the web (ADR 9).
//!
//! So this is the same module surface answering honestly: `NotFound` for the
//! directories that do not exist, `Unsupported` for the writes that cannot
//! happen. **Not** silent success, which would let a save look like it worked,
//! and not a missing module, which would mean gating every one of the ~20 call
//! sites and the functions above them.
//!
//! The two that do work are the two that never touched the disk: `resolve_path`
//! has no `~` to expand here, and `backup_path` is string manipulation.
//!
//! One narrow exception, and it is spelled as one: a path under
//! [`BROWSER_STORE_DIR`] is not a file, it is a `localStorage` entry. That is
//! where the app's own preferences go (`zedis.toml`: language, theme, font
//! size, layout) — they are the visitor's and the device's, so they belong to
//! the browser, need no account to tell people apart, and cost the bridge no
//! state. Only [`browser_store_path`] makes such a path, so nothing becomes
//! writable by accident: every real path still answers `Unsupported`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

type Result<T, E = std::io::Error> = std::result::Result<T, E>;

/// What every path-producing call answers with.
fn no_file_system(what: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("{what} does not exist in a browser"),
    )
}

/// What every write answers with.
fn cannot_write(path: &Path) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!("a browser cannot write {}", path.display()),
    )
}

/// Accepted and ignored: there is no config directory to redirect.
pub fn override_config_dir(_path: PathBuf) {}

pub fn config_dir_override() -> Option<PathBuf> {
    None
}

pub fn copy_dir_recursive(src: &PathBuf, _dst: &Path) -> Result<()> {
    Err(cannot_write(src))
}

/// There is no sandboxed Mac App Store build of a web page.
pub fn is_app_store_build() -> bool {
    false
}

pub fn get_home_dir() -> Option<PathBuf> {
    None
}

pub fn get_download_dir() -> Option<PathBuf> {
    None
}

pub fn get_or_create_config_dir() -> Result<PathBuf> {
    Err(no_file_system("a config directory"))
}

/// Identity. On the desktop this expands `~` and absolutizes against the
/// working directory; a page has neither, and the string is all the caller
/// ever had.
pub fn resolve_path(path: &str) -> String {
    path.to_string()
}

/// Pure string work, and the one function here that behaves identically.
pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.push_str(".bak");
    path.with_file_name(name)
}

/// The directory that is not one: a path under it names a `localStorage`
/// entry (see the module note). No real path can start with it by accident.
pub const BROWSER_STORE_DIR: &str = "/localStorage";

/// The path for the browser-stored file called `name` — the only way to get a
/// path this module will write.
pub fn browser_store_path(name: &str) -> PathBuf {
    Path::new(BROWSER_STORE_DIR).join(name)
}

/// The `localStorage` key behind `path`, or `None` for a path that is a real
/// one and so has nowhere to go here. Namespaced, because the origin may be
/// shared with whatever else the same host serves.
fn browser_store_key(path: &Path) -> Option<String> {
    let name = path.strip_prefix(BROWSER_STORE_DIR).ok()?.to_str()?;
    (!name.is_empty()).then(|| format!("zedis:{name}"))
}

fn storage_error(what: &str) -> std::io::Error {
    std::io::Error::other(format!("localStorage: {what}"))
}

/// `window.localStorage`, which a browser may withhold (a sandboxed frame, a
/// privacy mode) — an error then, like any other store that is not there.
fn local_storage() -> Result<web_sys::Storage> {
    web_sys::window()
        .ok_or_else(|| storage_error("no window"))?
        .local_storage()
        .map_err(|_| storage_error("access denied"))?
        .ok_or_else(|| storage_error("not available"))
}

pub fn write_file_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let Some(key) = browser_store_key(path) else {
        return Err(cannot_write(path));
    };
    let text = std::str::from_utf8(contents).map_err(|_| storage_error("the value is not UTF-8"))?;
    // `setItem` replaces the value in one step, which is all "atomic" asks
    // for here; it throws when the quota is exhausted.
    local_storage()?
        .set_item(&key, text)
        .map_err(|_| storage_error("the write was refused (quota?)"))
}

/// No rolling `.bak` here: the desktop keeps one because a crash can leave a
/// half-written file, and a `setItem` cannot be half-done.
pub fn write_file_atomic_with_backup(path: &Path, contents: &[u8]) -> Result<()> {
    write_file_atomic(path, contents)
}

/// What [`load_config_with_recovery`] had to do to hand back a usable value.
/// Kept as a type on both targets so the UI that reports a recovery compiles
/// either way; nothing here ever produces one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigRecovery {
    RestoredFromBackup { path: PathBuf, corrupt_path: PathBuf },
    Reset { path: PathBuf, corrupt_path: PathBuf },
}

impl ConfigRecovery {
    pub fn path(&self) -> &Path {
        match self {
            Self::RestoredFromBackup { path, .. } | Self::Reset { path, .. } => path,
        }
    }
    pub fn corrupt_path(&self) -> &Path {
        match self {
            Self::RestoredFromBackup { corrupt_path, .. } | Self::Reset { corrupt_path, .. } => corrupt_path,
        }
    }
}

#[derive(Debug)]
pub struct LoadedConfig<T> {
    pub value: Option<T>,
    pub recovery: Option<ConfigRecovery>,
}

static CONFIG_RECOVERIES: Mutex<Vec<ConfigRecovery>> = Mutex::new(Vec::new());

/// What [`load_config_with_recovery`] had to reset since the last call, for
/// the UI to report once the window is up — the desktop's contract.
pub fn take_config_recoveries() -> Vec<ConfigRecovery> {
    let mut list = CONFIG_RECOVERIES.lock().unwrap_or_else(|e| e.into_inner());
    std::mem::take(&mut *list)
}

/// A browser-stored file is read from `localStorage`; any other path is
/// reported missing, which is what "first run, use defaults" looks like on
/// the desktop too.
///
/// A stored value that no longer parses is moved aside under `<key>.corrupt`
/// and reported as a reset, the way the desktop quarantines a damaged file:
/// parsing it as empty would let the next save write defaults over it with
/// nobody told. There is no `.bak` to restore from (see
/// [`write_file_atomic_with_backup`]).
pub fn load_config_with_recovery<T>(
    path: &Path,
    parse: impl Fn(&str) -> std::result::Result<T, String>,
) -> Result<LoadedConfig<T>> {
    let missing = LoadedConfig {
        value: None,
        recovery: None,
    };
    let Some(key) = browser_store_key(path) else {
        return Ok(missing);
    };
    let storage = local_storage()?;
    let Some(text) = storage
        .get_item(&key)
        .map_err(|_| storage_error("the read was refused"))?
    else {
        return Ok(missing);
    };
    if text.trim().is_empty() {
        return Ok(missing);
    }
    match parse(&text) {
        Ok(value) => Ok(LoadedConfig {
            value: Some(value),
            recovery: None,
        }),
        Err(_) => {
            let corrupt_path = path.with_file_name(format!(
                "{}.corrupt",
                path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
            ));
            // Best effort: if the quota refuses the copy, the reset still happens.
            let _ = storage.set_item(&format!("{key}.corrupt"), &text);
            let _ = storage.remove_item(&key);
            let recovery = ConfigRecovery::Reset {
                path: path.to_path_buf(),
                corrupt_path,
            };
            CONFIG_RECOVERIES
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(recovery.clone());
            Ok(LoadedConfig {
                value: None,
                recovery: Some(recovery),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_browser_store_path_has_a_key() {
        assert_eq!(
            browser_store_key(&browser_store_path("zedis.toml")).as_deref(),
            Some("zedis:zedis.toml")
        );
        assert_eq!(
            browser_store_key(Path::new("/zedis.toml")),
            None,
            "a real path stays unwritable"
        );
        assert_eq!(browser_store_key(Path::new("/etc/localStorage/zedis.toml")), None);
        assert_eq!(
            browser_store_key(Path::new(BROWSER_STORE_DIR)),
            None,
            "the directory itself names nothing"
        );
    }

    #[test]
    fn a_write_is_refused_rather_than_silently_dropped() {
        let path = Path::new("/zedis.toml");
        // The distinction matters: a save that returns `Ok` here would look
        // like it worked and lose the user's servers.
        let err = write_file_atomic(path, b"x").expect_err("must refuse");
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
        assert!(err.to_string().contains("zedis.toml"), "the error names the file");
    }

    #[test]
    fn there_is_no_config_directory() {
        assert!(get_or_create_config_dir().is_err());
        assert_eq!(get_home_dir(), None);
        assert_eq!(get_download_dir(), None);
        assert!(!is_app_store_build());
    }

    #[test]
    fn the_two_pure_helpers_behave_as_they_do_on_the_desktop() {
        assert_eq!(resolve_path("relative/x.json"), "relative/x.json");
        assert_eq!(
            backup_path(Path::new("/cfg/zedis.toml")),
            PathBuf::from("/cfg/zedis.toml.bak")
        );
    }

    #[test]
    fn a_missing_file_reads_as_first_run() {
        let loaded = load_config_with_recovery::<u32>(Path::new("/nothing"), |_| Ok(1)).expect("ok");
        assert!(loaded.value.is_none());
        assert!(loaded.recovery.is_none());
        assert!(take_config_recoveries().is_empty());
    }
}
