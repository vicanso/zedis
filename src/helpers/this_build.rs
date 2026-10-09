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
//! "Quit", a Settings item that opened no window and a dozen Export buttons
//! that asked the platform for a save dialog it does not have, each of them
//! silent when pressed.
//!
//! Each question here is one platform ability, asked by every entry point
//! that needs it, so a feature that later gains a browser form (a download
//! in place of a save dialog) changes one answer and not a dozen call sites.
//! Panels are asked through `ServerView::in_this_build`. A whole desktop
//! chore with a single entry point (the updater, the logs folder, the
//! diagnostics bundle) is simply gated where it is drawn.
//!
//! The desktop answers `true` to all of them, and a test holds it to that:
//! nothing here may take a feature away from the app this is built from.

/// Save and open panels: every "export to a file" and "import from a file".
///
/// The browser backend answers both prompts with an error (`prompt_for_paths
/// is not supported on the web`), and the callers treat an error like a
/// cancelled dialog — so the button did nothing at all.
#[cfg(not(target_family = "wasm"))]
pub const fn has_file_dialogs() -> bool {
    true
}
#[cfg(target_family = "wasm")]
pub const fn has_file_dialogs() -> bool {
    false
}

/// A second window: Settings and About.
///
/// A page is one canvas, and the browser backend refuses a second window
/// (`AlreadyOpen`); `open_secondary_window` then has nothing to show.
#[cfg(not(target_family = "wasm"))]
pub const fn has_secondary_windows() -> bool {
    true
}
#[cfg(target_family = "wasm")]
pub const fn has_secondary_windows() -> bool {
    false
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

/// Pub/Sub — the editor's channel mode.
///
/// A subscription is a connection the server pushes on, and the bridge
/// forwards requests and their replies (ADR 9); the mode's panel is not in
/// the browser build.
#[cfg(not(target_family = "wasm"))]
pub const fn has_pubsub() -> bool {
    true
}
#[cfg(target_family = "wasm")]
pub const fn has_pubsub() -> bool {
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
        assert!(has_secondary_windows());
        assert!(reads_clipboard_on_demand());
        assert!(has_pubsub());
    }
}
