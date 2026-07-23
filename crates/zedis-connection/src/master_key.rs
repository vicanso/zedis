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

//! Machine-local master key for [`crate::string::encrypt`] / `decrypt`.
//!
//! The old build baked a single 256-bit key into the binary, so every install
//! shared it — and, this being an open-source build, anyone could extract it
//! and decrypt any user's `redis-servers.toml`. Instead each machine now keeps
//! its own random key, resolved once per process:
//!
//! 1. **OS keychain** via `keyring` — macOS Keychain / Windows Credential
//!    Manager. The preferred store. Skipped for `RUST_ENV=dev` runs, whose
//!    unsigned binary would otherwise make macOS re-prompt on every rebuild;
//!    those use the key file (2) in the isolated `…/dev` config dir instead.
//! 2. **`master.key` file** (`0600`) in the config dir — the fallback when no
//!    keychain is reachable (headless Linux, CI, containers). It is also the
//!    only store on Linux: we deliberately don't pull the Secret Service /
//!    zbus stack there, and the session keyutils backend isn't reboot-durable,
//!    so a per-user `0600` file is both leaner and safer against key loss.
//! 3. The **legacy embedded key** — last resort if even the file can't be
//!    written, so encryption never hard-fails.
//!
//! `decrypt` tries the resolved key first, then [`LEGACY_MASTER_KEY`], so data
//! written by older builds still opens; re-saving a server / API key rewrites
//! it under the current key (lazy migration — no bulk rewrite on upgrade).

use std::sync::OnceLock;

/// The key baked into every build before the keychain migration. Retained
/// solely as a decryption fallback for configs those builds wrote.
pub(crate) const LEGACY_MASTER_KEY: [u8; 32] = *b"9dFVxjgeQTPfOXCoDdjpgMOlPhy2HE9E";

/// Resolved once, then cached for the process lifetime — the keychain / file
/// lookup is a syscall we don't want on every `encrypt` / `decrypt`.
static RESOLVED_KEY: OnceLock<[u8; 32]> = OnceLock::new();

/// The machine-local master key.
pub(crate) fn master_key() -> &'static [u8; 32] {
    RESOLVED_KEY.get_or_init(resolve_master_key)
}

/// Tests must never reach the real keychain or config dir — use a fixed key so
/// round-trips are deterministic and hermetic. Distinct from
/// [`LEGACY_MASTER_KEY`] so the legacy fallback path is exercised.
#[cfg(test)]
fn resolve_master_key() -> [u8; 32] {
    *b"zedis-test-master-key-0123456789"
}

#[cfg(not(test))]
fn resolve_master_key() -> [u8; 32] {
    real::resolve()
}

/// Real key resolution (keychain → `0600` file → legacy). Confined to a
/// `cfg(not(test))` module so none of it is dead code in the test build, where
/// [`resolve_master_key`] returns a fixed key instead.
#[cfg(not(test))]
mod real {
    use super::LEGACY_MASTER_KEY;
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
    use tracing::warn;

    /// Base name of the fallback key file inside the config dir.
    const KEY_FILE_NAME: &str = "master.key";

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    const KEYRING_SERVICE: &str = "com.bigtree.zedis";
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    const KEYRING_ACCOUNT: &str = "config-master-key";

    pub(super) fn resolve() -> [u8; 32] {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            // Skip the OS keychain and fall through to the key file when either:
            //  - the config dir is isolated (unit tests via `override_config_dir`,
            //    CI smoke runs via `ZEDIS_CONFIG_DIR`) — `config_dir_override`
            //    covers both; or
            //  - this is a development run (`RUST_ENV=dev`): the dev binary is
            //    unsigned / ad-hoc-signed, so macOS re-prompts for keychain
            //    access on every rebuild. Dev already keeps everything under an
            //    isolated `…/dev` config dir, so the key file lands there too.
            if zedis_core::fs::config_dir_override().is_none() && !zedis_core::env::is_development() {
                match keyring_key() {
                    Ok(key) => return key,
                    Err(e) => warn!(error = %e, "keychain unavailable; falling back to key file"),
                }
            }
        }
        match file_key() {
            Ok(key) => return key,
            Err(e) => warn!(error = %e, "key file unavailable; falling back to embedded legacy key"),
        }
        LEGACY_MASTER_KEY
    }

    /// A fresh 256-bit key from the system RNG.
    fn generate_key() -> [u8; 32] {
        use rand::RngExt;
        rand::rng().random()
    }

    /// Decode a stored base64 key, rejecting anything that isn't exactly 32 bytes.
    fn decode_key(raw: &str) -> Option<[u8; 32]> {
        let bytes = BASE64.decode(raw.trim()).ok()?;
        bytes.as_slice().try_into().ok()
    }

    /// Read the key from the OS keychain, creating (and storing) one on first run.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn keyring_key() -> Result<[u8; 32], keyring::Error> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)?;
        match entry.get_password() {
            Ok(stored) => {
                if let Some(key) = decode_key(&stored) {
                    return Ok(key);
                }
                // Corrupt entry: overwrite with a fresh key. Whatever it once
                // protected is unrecoverable regardless (the key is unreadable),
                // and `decrypt` still tries the legacy key for old data.
                let key = generate_key();
                entry.set_password(&BASE64.encode(key))?;
                Ok(key)
            }
            Err(keyring::Error::NoEntry) => {
                let key = generate_key();
                entry.set_password(&BASE64.encode(key))?;
                Ok(key)
            }
            Err(e) => Err(e),
        }
    }

    /// Read the key from the `0600` fallback file, creating one on first run.
    fn file_key() -> std::io::Result<[u8; 32]> {
        let path = zedis_core::fs::get_or_create_config_dir()?.join(KEY_FILE_NAME);
        if let Ok(stored) = std::fs::read_to_string(&path)
            && let Some(key) = decode_key(&stored)
        {
            return Ok(key);
        }
        let key = generate_key();
        write_key_file(&path, &key)?;
        Ok(key)
    }

    /// Write the base64 key with owner-only permissions where the platform allows.
    fn write_key_file(path: &std::path::Path, key: &[u8; 32]) -> std::io::Result<()> {
        let encoded = BASE64.encode(key);
        #[cfg(unix)]
        {
            use std::io::Write as _;
            use std::os::unix::fs::OpenOptionsExt as _;
            // `mode` applies on create; truncate keeps it `0600` on rewrite too.
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(path)?;
            f.write_all(encoded.as_bytes())
        }
        #[cfg(not(unix))]
        {
            // Windows: no portable `0600`. The config dir already sits under the
            // per-user profile, so its inherited ACL restricts it to the user.
            std::fs::write(path, encoded.as_bytes())
        }
    }
}
