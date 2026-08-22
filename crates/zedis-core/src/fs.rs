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

//! File system helper utilities.
//!
//! This module provides utility functions for file system operations including:
//! - Directory copying operations
//! - App Store build detection (for macOS sandboxing)
//! - Configuration directory management with migration support
//! - Crash-safe config file writes with a rolling backup, and loading that
//!   recovers from a damaged file instead of silently resetting it

use crate::env::is_development;
use directories::{ProjectDirs, UserDirs};
use home::home_dir;
use path_absolutize::Absolutize;
use std::{
    env, fs,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

/// Process-wide config-dir override, set at most once. Unit tests set it via
/// [`override_config_dir`]; external runs (CI smoke) set `ZEDIS_CONFIG_DIR`.
static CONFIG_DIR_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

/// Redirect the config directory for this process (first call wins). Test-only
/// by convention: keeps state persistence in tests away from the real
/// `zedis.toml`. Not `#[cfg(test)]` — the app crate's tests call it through
/// this crate, and a test-gated item would be invisible to them.
pub fn override_config_dir(path: PathBuf) {
    let _ = CONFIG_DIR_OVERRIDE.set(path);
}

/// The active config-dir override, if any — set in-process via
/// [`override_config_dir`] or externally via `ZEDIS_CONFIG_DIR`. `Some` means
/// the config dir is isolated (unit tests / CI smoke runs), which callers can
/// use to keep machine-local side effects (e.g. the encryption master key)
/// beside that isolated config rather than in a shared store like the OS
/// keychain. Prefer this over reading `ZEDIS_CONFIG_DIR` directly so both
/// override mechanisms stay covered by one source of truth.
pub fn config_dir_override() -> Option<PathBuf> {
    if let Some(dir) = CONFIG_DIR_OVERRIDE.get() {
        return Some(dir.clone());
    }
    match env::var("ZEDIS_CONFIG_DIR") {
        Ok(dir) if !dir.trim().is_empty() => Some(PathBuf::from(dir)),
        _ => None,
    }
}

type Result<T, E = std::io::Error> = std::result::Result<T, E>;
/// Recursively copies files from source directory to destination directory.
///
/// Note: This function only copies files, not subdirectories. Subdirectories
/// are skipped during the copy operation.
///
/// # Arguments
/// * `src` - Source directory path
/// * `dst` - Destination directory path
///
/// # Returns
/// `Ok(())` on success, or an error if any file operation fails
///
/// # Errors
/// Returns an error if:
/// - Source directory cannot be read
/// - File type cannot be determined
/// - File copy operation fails
pub fn copy_dir_recursive(src: &PathBuf, dst: &Path) -> Result<()> {
    // Iterate through all entries in the source directory
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;

        // Skip subdirectories, only copy files
        if file_type.is_dir() {
            continue;
        }

        // Build source and destination paths
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        // Copy the file
        fs::copy(&src_path, &dst_path)?;
    }
    Ok(())
}

/// Detects if the application is running as a Mac App Store build.
///
/// This is determined by checking for the presence of the `_MASReceipt/receipt`
/// file in the app bundle, which is automatically added by Apple for App Store
/// builds. This is useful for handling different sandboxing requirements.
///
/// # Returns
/// `true` if running as an App Store build, `false` otherwise
///
/// # Implementation Notes
/// The function navigates from the executable path:
/// - From: `/path/to/App.app/Contents/MacOS/executable`
/// - To: `/path/to/App.app/Contents/_MASReceipt/receipt`
pub fn is_app_store_build() -> bool {
    let Ok(exe_path) = env::current_exe() else {
        return false;
    };

    let mut receipt_path = exe_path;

    // Navigate up two levels: from MacOS/executable to Contents/
    if !receipt_path.pop() || !receipt_path.pop() {
        return false;
    }

    // Check for App Store receipt file
    receipt_path.push("_MASReceipt");
    receipt_path.push("receipt");

    receipt_path.exists()
}

pub fn get_home_dir() -> Option<PathBuf> {
    if is_app_store_build() {
        return None;
    }
    let dirs = UserDirs::new()?;
    Some(dirs.home_dir().to_path_buf())
}

