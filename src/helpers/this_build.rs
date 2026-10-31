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

//! What this build can do, for the buttons that would otherwise promise it.
//!
//! The browser build is the desktop app with parts taken out (ADR 9), and an
//! entry point goes with its feature: a button, a menu item or a palette row
//! is not drawn where pressing it can do nothing. That rule was kept for the
//! panels and broken everywhere smaller — a page had "Check for Updates",
//! "Quit" and a dozen Export buttons that asked the platform for a save
//! dialog it does not have, each of them silent when pressed.
//!
//! Each question here is one platform ability, asked by every entry point
//! that needs it, so a feature that later gains a browser form (a download
//! in place of a save dialog) changes one answer and not a dozen call sites.
//! Panels are asked through `ServerView::in_this_build`. A whole desktop
//! chore with a single entry point (the updater, the logs folder, the
//! diagnostics bundle, the About window) is simply gated where it is drawn.
//! A second window is the one thing a page cannot have at all — the browser
//! backend answers `open_window` with `AlreadyOpen` — so what the desktop
//! shows in one is a dialog there (`views::open_settings`) or is left out.
//!
//! The desktop answers `true` to all of them, and a test holds it to that:
//! nothing here may take a feature away from the app this is built from.

/// Save and open panels, which hand back a *path*.
///
/// The browser backend answers both prompts with an error (`prompt_for_paths
/// is not supported on the web`), and the callers treat an error like a
/// cancelled dialog — so the button did nothing at all. A button that only
/// needs the file's bytes asks [`saves_files`] / [`opens_files`] instead,
/// which a page answers with a download and an upload; this one is left for
/// what needs the path itself (the migration window, a path typed or
/// dropped into the import box).
#[cfg(not(target_family = "wasm"))]
pub const fn has_file_dialogs() -> bool {
    true
}
#[cfg(target_family = "wasm")]
pub const fn has_file_dialogs() -> bool {
    false
}

/// Putting a file in the person's hands: every "Export" and "Save". A save
/// panel on the desktop; in a page, the browser's download, where
/// `zedis-web` installed one (`views::export_to_file` takes either).
#[cfg(not(target_family = "wasm"))]
pub const fn saves_files() -> bool {
    true
}
#[cfg(target_family = "wasm")]
pub fn saves_files() -> bool {
    super::web_download().is_some()
}

/// Taking a file the person picks: an open panel on the desktop; in a page,
/// the browser's file picker, where `zedis-web` installed one
/// (`views::open_file` takes either). What only a *path* can do — a dump
/// streamed from disk, a file dropped on a dialog — still asks
/// [`has_file_dialogs`].
#[cfg(not(target_family = "wasm"))]
pub const fn opens_files() -> bool {
    true
}
#[cfg(target_family = "wasm")]
pub fn opens_files() -> bool {
    super::web_upload().is_some()
}

/// Reading the clipboard when a button asks for it ("Import from clipboard").
///
/// A page may read the clipboard only asynchronously and with the user's
/// permission, so the synchronous read those buttons make answers `None`
/// there and the button reports an empty clipboard. Pasting into a field is
/// the input's own doing and works in both.
#[cfg(not(target_family = "wasm"))]
pub const fn reads_clipboard_on_demand() -> bool {
    true
}
#[cfg(target_family = "wasm")]
pub const fn reads_clipboard_on_demand() -> bool {
    false
}

/// The largest value one write may carry, where there is a limit at all.
///
/// The desktop sends a value to Redis as it is. A page sends it to the
/// bridge, one request at a time, and a request holds at most
/// `BRIDGE_MAX_VALUE_BYTES` of value — so a larger one opens read-only there
/// and says why, instead of editable and then refused at Save.
#[cfg(not(target_family = "wasm"))]
pub const fn value_write_limit() -> Option<usize> {
    None
}
#[cfg(target_family = "wasm")]
pub const fn value_write_limit() -> Option<usize> {
    Some(crate::connection::BRIDGE_MAX_VALUE_BYTES)
}

/// Stopping a runaway script (`SCRIPT KILL` / `FUNCTION KILL`), which has to
/// dial around the pool a `BUSY` server has stuck: directly on the desktop,
/// and through the bridge (`POST /v1/servers/{id}/kill-script`) in a page.
#[cfg(not(target_family = "wasm"))]
pub const fn kills_runaway_scripts() -> bool {
    true
}
#[cfg(target_family = "wasm")]
pub fn kills_runaway_scripts() -> bool {
    crate::connection::signed_in_to_bridge()
}

/// Asking the configured AI endpoint for advice. That is a request this
/// process makes itself, and the browser build carries no HTTP client of
/// its own (ADR 9).
#[cfg(not(target_family = "wasm"))]
pub const fn has_ai_assistant() -> bool {
    true
}
#[cfg(target_family = "wasm")]
pub const fn has_ai_assistant() -> bool {
    false
}

/// Debugging a Lua script step by step (`SCRIPT DEBUG`). The debugger holds
/// one connection for the whole session and reads two replies to one
/// command at its end; a page reaches Redis one request at a time through
/// the bridge, which has no such connection to hold.
#[cfg(not(target_family = "wasm"))]
pub const fn debugs_lua_scripts() -> bool {
    true
}
#[cfg(target_family = "wasm")]
pub const fn debugs_lua_scripts() -> bool {
    false
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;

    /// The desktop is the reference implementation: every question here is
    /// about what the browser lacks, and none of them may hide anything in
    /// the app.
    #[test]
    fn the_desktop_has_all_of_it() {
        assert!(has_file_dialogs());
        assert!(saves_files());
        assert!(opens_files());
        assert!(reads_clipboard_on_demand());
        assert_eq!(value_write_limit(), None, "the desktop writes a value of any size");
        assert!(kills_runaway_scripts());
        assert!(has_ai_assistant());
        assert!(debugs_lua_scripts());
    }
}
