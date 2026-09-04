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

//! User keybinding overrides: `<config_dir>/keybindings.toml`.
//!
//! The file maps a shortcut id (see [`hot_key_table`]) to a gpui keystroke,
//! one line per override. It is read once at startup — gpui's keymap is
//! bound before the first window opens and there is no per-binding unbind —
//! so a change applies after a restart, which the Settings row says. The
//! effective map is kept process-wide so both the keymap
//! ([`new_hot_keys`]) and the ⌘/ reference overlay
//! ([`shortcut_reference`]) show the same keys.

use crate::helpers::hot_key_table;
use gpui::Keystroke;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use tracing::warn;
use zedis_core::fs::{get_or_create_config_dir, write_file_atomic};

pub const KEYBINDINGS_FILE: &str = "keybindings.toml";

static OVERRIDES: OnceLock<HashMap<String, String>> = OnceLock::new();

/// The loaded overrides (`id → keystroke`). Empty until
/// [`load_keybinding_overrides`] ran — tests and the offline tools never do.
pub fn keybinding_overrides() -> &'static HashMap<String, String> {
    static EMPTY: OnceLock<HashMap<String, String>> = OnceLock::new();
    OVERRIDES.get().unwrap_or_else(|| EMPTY.get_or_init(HashMap::new))
}

pub fn keybindings_file_path() -> std::io::Result<PathBuf> {
    Ok(get_or_create_config_dir()?.join(KEYBINDINGS_FILE))
}

/// Read and validate the overrides file, then publish the result. Returns
/// how many bindings are overridden. A missing file is the common case and
/// is silent; an unreadable or malformed one is logged and ignored, never
/// fatal — the defaults still bind.
pub fn load_keybinding_overrides() -> usize {
    let overrides = match keybindings_file_path() {
        Ok(path) => match std::fs::read_to_string(&path) {
            Ok(text) => {
                let (overrides, warnings) = parse_keybinding_overrides(&text);
                for warning in warnings {
                    warn!(file = %path.display(), "{warning}");
                }
                overrides
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => {
                warn!(file = %path.display(), error = %e, "keybindings file unreadable, using defaults");
                HashMap::new()
            }
        },
        Err(e) => {
            warn!(error = %e, "config dir unavailable, using default keybindings");
            HashMap::new()
        }
    };
    let count = overrides.len();
    // A second call (there is none) keeps the first map; the keymap was
    // bound from it anyway.
    let _ = OVERRIDES.set(overrides);
    count
}

/// Parse the file body: top-level `id = "keystroke"` pairs. Unknown ids and
/// invalid keystrokes are dropped with a warning each, so one typo never
/// discards the rest of the file.
pub fn parse_keybinding_overrides(text: &str) -> (HashMap<String, String>, Vec<String>) {
    let mut overrides = HashMap::new();
    let mut warnings = Vec::new();
    let table: toml::Table = match text.parse() {
        Ok(table) => table,
        Err(e) => {
            warnings.push(format!("keybindings file ignored: {e}"));
            return (overrides, warnings);
        }
    };
    let known = hot_key_table();
    for (id, value) in table {
        let Some(hot_key) = known.iter().find(|hot_key| hot_key.id == id) else {
            warnings.push(format!("unknown shortcut id `{id}` ignored"));
            continue;
        };
        let Some(keystroke) = value.as_str() else {
            warnings.push(format!("shortcut `{id}` must be a string like \"secondary-k\""));
            continue;
        };
        let keystroke = keystroke.trim();
        if !keystroke_is_valid(keystroke) {
            warnings.push(format!("shortcut `{id}`: invalid keystroke `{keystroke}`"));
            continue;
        }
        if keystroke == hot_key.default {
            continue;
        }
        overrides.insert(id, keystroke.to_string());
    }
    (overrides, warnings)
}

/// A keystroke is one or more space-separated gpui keystrokes
/// (`secondary-k`, `ctrl-alt-x`, `ctrl-k ctrl-s`).
fn keystroke_is_valid(keystroke: &str) -> bool {
    !keystroke.is_empty() && keystroke.split_whitespace().all(|part| Keystroke::parse(part).is_ok())
}

/// The file body written on first open: every id with its default, commented
/// out, so a user edits in place instead of guessing names.
pub fn keybindings_template() -> String {
    let mut out = String::from(
        "# Zedis keyboard shortcuts.\n\
         #\n\
         # One line per shortcut: id = \"keystroke\". Uncomment a line and change\n\
         # the keystroke; delete it (or comment it out) to restore the default.\n\
         # Restart Zedis to apply.\n\
         #\n\
         # Keystroke syntax: modifiers joined with `-`, then the key:\n\
         #   secondary  = Cmd on macOS, Ctrl on Linux / Windows\n\
         #   cmd, ctrl, alt, shift, fn\n\
         #   keys: a-z, 0-9, f1-f12, enter, escape, tab, space, backspace,\n\
         #         delete, up, down, left, right, home, end, pageup, pagedown,\n\
         #         and punctuation such as `/`, `=`, `-`, `[`\n\
         # A chord is two keystrokes separated by a space: \"ctrl-k ctrl-s\".\n\n",
    );
    for hot_key in hot_key_table() {
        out.push_str(&format!("# {} = \"{}\"\n", hot_key.id, hot_key.default));
    }
    out
}

/// Create the overrides file from the template when it does not exist yet
/// and return its path — the Settings "Edit shortcuts" button.
pub fn ensure_keybindings_file() -> std::io::Result<PathBuf> {
    let path = keybindings_file_path()?;
    if !path.exists() {
        write_file_atomic(&path, keybindings_template().as_bytes())?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_lists_every_id_and_parses_to_no_overrides() {
        let template = keybindings_template();
        for hot_key in hot_key_table() {
            assert!(template.contains(&format!("# {} = ", hot_key.id)), "{}", hot_key.id);
        }
        let (overrides, warnings) = parse_keybinding_overrides(&template);
        assert!(overrides.is_empty());
        assert!(warnings.is_empty());
        // Uncommenting every shortcut line binds the defaults again: still
        // no override.
        let uncommented: String = hot_key_table()
            .iter()
            .map(|hot_key| format!("{} = \"{}\"\n", hot_key.id, hot_key.default))
            .collect();
        let (overrides, warnings) = parse_keybinding_overrides(&uncommented);
        assert!(overrides.is_empty(), "{overrides:?}");
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn bad_lines_are_skipped_one_at_a_time() {
        let text = "command_palette = \"ctrl-shift-p\"\nno_such_id = \"cmd-x\"\nsave = \"nope-x\"\nquit = 3\nterminal = \"ctrl-k ctrl-t\"\n";
        let (overrides, warnings) = parse_keybinding_overrides(text);
        assert_eq!(
            overrides.get("command_palette").map(String::as_str),
            Some("ctrl-shift-p")
        );
        assert_eq!(overrides.get("terminal").map(String::as_str), Some("ctrl-k ctrl-t"));
        assert_eq!(overrides.len(), 2, "{overrides:?}");
        assert_eq!(warnings.len(), 3, "{warnings:?}");
    }

    #[test]
    fn a_malformed_file_is_ignored_whole() {
        let (overrides, warnings) = parse_keybinding_overrides("[[[");
        assert!(overrides.is_empty());
        assert_eq!(warnings.len(), 1);
    }
}