/// The user's Downloads directory via `UserDirs` — cross-platform and
/// honoring localized folder names / XDG user dirs (rather than blindly
/// joining `~/Downloads`). `None` on App Store builds or when the platform
/// reports no configured Downloads directory.
pub fn get_download_dir() -> Option<PathBuf> {
    if is_app_store_build() {
        return None;
    }
    let dirs = UserDirs::new()?;
    dirs.download_dir().map(Path::to_path_buf)
}

/// Gets or creates the application's configuration directory.
///
/// This function handles configuration directory management with backward compatibility:
/// 1. Determines the platform-specific config directory (using XDG on Linux, ~/Library on macOS, etc.)
/// 2. Creates the directory if it doesn't exist
/// 3. Migrates old configuration from `~/.zedis` to the new location if found
///
/// # Returns
/// The path to the configuration directory
///
/// # Errors
/// Returns an error if:
/// - Project directories cannot be determined for the platform
/// - Directory creation fails
///
/// # Platform-specific Locations
/// - **Linux**: `~/.config/zedis/` or `$XDG_CONFIG_HOME/zedis/`
/// - **macOS**: `~/Library/Application Support/com.bigtree.zedis/`
/// - **Windows**: `C:\Users\<User>\AppData\Roaming\bigtree\zedis\config\`
///
/// # Migration
/// If an old `~/.zedis` directory exists, its contents are copied to the new
/// location and the old directory is removed.
pub fn get_or_create_config_dir() -> Result<PathBuf> {
    // Isolation override for CI smoke runs (`ZEDIS_CONFIG_DIR=…`) and unit
    // tests (`override_config_dir`) — anything exercising state persistence
    // must never touch the real user profile. Skips the `~/.zedis` migration.
    if let Some(dir) = config_dir_override() {
        if !dir.exists() {
            fs::create_dir_all(&dir)?;
        }
        return Ok(dir);
    }
    // Get platform-specific configuration directory
    let Some(project_dirs) = ProjectDirs::from("com", "bigtree", "zedis") else {
        return Err(std::io::Error::other("project directories not found".to_string()));
    };

    let config_dir = project_dirs.config_dir();

    // Create config directory if it doesn't exist
    if !config_dir.exists() {
        fs::create_dir_all(config_dir)?;
    }

    // Handle migration from old ~/.zedis location
    let Some(home) = home_dir() else {
        // If home directory cannot be determined, just return the config dir
        return Ok(config_dir.to_path_buf());
    };

    let old_config_path = home.join(".zedis");
    if old_config_path.exists() {
        // Attempt to copy files from old location (ignore errors)
        let _ = copy_dir_recursive(&old_config_path, config_dir);

        // Clean up old directory (ignore errors)
        let _ = fs::remove_dir_all(&old_config_path);
    }

    if is_development() {
        return dev_config_dir(config_dir);
    }

    Ok(config_dir.to_path_buf())
}

/// `<config_dir>/dev` — where a `RUST_ENV=dev` run keeps *everything*, under the
/// same file names as production.
///
/// Isolation used to be per-file (`zedis-dev.toml`, `zedis-dev.redb`), which
/// only covered two of them: the server list, the session options and the SSH
/// keys were shared with the installed app. So deleting a server while
/// developing silently orphaned the production proto configs that keyed off its
/// id — the data was intact, but pointed at a server that no longer existed.
/// One directory, one rule: dev writes never leave `dev/`.
fn dev_config_dir(config_dir: &Path) -> Result<PathBuf> {
    let dev_dir = config_dir.join("dev");
    if dev_dir.exists() {
        return Ok(dev_dir);
    }
    fs::create_dir_all(&dev_dir)?;

    // First run on the new layout — carry the old dev session over instead of
    // resetting it. Best-effort throughout: a failure just leaves that file out.
    // Legacy per-file variants move in under their production names.
    for (legacy, name) in [("zedis-dev.toml", "zedis.toml"), ("zedis-dev.redb", "zedis.redb")] {
        let from = config_dir.join(legacy);
        if from.exists() {
            let _ = fs::rename(&from, dev_dir.join(name));
        }
    }
    // These were *shared* with production, so dev would otherwise start with no
    // servers at all — and the proto/script configs it already holds key off
    // those server ids. Seeded by copy: production's copies are never touched
    // again after this.
    for name in ["redis-servers.toml", "redis-sessions.toml", "ssh_agent_keys"] {
        let from = config_dir.join(name);
        if from.exists() {
            let _ = fs::copy(&from, dev_dir.join(name));
        }
    }

    Ok(dev_dir)
}

