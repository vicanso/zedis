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

use crate::env::is_development;
use directories::{ProjectDirs, UserDirs};
use home::home_dir;
use path_absolutize::Absolutize;
use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::OnceLock,
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

fn config_dir_override() -> Option<PathBuf> {
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