pub fn resolve_path(path: &str) -> String {
    if path.is_empty() {
        return "".to_string();
    }
    let mut p = path.to_string();
    if p.starts_with('~')
        && let Some(home) = get_home_dir()
    {
        p = home.to_string_lossy().to_string() + &p[1..];
    }
    if let Ok(p) = Path::new(&p).absolutize() {
        p.to_string_lossy().to_string()
    } else {
        p
    }
}

// ---------------------------------------------------------------------------
// Crash-safe config files
// ---------------------------------------------------------------------------

/// Serialises every config write in this process. The app-state saver runs on
/// background tasks (debounced prefs, route persistence) that can overlap, and
/// two writers racing on the same `.tmp` sibling would leave one rename
/// failing. Config files are a few KB, so one lock costs nothing.
static CONFIG_WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Recoveries performed by [`load_config_with_recovery`] that the UI has not
/// reported yet. Startup loads run before any window exists, so the outcome is
/// parked here and drained by [`take_config_recoveries`] once the UI is up.
static CONFIG_RECOVERIES: Mutex<Vec<ConfigRecovery>> = Mutex::new(Vec::new());

/// Path of the rolling backup kept beside a config file
/// (`zedis.toml` → `zedis.toml.bak`).
pub fn backup_path(path: &Path) -> PathBuf {
    sibling_with_suffix(path, ".bak")
}

fn sibling_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(suffix);
    path.with_file_name(name)
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Writes `contents` to `path` so that a crash or power loss can never leave a
/// half-written file behind: the bytes go to a `.tmp` sibling first, are
/// flushed to disk, and the sibling is then renamed over `path` — an atomic
/// replace on every supported platform.
pub fn write_file_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let _guard = CONFIG_WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    write_file_atomic_locked(path, contents)
}

/// [`write_file_atomic`] plus a rolling backup: the current on-disk content is
/// first copied (atomically as well) to [`backup_path`], so after the write
/// the `.bak` holds the last known-good version. An empty or missing `path`
/// is not backed up — that would only clobber a useful backup with nothing.
pub fn write_file_atomic_with_backup(path: &Path, contents: &[u8]) -> Result<()> {
    let _guard = CONFIG_WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    match fs::read(path) {
        Ok(previous) if !previous.is_empty() => {
            write_file_atomic_locked(&backup_path(path), &previous)?;
        }
        Ok(_) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    write_file_atomic_locked(path, contents)
}

/// The write itself; callers hold [`CONFIG_WRITE_LOCK`].
fn write_file_atomic_locked(path: &Path, contents: &[u8]) -> Result<()> {
    let tmp = sibling_with_suffix(path, ".tmp");
    let result = write_tmp_then_rename(&tmp, path, contents);
    if result.is_err() {
        // Best effort: don't leave a stray `.tmp` behind a failed write.
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn write_tmp_then_rename(tmp: &Path, path: &Path, contents: &[u8]) -> Result<()> {
    {
        // Scoped so the handle is closed before the rename — Windows refuses
        // to move a file that is still open.
        let mut file = fs::File::create(tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
    }
    fs::rename(tmp, path)
}

/// What [`load_config_with_recovery`] had to do to hand back a usable value.
/// Both variants keep the damaged file beside the original (as
/// `<name>.corrupt-<unix-secs>`) so nothing is thrown away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigRecovery {
    /// `path` failed to parse; its `.bak` parsed and was restored over it.
    RestoredFromBackup { path: PathBuf, corrupt_path: PathBuf },
    /// `path` failed to parse and no usable backup existed; the caller starts
    /// from defaults.
    Reset { path: PathBuf, corrupt_path: PathBuf },
}

impl ConfigRecovery {
    /// The config file that was damaged.
    pub fn path(&self) -> &Path {
        match self {
            Self::RestoredFromBackup { path, .. } | Self::Reset { path, .. } => path,
        }
    }
    /// Where the damaged copy was moved to.
    pub fn corrupt_path(&self) -> &Path {
        match self {
            Self::RestoredFromBackup { corrupt_path, .. } | Self::Reset { corrupt_path, .. } => corrupt_path,
        }
    }
}

/// Result of [`load_config_with_recovery`].
#[derive(Debug)]
pub struct LoadedConfig<T> {
    /// `None` when the file is missing or empty (first run) — use defaults.
    pub value: Option<T>,
    /// Set when the file was damaged; also recorded for
    /// [`take_config_recoveries`] so the UI can report it.
    pub recovery: Option<ConfigRecovery>,
}

/// Drains the recoveries recorded so far, in the order they happened.
pub fn take_config_recoveries() -> Vec<ConfigRecovery> {
    let mut list = CONFIG_RECOVERIES.lock().unwrap_or_else(|e| e.into_inner());
    std::mem::take(&mut *list)
}

/// Reads and parses a config file, falling back to its `.bak` when the file
/// itself is damaged. A parse failure is never silently turned into defaults
/// (which the next save would then write over the user's data): the damaged
/// file is moved aside as `<name>.corrupt-<unix-secs>`, the backup is
/// restored over `path` when it parses, and the outcome is returned and
/// recorded for the UI. I/O errors other than "not found" propagate.
pub fn load_config_with_recovery<T>(
    path: &Path,
    parse: impl Fn(&str) -> std::result::Result<T, String>,
) -> Result<LoadedConfig<T>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Ok(LoadedConfig {
                value: None,
                recovery: None,
            });
        }
        Err(e) => return Err(e),
    };
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(LoadedConfig {
            value: None,
            recovery: None,
        });
    }
    // Invalid UTF-8 is damage too, not an I/O failure.
    let parsed = String::from_utf8(bytes)
        .map_err(|e| e.to_string())
        .and_then(|text| parse(&text));
    if let Ok(value) = parsed {
        return Ok(LoadedConfig {
            value: Some(value),
            recovery: None,
        });
    }

    let _guard = CONFIG_WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let corrupt_path = sibling_with_suffix(path, &format!(".corrupt-{}", unix_secs()));
    fs::rename(path, &corrupt_path)?;

    let backup = fs::read(backup_path(path)).ok().filter(|b| !b.is_empty());
    let restored = backup.and_then(|bytes| {
        let value = String::from_utf8(bytes.clone())
            .ok()
            .and_then(|text| parse(&text).ok())?;
        Some((bytes, value))
    });
    let (value, recovery) = match restored {
        Some((bytes, value)) => {
            write_file_atomic_locked(path, &bytes)?;
            (
                Some(value),
                ConfigRecovery::RestoredFromBackup {
                    path: path.to_path_buf(),
                    corrupt_path,
                },
            )
        }
        None => (
            None,
            ConfigRecovery::Reset {
                path: path.to_path_buf(),
                corrupt_path,
            },
        ),
    };
    CONFIG_RECOVERIES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(recovery.clone());
    Ok(LoadedConfig {
        value,
        recovery: Some(recovery),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh scratch directory per test (unique per process + test name),
    /// removed on drop so a failing assertion doesn't leak files.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = env::temp_dir().join(format!("zedis-core-fs-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("create scratch dir");
            Self(dir)
        }
        fn file(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
        fn entries(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(&self.0)
                .expect("read scratch dir")
                .map(|e| e.expect("dir entry").file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn parse_num(text: &str) -> std::result::Result<u32, String> {
        text.trim().parse::<u32>().map_err(|e| e.to_string())
    }

    #[test]
    fn atomic_write_replaces_content_and_leaves_no_tmp() {
        let scratch = Scratch::new("atomic");
        let path = scratch.file("a.toml");
        write_file_atomic(&path, b"one").expect("first write");
        write_file_atomic(&path, b"two").expect("second write");
        assert_eq!(fs::read_to_string(&path).expect("read"), "two");
        assert_eq!(scratch.entries(), vec!["a.toml"]);
    }

    #[test]
    fn backup_holds_previous_version_and_skips_empty_files() {
        let scratch = Scratch::new("backup");
        let path = scratch.file("a.toml");
        // First write over nothing: no backup should appear.
        write_file_atomic_with_backup(&path, b"v1").expect("write v1");
        assert_eq!(scratch.entries(), vec!["a.toml"]);
        // An empty file (what `get_or_create_*` leaves behind) is not
        // worth backing up either.
        fs::write(&path, b"").expect("truncate");
        write_file_atomic_with_backup(&path, b"v2").expect("write v2");
        assert_eq!(scratch.entries(), vec!["a.toml"]);
        // From here on the backup trails by exactly one version.
        write_file_atomic_with_backup(&path, b"v3").expect("write v3");
        assert_eq!(fs::read_to_string(&path).expect("read"), "v3");
        assert_eq!(fs::read_to_string(backup_path(&path)).expect("read bak"), "v2");
        assert_eq!(scratch.entries(), vec!["a.toml", "a.toml.bak"]);
    }

    #[test]
    fn load_returns_none_for_missing_or_blank_file() {
        let scratch = Scratch::new("blank");
        let path = scratch.file("a.toml");
        let loaded = load_config_with_recovery(&path, parse_num).expect("missing");
        assert!(loaded.value.is_none() && loaded.recovery.is_none());
        fs::write(&path, b"  \n").expect("write blank");
        let loaded = load_config_with_recovery(&path, parse_num).expect("blank");
        assert!(loaded.value.is_none() && loaded.recovery.is_none());
        assert_eq!(scratch.entries(), vec!["a.toml"]);
    }

    #[test]
    fn load_parses_valid_file_without_touching_disk() {
        let scratch = Scratch::new("valid");
        let path = scratch.file("a.toml");
        fs::write(&path, b"42").expect("write");
        let loaded = load_config_with_recovery(&path, parse_num).expect("load");
        assert_eq!(loaded.value, Some(42));
        assert!(loaded.recovery.is_none());
        assert_eq!(scratch.entries(), vec!["a.toml"]);
    }

    /// The only test that produces recoveries: it also checks the
    /// process-wide queue, which parallel tests could otherwise drain.
    #[test]
    fn damaged_file_is_quarantined_then_restored_or_reset() {
        let scratch = Scratch::new("recover");
        let path = scratch.file("a.toml");

        // Damaged file + good backup → restored, file repaired on disk.
        fs::write(&path, b"not a number").expect("write corrupt");
        fs::write(backup_path(&path), b"7").expect("write bak");
        let loaded = load_config_with_recovery(&path, parse_num).expect("load");
        assert_eq!(loaded.value, Some(7));
        let Some(ConfigRecovery::RestoredFromBackup { corrupt_path, .. }) = loaded.recovery.clone() else {
            panic!("expected RestoredFromBackup, got {:?}", loaded.recovery);
        };
        assert_eq!(fs::read_to_string(&path).expect("repaired"), "7");
        assert_eq!(fs::read_to_string(&corrupt_path).expect("quarantined"), "not a number");
        assert!(
            corrupt_path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("a.toml.corrupt-"))
        );
        let restored = loaded.recovery.clone().expect("recovery");

        // Damaged file + damaged backup → reset, both copies kept.
        fs::write(&path, [0xff, 0xfe]).expect("write invalid utf8");
        fs::write(backup_path(&path), b"also bad").expect("write bad bak");
        let loaded = load_config_with_recovery(&path, parse_num).expect("load");
        assert!(loaded.value.is_none());
        assert!(matches!(loaded.recovery, Some(ConfigRecovery::Reset { .. })));
        assert!(!path.exists(), "a reset must not leave a damaged file in place");
        let reset = loaded.recovery.clone().expect("recovery");

        let recorded = take_config_recoveries();
        assert!(recorded.contains(&restored) && recorded.contains(&reset));
        assert!(!take_config_recoveries().contains(&reset), "take drains the queue");
    }
}
