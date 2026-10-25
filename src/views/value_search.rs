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

//! Search keys by **value content** — something Redis has no native index
//! for, so it is a guarded, *sampling* scan.
//!
//! Redis can only match by key name, not by value, so finding "which key
//! contains this text" means SCANning the keyspace and reading each value.
//! That is `O(keyspace)` of reads, so it runs behind guardrails:
//!
//! - **Mandatory key prefix** — the scan is pinned to a namespace
//!   (`prefix*`), never the whole keyspace by accident.
//! - **Scan + time caps** — stops after the configured key cap or time
//!   budget (Settings → Key Behavior, defaults 10k keys / 10s), whichever
//!   first; cancellable via the Stop button (dropping the task ends the
//!   loop at its next await).
//! - **Per-value size gate** — values larger than [`MAX_VALUE_BYTES`] are
//!   skipped (not pulled down just to grep), and counted.
//! - **Sampling semantics** — the summary states what was scanned / matched /
//!   skipped and why it stopped; results are never claimed exhaustive.
//!
//! - **Continue** — a search the budget or the Stop button ended keeps its
//!   cursors, and goes on from them with a fresh budget while the form still
//!   asks what it asked.
//!
//! Clicking a hit previews its value **inline** in the right-hand pane (the
//! "Open" button there still jumps to the editor). Supports string / hash /
//! list / set / zset — all of them or one — with case-insensitive substring
//! matching.

use crate::components::KeyTypeBadge;
use crate::connection::{
    MatchLocation, SearchableType, ServerDb, ValueMatch, ValueSearchRound, ValueSearchSpec, scan_values_round,
    value_preview,
};
use crate::helpers::{ValueSearchTypeFilter, saves_files};
use crate::helpers::{build_csv, get_mono_font_family, group_thousands};
use crate::states::{
    KeyType, ServerEvent, ServerView, ZedisGlobalStore, ZedisServerState, back_to_editor_tooltip, i18n_common,
    i18n_value_search,
};
use crate::views::export_to_file;
use crate::views::placeholders::Placeholders;
use gpui::{
    App, ClipboardItem, Context, Entity, ScrollHandle, SharedString, Subscription, Task, Window, div, prelude::*, px,
    uniform_list,
};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    menu::DropdownMenu,
    notification::Notification,
    v_flex,
};
use std::{cell::Cell, mem::take, rc::Rc, sync::Arc, time::Duration};
use web_time::Instant;

// The scan cap, time budget and match cap are user-tunable (Settings →
// Key Behavior; `ZedisAppState::value_search_*`, clamped there). The
// constants below stay fixed: they bound what one round asks the server
// for, not how deep a search may go.

/// Values larger than this (bytes) are skipped, not read.
const MAX_VALUE_BYTES: u64 = 1024 * 1024;
/// Containers (hash/list/set/zset) with more than this many elements are
/// skipped, not read whole.
const MAX_CONTAINER_ELEMS: u64 = 10_000;
/// `SCAN COUNT` per master per round — kept modest so cancellation / the time
/// check stay responsive between rounds.
const PAGE_COUNT: u64 = 128;
/// Bytes of a value rendered in the preview pane before truncating.
const PREVIEW_MAX_BYTES: usize = 64 * 1024;

/// The status row's Continue button, which a test looks for.
const CONTINUE_BUTTON_ID: &str = "vs-continue";

/// Example prefixes offered on the empty state (fill the prefix field only).
const EXAMPLE_PREFIXES: &[&str] = &["session:", "user:", "cache:", "job:"];

/// Why a search loop stopped — drives the honest summary line.
#[derive(Clone, Copy, Debug, PartialEq)]
enum StopReason {
    /// Whole (prefix-scoped) keyspace covered.
    Done,
    /// Scanned as many keys as one run may.
    ScanCapped,
    /// Listed as many matches as one run may — that number. The keys
    /// scanned can be far under their own cap then, and a continue lists
    /// more matches rather than scanning further (#230: both caps used to
    /// read "hit scan cap").
    MatchCapped(usize),
    /// Ran out the time budget.
    Timeout,
    /// User pressed Stop.
    Cancelled,
}

/// The guardrails one run of a search is held to, each counted from the
/// run's start.
#[derive(Clone, Copy)]
struct SearchBudget {
    scan_cap: usize,
    time_budget_secs: u64,
    max_matches: usize,
}

impl SearchBudget {
    /// Whether a round ends the run, and why. The match cap comes first:
    /// the hits past it are held for a continue, so even the end of the
    /// keyspace is not the end of the list.
    fn stop_after_round(&self, truncated: bool, done: bool, scanned: usize, elapsed: Duration) -> Option<StopReason> {
        if truncated {
            Some(StopReason::MatchCapped(self.max_matches))
        } else if done {
            Some(StopReason::Done)
        } else if scanned >= self.scan_cap {
            Some(StopReason::ScanCapped)
        } else if elapsed.as_secs() >= self.time_budget_secs {
            Some(StopReason::Timeout)
        } else {
            None
        }
    }
}

/// Where a stopped search can go on from: what it was asked, as typed, and
/// the cursors past the last page whose hits are on screen.
struct Resume {
    prefix: String,
    query: String,
    spec: ValueSearchSpec,
    /// `None` until a first round has answered.
    cursors: Option<Vec<u64>>,
    /// Hits of the round that reached the match cap, past it: the cursors
    /// are already beyond their page, so a continue starts with them.
    held: Vec<ValueMatch>,
}

impl Resume {
    /// Whether anything is left to read: keys past the cursors, or hits held.
    fn has_more(&self) -> bool {
        !self.held.is_empty() || !self.cursors.as_ref().is_some_and(|c| c.iter().all(|&c| c == 0))
    }
}

/// State of the inline value preview for the selected match.
enum Preview {
    Loading,
    Value(SharedString),
    Error(SharedString),
}

/// Snapshot of one filtered result row for the virtualized list —
/// `uniform_list`'s range closure is `'static` and only receives
/// `&mut App`, so it renders from this owned copy instead of borrowing
/// the view. Clones are Arc bumps / small strings.
struct ResultRow {
    key: SharedString,
    key_type: SharedString,
    location: MatchLocation,
    is_selected: bool,
}

pub struct ZedisValueSearch {
    server_state: Entity<ZedisServerState>,
    prefix_input: Entity<InputState>,
    query_input: Entity<InputState>,
    /// Local filter over already-found matches (key substring).
    filter_input: Entity<InputState>,
    /// The one type to search, or every searchable type.
    only: Option<SearchableType>,
    /// The search on screen, for Continue.
    resume: Option<Resume>,
    /// One-shot flag: focus the prefix box on the first render only, so a
    /// freshly opened panel is ready to type into.
    should_focus: bool,
    running: bool,
    matches: Vec<ValueMatch>,
    scanned: usize,
    skipped: usize,
    /// Matches hit the configured match cap and further hits were dropped.
    truncated: bool,
    stop_reason: Option<StopReason>,
    error: Option<SharedString>,
    task: Option<Task<()>>,
    /// The server and database the search on screen ran in. The panel is
    /// kept while the route is elsewhere (`ServerView::survives_navigation`),
    /// so this is what says whose hits they are when the connection under
    /// it changes.
    searched: Option<(String, usize)>,

    /// Monotonic revision of `matches` (bumped on clear and on every result
    /// round) — the filter cache below keys on it instead of comparing
    /// contents.
    matches_rev: u64,
    /// Indices into `matches` that pass the key filter, cached under
    /// `filtered_signature` so plain repaints don't re-lowercase every key.
    filtered_cache: Vec<usize>,
    /// `(filter, matches_rev)` the cache was computed for.
    filtered_signature: Option<(String, u64)>,

    /// Currently-previewed match (also the highlighted row).
    selected: Option<SharedString>,
    selected_type: Option<SharedString>,
    selected_location: Option<MatchLocation>,
    preview: Option<Preview>,
    preview_task: Option<Task<()>>,
    preview_scroll: ScrollHandle,
    /// Re-words the three fields' placeholders after a language change.
    placeholders: Placeholders,
    /// `only`, as the query placeholder's wording reads it — set with it.
    query_type: Rc<Cell<Option<SearchableType>>>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisValueSearch {
    /// Focus this panel's primary search box (⌘F). The key tree — which
    /// normally answers ⌘F on server routes — is not rendered on this
    /// route, so without this the shortcut would target an off-screen
    /// input and appear to do nothing.
    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.query_input.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    /// Pick the type to search, and word the query field for it.
    fn set_type(&mut self, only: Option<SearchableType>, window: &mut Window, cx: &mut Context<Self>) {
        self.only = only;
        self.query_type.set(only);
        let text = query_placeholder(only, cx);
        self.query_input
            .update(cx, |state, cx| state.set_placeholder(text, window, cx));
        cx.notify();
    }

    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let prefix_input = cx.new(|cx| InputState::new(window, cx));
        let query_input = cx.new(|cx| InputState::new(window, cx));
        let filter_input = cx.new(|cx| InputState::new(window, cx));
        // The panel is kept across routes, so a language picked while it was
        // away has to reach its fields too (#210).
        let mut placeholders = Placeholders::new(cx);
        placeholders.add(
            &prefix_input,
            |cx| i18n_value_search(cx, "prefix_placeholder"),
            window,
            cx,
        );
        // The query's placeholder names what the picked type is searched
        // for (#239): the type is shared with the wording so a language
        // change re-words it for the type then selected.
        let query_type: Rc<Cell<Option<SearchableType>>> = Rc::new(Cell::new(None));
        let worded_type = query_type.clone();
        placeholders.add(
            &query_input,
            move |cx| query_placeholder(worded_type.get(), cx),
            window,
            cx,
        );
        placeholders.add(
            &filter_input,
            |cx| i18n_value_search(cx, "filter_placeholder"),
            window,
            cx,
        );

        // Enter in either field starts the search (when not already running).
        let mut subscriptions = vec![cx.subscribe_in(&query_input, window, |this, _s, event, _w, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) && !this.running {
                this.start_search(cx);
            }
        })];
        subscriptions.push(cx.subscribe_in(&prefix_input, window, |this, _s, event, _w, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) && !this.running {
                this.start_search(cx);
            }
        }));
        // Local filter is pure UI — re-render on every keystroke.
        subscriptions.push(cx.subscribe(&filter_input, |_this, _s, event, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        }));
        // A database switch is a `ServerSelected` too: the hits name keys of
        // the database they were found in, and a preview or an Open would
        // read the same name in the new one — and a running scan would keep
        // reading the old one. A reconnect is one as well, and after it the
        // hits are still this database's: only another target clears them.
        subscriptions.push(cx.subscribe(&server_state, |this, state, event: &ServerEvent, cx| {
            if !matches!(event, ServerEvent::ServerSelected(_)) {
                return;
            }
            let now = {
                let state = state.read(cx);
                (state.server_id().to_string(), state.db())
            };
            if this.searched.as_ref().is_some_and(|searched| *searched != now) {
                this.task = None;
                this.clear_results();
                this.searched = None;
                cx.notify();
            }
        }));

        Self {
            server_state,
            prefix_input,
            query_input,
            filter_input,
            only: None,
            resume: None,
            should_focus: true,
            running: false,
            matches: Vec::new(),
            scanned: 0,
            skipped: 0,
            truncated: false,
            stop_reason: None,
            error: None,
            task: None,
            searched: None,
            matches_rev: 0,
            filtered_cache: Vec::new(),
            filtered_signature: None,
            selected: None,
            selected_type: None,
            selected_location: None,
            preview: None,
            preview_task: None,
            preview_scroll: ScrollHandle::new(),
            placeholders,
            query_type,
            _subscriptions: subscriptions,
        }
    }

    /// The prefix and the query as the form holds them.
    fn typed(&self, cx: &App) -> (String, String) {
        (
            self.prefix_input.read(cx).value().trim().to_string(),
            self.query_input.read(cx).value().trim().to_string(),
        )
    }

    fn start_search(&mut self, cx: &mut Context<Self>) {
        let (prefix, query) = self.typed(cx);
        if prefix.is_empty() {
            self.error = Some(i18n_value_search(cx, "need_prefix"));
            cx.notify();
            return;
        }
        if query.is_empty() {
            self.error = Some(i18n_value_search(cx, "need_query"));
            cx.notify();
            return;
        }
        // Respect an explicit glob; otherwise pin the scan to `prefix*`.
        let pattern = if prefix.contains(['*', '?', '[']) {
            prefix.clone()
        } else {
            format!("{prefix}*")
        };
        let spec = ValueSearchSpec {
            pattern,
            needle_lower: query.to_lowercase(),
            only: self.only,
            max_value_bytes: MAX_VALUE_BYTES,
            max_container_elems: MAX_CONTAINER_ELEMS,
            page_count: PAGE_COUNT,
        };

        self.clear_results();
        // Read the connection live at search time, not at construction — a
        // restored `valuesearch` route recreates this view before
        // ServerSelected wires up the server, so a cached id would be empty
        // ("Redis config not found"). By search time the connection is ready.
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        self.searched = Some((server_id, db));
        self.resume = Some(Resume {
            prefix,
            query,
            spec,
            cursors: None,
            held: Vec::new(),
        });
        self.run_search(cx);
    }

    /// Whether the search on screen can go on: it was stopped rather than
    /// finished, something is left to read, and the form still asks what
    /// it asked — a changed prefix, query or type is a new search.
    fn resumable(&self, cx: &App) -> bool {
        if self.running
            || !matches!(
                self.stop_reason,
                Some(StopReason::ScanCapped | StopReason::MatchCapped(_) | StopReason::Timeout | StopReason::Cancelled)
            )
        {
            return false;
        }
        let Some(resume) = &self.resume else {
            return false;
        };
        let (prefix, query) = self.typed(cx);
        resume.has_more() && resume.prefix == prefix && resume.query == query && resume.spec.only == self.only
    }

    /// Go on with the search on screen from where it stopped, keeping its
    /// hits and counts, with a fresh budget.
    fn continue_search(&mut self, cx: &mut Context<Self>) {
        if !self.resumable(cx) {
            return;
        }
        self.stop_reason = None;
        self.truncated = false;
        self.error = None;
        self.run_search(cx);
    }

    /// Run the search in `resume` from its cursors until it is done or a
    /// budget — keys, time or matches, each counted from now — runs out.
    fn run_search(&mut self, cx: &mut Context<Self>) {
        let Some(resume) = self.resume.as_mut() else {
            return;
        };
        let spec = resume.spec.clone();
        let mut cursors = resume.cursors.clone();
        let held = take(&mut resume.held);
        let Some((server_id, db)) = self.searched.clone() else {
            return;
        };
        // Snapshot the tunable guardrails at search start — a mid-search
        // settings change applies to the next search, not this one.
        let budget = {
            let store = cx.global::<ZedisGlobalStore>().read(cx);
            SearchBudget {
                scan_cap: store.value_search_scan_cap(),
                time_budget_secs: store.value_search_time_budget_secs(),
                max_matches: store.value_search_max_matches(),
            }
        };
        let limit = self.matches.len() + budget.max_matches;
        self.running = true;
        self.admit(held, limit);
        if self.truncated {
            self.finish(StopReason::MatchCapped(budget.max_matches), cx);
            return;
        }
        cx.notify();

        self.task = Some(cx.spawn(async move |this, cx| {
            let at = ServerDb::new(&*server_id, db);
            let start = Instant::now();
            let mut scanned_here = 0;
            loop {
                // Off the UI thread: a round decodes and lower-cases every
                // value it reads to match it, which is CPU work that grows
                // with the values, not just a wait on the server.
                let round = cx.background_spawn({
                    let (at, spec, cursors) = (at.clone(), spec.clone(), cursors.clone());
                    async move { scan_values_round(&at, &spec, cursors).await }
                });
                let round = match round.await {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = this.update(cx, |this, cx| this.finish_error(e.to_string().into(), cx));
                        return;
                    }
                };
                let ValueSearchRound {
                    cursors: next,
                    matches,
                    scanned,
                    skipped_oversized,
                    done,
                } = round;
                scanned_here += scanned;
                let stop = this.update(cx, |this, cx| {
                    this.scanned += scanned;
                    this.skipped += skipped_oversized;
                    // The cursors move with the hits they were read with, so
                    // a Stop between rounds leaves no page half counted.
                    if let Some(resume) = this.resume.as_mut() {
                        resume.cursors = Some(next.clone());
                    }
                    this.admit(matches, limit);
                    cx.notify();
                    budget.stop_after_round(this.truncated, done, scanned_here, start.elapsed())
                });
                match stop {
                    Ok(Some(reason)) => {
                        let _ = this.update(cx, |this, cx| this.finish(reason, cx));
                        return;
                    }
                    Ok(None) => cursors = Some(next),
                    Err(_) => return, // view dropped
                }
            }
        }));
    }

    /// Put `found` on the list up to `limit` hits; past it they are held for
    /// a continue and the list is marked truncated.
    fn admit(&mut self, found: Vec<ValueMatch>, limit: usize) {
        let held = admit_up_to(&mut self.matches, found, limit);
        self.truncated |= !held.is_empty();
        if let Some(resume) = self.resume.as_mut() {
            resume.held = held;
        }
        self.matches_rev += 1;
    }

    /// Forget the previous search's hits, summary and preview.
    fn clear_results(&mut self) {
        self.resume = None;
        self.matches.clear();
        self.matches_rev += 1;
        self.scanned = 0;
        self.skipped = 0;
        self.truncated = false;
        self.stop_reason = None;
        self.error = None;
        self.running = false;
        self.selected = None;
        self.selected_type = None;
        self.selected_location = None;
        self.preview = None;
        self.preview_task = None;
    }

    fn finish(&mut self, reason: StopReason, cx: &mut Context<Self>) {
        self.running = false;
        self.stop_reason = Some(reason);
        cx.notify();
    }

    fn finish_error(&mut self, msg: SharedString, cx: &mut Context<Self>) {
        self.running = false;
        self.error = Some(msg);
        cx.notify();
    }

    fn stop_search(&mut self, cx: &mut Context<Self>) {
        self.task = None; // drop → the loop ends at its next await
        self.running = false;
        self.stop_reason = Some(StopReason::Cancelled);
        cx.notify();
    }

    fn apply_example_prefix(&mut self, prefix: &str, window: &mut Window, cx: &mut Context<Self>) {
        let p: SharedString = prefix.to_string().into();
        self.prefix_input.update(cx, |input, cx| {
            input.set_value(p, window, cx);
        });
        self.error = None;
        cx.notify();
    }

    /// Preview a matched key's value inline (right pane), without leaving.
    fn select_result(
        &mut self,
        key: SharedString,
        key_type: SharedString,
        location: MatchLocation,
        cx: &mut Context<Self>,
    ) {
        self.selected = Some(key.clone());
        self.selected_type = Some(key_type);
        self.selected_location = Some(location);
        self.preview = Some(Preview::Loading);
        cx.notify();
        self.load_preview(key, cx);
    }

    /// Read `key`'s value into the preview pane, leaving what the pane
    /// shows in place until the answer is in.
    fn load_preview(&mut self, key: SharedString, cx: &mut Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        self.preview_task = Some(cx.spawn(async move |this, cx| {
            let fetched = async { value_preview(&ServerDb::new(&*server_id, db), &key).await }.await;
            let _ = this.update(cx, |this, cx| {
                // Ignore stale previews if the user already clicked another row.
                if this.selected.as_deref() != Some(key.as_ref()) {
                    return;
                }
                this.preview = Some(match fetched {
                    Ok(text) => Preview::Value(truncate_preview(text)),
                    Err(e) => Preview::Error(e.to_string().into()),
                });
                cx.notify();
            });
        }));
    }

    /// The route is back on this panel after having been elsewhere.
    ///
    /// With nothing found it is a form again, and the caret goes where a
    /// new panel puts it; with hits on screen it stays out of the fields,
    /// since the list is what was come back for. The value in the preview
    /// was read before the route left and "Open" leads to that key's
    /// editor, so it is read again rather than shown as it was.
    pub fn shown_again(&mut self, cx: &mut Context<Self>) {
        self.should_focus = self.matches.is_empty() && !self.running;
        if let Some(key) = self.selected.clone() {
            self.load_preview(key, cx);
        }
        cx.notify();
    }

    /// Optional jump from the preview: select the key and switch to the editor.
    fn open_key(&mut self, key: SharedString, cx: &mut Context<Self>) {
        self.server_state.update(cx, |state, cx| state.select_key(key, cx));
        cx.global::<ZedisGlobalStore>()
            .clone()
            .update(cx, |state, cx| state.go_to_view(ServerView::Editor, cx));
    }

    fn copy_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Preview::Value(text)) = &self.preview else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
        window.push_notification(Notification::info(i18n_common(cx, "copied_to_clipboard")), cx);
    }

    fn copy_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = &self.selected else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(key.to_string()));
        window.push_notification(Notification::info(i18n_common(cx, "copied_to_clipboard")), cx);
    }

    /// Export the current hits to a CSV (`key`, `type`, where it matched).
    fn export_csv(&mut self, cx: &mut Context<Self>) {
        if self.matches.is_empty() {
            return;
        }
        let rows: Vec<Vec<String>> = self
            .matches
            .iter()
            .map(|m| vec![m.key.to_string(), m.key_type.to_string(), location_csv(&m.location)])
            .collect();
        let csv = build_csv(&["key", "type", "match"], &rows);
        let server_state = self.server_state.clone();
        let success = i18n_common(cx, "csv_exported");
        let error = i18n_common(cx, "csv_export_failed");
        export_to_file(cx, server_state, csv.into_bytes(), "value-search.csv", success, error);
    }

    fn filter_text(&self, cx: &Context<Self>) -> String {
        self.filter_input.read(cx).value().trim().to_lowercase()
    }

    /// Refresh `filtered_cache` for `filter` (already lowercased). Keyed on
    /// `(filter, matches_rev)`: plain repaints (hover, selection, preview
    /// loads) reuse the cache; only typing in the filter box or a new result
    /// round re-lowercases the keys.
    fn ensure_filtered(&mut self, filter: &str) {
        if self
            .filtered_signature
            .as_ref()
            .is_some_and(|(f, rev)| f == filter && *rev == self.matches_rev)
        {
            return;
        }
        self.filtered_cache = self
            .matches
            .iter()
            .enumerate()
            .filter(|(_, m)| filter.is_empty() || m.key.to_lowercase().contains(filter))
            .map(|(i, _)| i)
            .collect();
        self.filtered_signature = Some((filter.to_string(), self.matches_rev));
    }

    fn reason_label(&self, cx: &Context<Self>) -> SharedString {
        match self.stop_reason {
            Some(StopReason::Done) => i18n_value_search(cx, "reason_done"),
            Some(StopReason::ScanCapped) => i18n_value_search(cx, "reason_capped"),
            Some(StopReason::MatchCapped(cap)) => rust_i18n::t!(
                "value_search.reason_match_capped",
                cap = group_thousands(cap as u64),
                locale = cx.global::<ZedisGlobalStore>().read(cx).locale()
            )
            .to_string()
            .into(),
            Some(StopReason::Timeout) => i18n_value_search(cx, "reason_timeout"),
            Some(StopReason::Cancelled) => i18n_value_search(cx, "reason_cancelled"),
            None if self.running => i18n_value_search(cx, "searching"),
            None => SharedString::default(),
        }
    }

    /// A guardrail sentence with the budget Settings gives a search now.
    fn with_budget(&self, key: &str, cx: &App) -> SharedString {
        let store = cx.global::<ZedisGlobalStore>().read(cx);
        rust_i18n::t!(
            key,
            cap = group_thousands(store.value_search_scan_cap() as u64),
            secs = store.value_search_time_budget_secs(),
            locale = store.locale()
        )
        .to_string()
        .into()
    }

    fn render_stat_chip(
        label: SharedString,
        value: SharedString,
        muted: gpui::Hsla,
        border: gpui::Hsla,
    ) -> gpui::AnyElement {
        h_flex()
            .gap_1()
            .items_center()
            .px_2()
            .py_0p5()
            .rounded_md()
            .border_1()
            .border_color(border)
            .child(Label::new(label).text_xs().text_color(muted))
            .child(Label::new(value).text_xs().font_semibold())
            .into_any_element()
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let danger = cx.theme().danger;
        let warning = cx.theme().warning;

        if let Some(err) = &self.error {
            return div()
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(danger)
                .bg(danger.opacity(0.1))
                .child(Label::new(err.clone()).text_xs().text_color(danger))
                .into_any_element();
        }

        if !self.running && self.stop_reason.is_none() {
            return Label::new(self.with_budget("value_search.guardrails_hint", cx))
                .text_xs()
                .text_color(muted)
                .into_any_element();
        }

        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let mut row = h_flex().gap_2().items_center().flex_wrap();
        row = row.child(Self::render_stat_chip(
            i18n_value_search(cx, "stat_scanned"),
            SharedString::from(self.scanned.to_string()),
            muted,
            border,
        ));
        row = row.child(Self::render_stat_chip(
            i18n_value_search(cx, "stat_matched"),
            SharedString::from(self.matches.len().to_string()),
            muted,
            border,
        ));
        row = row.child(Self::render_stat_chip(
            i18n_value_search(cx, "stat_skipped"),
            SharedString::from(self.skipped.to_string()),
            muted,
            border,
        ));
        let reason = self.reason_label(cx);
        if !reason.is_empty() {
            row = row.child(Self::render_stat_chip(
                i18n_value_search(cx, "stat_status"),
                reason,
                muted,
                border,
            ));
        }
        if self.truncated {
            row = row.child(
                Label::new(i18n_value_search(cx, "truncated"))
                    .text_xs()
                    .text_color(warning),
            );
        }
        if self.resumable(cx) {
            row = row.child(
                Button::new(CONTINUE_BUTTON_ID)
                    .outline()
                    .xsmall()
                    .icon(IconName::Play)
                    .label(i18n_value_search(cx, "continue"))
                    .tooltip(i18n_value_search(cx, "continue_tooltip"))
                    .on_click(cx.listener(|this, _, _w, cx| this.continue_search(cx))),
            );
        }
        // Cap reminder while running.
        if self.running {
            let store = cx.global::<ZedisGlobalStore>().read(cx);
            let budget: SharedString = rust_i18n::t!(
                "value_search.budget_running",
                cap = store.value_search_scan_cap(),
                secs = store.value_search_time_budget_secs(),
                locale = locale
            )
            .to_string()
            .into();
            row = row.child(Label::new(budget).text_xs().text_color(muted));
        }
        row.into_any_element()
    }

    fn render_empty_state(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let mut examples = h_flex().gap_2().flex_wrap();
        for p in EXAMPLE_PREFIXES {
            let prefix = (*p).to_string();
            let label = (*p).to_string();
            examples = examples.child(
                Button::new(SharedString::from(format!("vs-ex-{p}")))
                    .outline()
                    .small()
                    .label(SharedString::from(label))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.apply_example_prefix(&prefix, window, cx);
                    })),
            );
        }

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .p_6()
            .child(Icon::new(IconName::Search).text_color(muted))
            .child(Label::new(i18n_value_search(cx, "empty_title")).font_semibold())
            .child(
                Label::new(i18n_value_search(cx, "empty_body"))
                    .text_sm()
                    .text_color(muted)
                    .text_center(),
            )
            .child(
                v_flex()
                    .gap_1()
                    .max_w(px(480.))
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .child(
                        Label::new(i18n_value_search(cx, "empty_guardrails_title"))
                            .text_xs()
                            .font_semibold(),
                    )
                    .child(
                        Label::new(self.with_budget("value_search.empty_guardrails_body", cx))
                            .text_xs()
                            .text_color(muted),
                    ),
            )
            .child(
                Label::new(i18n_value_search(cx, "empty_examples"))
                    .text_xs()
                    .text_color(muted),
            )
            .child(examples)
            .into_any_element()
    }

    fn render_results(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;

        if self.matches.is_empty() {
            let msg = if self.running {
                i18n_value_search(cx, "searching")
            } else if self.stop_reason.is_some() {
                i18n_value_search(cx, "no_matches")
            } else {
                return div().into_any_element();
            };
            return div()
                .p_4()
                .child(Label::new(msg).text_sm().text_color(muted))
                .into_any_element();
        }

        if self.filtered_cache.is_empty() {
            return div()
                .p_4()
                .child(
                    Label::new(i18n_value_search(cx, "filter_no_matches"))
                        .text_sm()
                        .text_color(muted),
                )
                .into_any_element();
        }

        // Stronger than the usual `table_even` so the match list is easy to
        // scan row-by-row.
        let stripe = cx.theme().muted.opacity(0.5);
        let active = cx.theme().list_active;
        let hover = cx.theme().table_hover;
        let border = cx.theme().border;

        // Snapshot everything the row closure needs up front —
        // `uniform_list`'s range callback is `'static`, so it renders from
        // this owned copy instead of borrowing the view.
        let selected = self.selected.clone();
        let loc_field = i18n_value_search(cx, "loc_field");
        let loc_member = i18n_value_search(cx, "loc_member");
        let open_tooltip = i18n_value_search(cx, "open");
        let rows: Arc<Vec<ResultRow>> = Arc::new(
            self.filtered_cache
                .iter()
                .map(|&i| {
                    let vm = &self.matches[i];
                    ResultRow {
                        key: SharedString::from(vm.key.clone()),
                        key_type: SharedString::from(vm.key_type.clone()),
                        location: vm.location.clone(),
                        is_selected: selected.as_deref() == Some(vm.key.as_str()),
                    }
                })
                .collect(),
        );
        let entity = cx.entity();

        // Virtualized: only the rows inside the viewport build elements —
        // the previous version materialised up to MAX_MATCHES interactive
        // rows on every repaint.
        let list = uniform_list("vs-results-list", rows.len(), move |range, _window, _cx| {
            let mut out = Vec::with_capacity(range.len());
            for ix in range {
                let row = &rows[ix];
                let is_stripe = ix % 2 != 0;
                let is_selected = row.is_selected;
                let key_type = KeyType::from(row.key_type.as_ref());
                // A muted second line names where the needle matched (field /
                // index / member); plain string values carry no location.
                let loc_label: Option<SharedString> = match &row.location {
                    MatchLocation::Value => None,
                    MatchLocation::Field(f) => Some(format!("{loc_field}: {f}").into()),
                    MatchLocation::Index(i) => Some(format!("[{i}]").into()),
                    MatchLocation::Member(m) => Some(format!("{loc_member}: {m}").into()),
                };
                let select_entity = entity.clone();
                let select_key = row.key.clone();
                let select_type = row.key_type.clone();
                let select_loc = row.location.clone();
                let open_entity = entity.clone();
                let open_key = row.key.clone();
                out.push(
                    h_flex()
                        .id(SharedString::from(format!("vs-row-{ix}")))
                        .w_full()
                        // Fixed row height — `uniform_list` sizes every row
                        // from the first; tall enough for the location line.
                        .h(px(40.))
                        .items_center()
                        .gap_2()
                        .px_2()
                        .cursor_pointer()
                        .when(is_stripe && !is_selected, |this| this.bg(stripe))
                        .when(is_selected, |this| this.bg(active))
                        .when(!is_selected, |this| this.hover(move |s| s.bg(hover)))
                        .on_click(move |_, _w, cx| {
                            let key = select_key.clone();
                            let key_type = select_type.clone();
                            let location = select_loc.clone();
                            select_entity.update(cx, |this, cx| {
                                this.select_result(key, key_type, location, cx);
                            });
                        })
                        .child(KeyTypeBadge::new(key_type).plain(true))
                        .child(
                            v_flex()
                                .min_w_0()
                                .flex_1()
                                .child(Label::new(row.key.clone()).text_xs().truncate())
                                .when_some(loc_label, |col, loc| {
                                    col.child(Label::new(loc).text_xs().text_color(muted).truncate())
                                }),
                        )
                        .child(
                            Button::new(SharedString::from(format!("vs-open-row-{ix}")))
                                .ghost()
                                .small()
                                .icon(IconName::ArrowRight)
                                .tooltip(open_tooltip.clone())
                                .on_click(move |_, _w, cx| {
                                    let key = open_key.clone();
                                    open_entity.update(cx, |this, cx| this.open_key(key, cx));
                                }),
                        ),
                );
            }
            out
        })
        .flex_1()
        .min_h_0()
        .w_full();

        let mut col = v_flex().size_full().child(list);
        if self.truncated {
            col = col.child(
                div().px_3().py_1().border_t_1().border_color(border).child(
                    Label::new(i18n_value_search(cx, "truncated"))
                        .text_xs()
                        .text_color(cx.theme().warning),
                ),
            );
        }
        col.into_any_element()
    }

    fn render_preview(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        match &self.preview {
            None => div()
                .size_full()
                .p_4()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Label::new(i18n_value_search(cx, "preview_hint"))
                        .text_sm()
                        .text_color(muted)
                        .text_center(),
                )
                .into_any_element(),
            Some(Preview::Loading) => div()
                .p_4()
                .child(Label::new(i18n_value_search(cx, "loading")).text_sm().text_color(muted))
                .into_any_element(),
            Some(Preview::Error(e)) => div()
                .p_4()
                .child(Label::new(e.clone()).text_sm().text_color(cx.theme().danger))
                .into_any_element(),
            Some(Preview::Value(text)) => {
                let key = self.selected.clone().unwrap_or_default();
                let key_open = key.clone();
                let key_type = self
                    .selected_type
                    .as_deref()
                    .map(KeyType::from)
                    .unwrap_or(KeyType::Unknown);
                let loc_label: Option<SharedString> = match &self.selected_location {
                    Some(MatchLocation::Field(f)) => {
                        Some(format!("{}: {f}", i18n_value_search(cx, "loc_field")).into())
                    }
                    Some(MatchLocation::Index(i)) => Some(format!("[{i}]").into()),
                    Some(MatchLocation::Member(m)) => {
                        Some(format!("{}: {m}", i18n_value_search(cx, "loc_member")).into())
                    }
                    _ => None,
                };
                // Content of a scroll viewport: at least pane-height, free to
                // grow with the value so `vs-preview`'s `overflow_scroll` has
                // a range (`size_full` would pin it to exactly one screen).
                v_flex()
                    .w_full()
                    .min_h_full()
                    .gap_2()
                    .p_3()
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_center()
                            .child(KeyTypeBadge::new(key_type).plain(true))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(Label::new(key).text_xs().font_semibold().truncate()),
                            )
                            .when_some(loc_label, |row, loc| {
                                row.child(Label::new(loc).text_xs().text_color(muted))
                            })
                            .child(
                                Button::new("vs-copy-key")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Copy)
                                    .tooltip(i18n_value_search(cx, "copy_key_tooltip"))
                                    .on_click(cx.listener(|this, _, w, cx| this.copy_key(w, cx))),
                            )
                            .child(
                                Button::new("vs-copy-value")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Copy)
                                    .tooltip(i18n_value_search(cx, "copy_value_tooltip"))
                                    .on_click(cx.listener(|this, _, w, cx| this.copy_preview(w, cx))),
                            )
                            .child(
                                Button::new("vs-open")
                                    .small()
                                    .outline()
                                    .label(i18n_value_search(cx, "open"))
                                    .on_click(cx.listener(move |this, _, _w, cx| this.open_key(key_open.clone(), cx))),
                            ),
                    )
                    .child(
                        div()
                            .w_full()
                            .text_xs()
                            .font_family(get_mono_font_family())
                            .child(text.clone()),
                    )
                    .into_any_element()
            }
        }
    }
}

/// Move `found` onto `matches` until it holds `limit`, and answer the rest.
fn admit_up_to(matches: &mut Vec<ValueMatch>, found: Vec<ValueMatch>, limit: usize) -> Vec<ValueMatch> {
    let room = limit.saturating_sub(matches.len());
    let mut found = found.into_iter();
    matches.extend(found.by_ref().take(room));
    found.collect()
}

/// The type filter as the panel's action names it.
fn type_filter(only: Option<SearchableType>) -> ValueSearchTypeFilter {
    match only {
        None => ValueSearchTypeFilter::All,
        Some(SearchableType::String) => ValueSearchTypeFilter::String,
        Some(SearchableType::Hash) => ValueSearchTypeFilter::Hash,
        Some(SearchableType::List) => ValueSearchTypeFilter::List,
        Some(SearchableType::Set) => ValueSearchTypeFilter::Set,
        Some(SearchableType::Zset) => ValueSearchTypeFilter::Zset,
    }
}

/// …and back.
fn searchable(filter: ValueSearchTypeFilter) -> Option<SearchableType> {
    match filter {
        ValueSearchTypeFilter::All => None,
        ValueSearchTypeFilter::String => Some(SearchableType::String),
        ValueSearchTypeFilter::Hash => Some(SearchableType::Hash),
        ValueSearchTypeFilter::List => Some(SearchableType::List),
        ValueSearchTypeFilter::Set => Some(SearchableType::Set),
        ValueSearchTypeFilter::Zset => Some(SearchableType::Zset),
    }
}

/// A type's name as the key tree's type filter writes it.
/// The query field's placeholder: what is read of the picked type — a hash's
/// field names and values, a list's items, a set's members — or every type
/// when none is picked (#239).
fn query_placeholder(only: Option<SearchableType>, cx: &App) -> SharedString {
    let key = match only {
        None => "query_placeholder",
        Some(SearchableType::String) => "query_placeholder_string",
        Some(SearchableType::Hash) => "query_placeholder_hash",
        Some(SearchableType::List) => "query_placeholder_list",
        Some(SearchableType::Set) => "query_placeholder_set",
        Some(SearchableType::Zset) => "query_placeholder_zset",
    };
    i18n_value_search(cx, key)
}

fn type_label(only: SearchableType) -> &'static str {
    match only {
        SearchableType::String => "String",
        SearchableType::Hash => "Hash",
        SearchableType::List => "List",
        SearchableType::Set => "Set",
        SearchableType::Zset => "Zset",
    }
}

/// Format a match location for a CSV cell (machine-readable, no i18n).
fn location_csv(loc: &MatchLocation) -> String {
    match loc {
        MatchLocation::Value => "value".to_string(),
        MatchLocation::Field(f) => format!("field:{f}"),
        MatchLocation::Index(i) => format!("[{i}]"),
        MatchLocation::Member(m) => format!("member:{m}"),
    }
}

fn truncate_preview(mut text: String) -> SharedString {
    if text.len() > PREVIEW_MAX_BYTES {
        let mut idx = PREVIEW_MAX_BYTES;
        while idx > 0 && !text.is_char_boundary(idx) {
            idx -= 1;
        }
        text.truncate(idx);
        text.push_str("\n… (truncated)");
    }
    text.into()
}

impl Render for ZedisValueSearch {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.placeholders.follow(window, cx);
        if take(&mut self.should_focus) {
            self.prefix_input.update(cx, |state, cx| state.focus(window, cx));
        }
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let show_results_pane = self.running || self.stop_reason.is_some() || !self.matches.is_empty();

        let action = if self.running {
            Button::new("vs-stop")
                .outline()
                .small()
                .label(i18n_value_search(cx, "stop"))
                .on_click(cx.listener(|this, _, _w, cx| this.stop_search(cx)))
        } else {
            Button::new("vs-search")
                .outline()
                .small()
                .icon(IconName::Search)
                .label(i18n_value_search(cx, "search"))
                .on_click(cx.listener(|this, _, _w, cx| this.start_search(cx)))
        };

        let only = self.only;
        let all_types = i18n_value_search(cx, "type_all");
        let type_picker = Button::new("vs-type")
            .outline()
            .small()
            .label(only.map_or(all_types.clone(), |t| type_label(t).into()))
            .dropdown_caret(true)
            .tooltip(i18n_value_search(cx, "type_tooltip"))
            // What a running search reads was fixed when it started.
            .disabled(self.running)
            .dropdown_menu(move |menu, _window, _cx| {
                let menu =
                    menu.menu_with_check(all_types.clone(), only.is_none(), Box::new(ValueSearchTypeFilter::All));
                SearchableType::ALL.into_iter().fold(menu, |menu, t| {
                    menu.menu_with_check(type_label(t), only == Some(t), Box::new(type_filter(Some(t))))
                })
            });

        let header = h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .child(div().w(px(220.)).child(Input::new(&self.prefix_input).small()))
            .child(type_picker)
            .child(div().flex_1().child(Input::new(&self.query_input).small()))
            .child(action)
            .when(saves_files() && !self.matches.is_empty(), |this| {
                this.child(
                    Button::new("vs-export-csv")
                        .outline()
                        .small()
                        .label(i18n_common(cx, "export_csv"))
                        .tooltip(i18n_value_search(cx, "export_tooltip"))
                        .on_click(cx.listener(|this, _, _w, cx| this.export_csv(cx))),
                )
            });

        let body: gpui::AnyElement = if !show_results_pane {
            self.render_empty_state(cx)
        } else {
            // Refresh the filter cache once per frame; the results list and
            // the shown/total label both read it.
            let filter = self.filter_text(cx);
            self.ensure_filtered(&filter);
            let results = self.render_results(cx);
            let preview = self.render_preview(cx);
            let filter_bar = h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .px_2()
                .py_1()
                .border_b_1()
                .border_color(border)
                .child(
                    Label::new(SharedString::from({
                        let shown = self.filtered_cache.len();
                        let total = self.matches.len();
                        if filter.is_empty() {
                            format!("{total}")
                        } else {
                            format!("{shown}/{total}")
                        }
                    }))
                    .text_xs()
                    .text_color(muted),
                )
                .child(div().flex_1().child(Input::new(&self.filter_input).small()));

            h_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .gap_0()
                .child(
                    v_flex()
                        .w(px(340.))
                        .h_full()
                        .border_r_1()
                        .border_color(border)
                        .child(filter_bar)
                        .child(
                            // `uniform_list` scrolls itself — no outer
                            // overflow/scroll-handle wrapper needed — but only
                            // if a definite height reaches it: this wrapper
                            // must be a flex container (a bare `div()` is
                            // `display: Block`, which drops the height chain
                            // and collapses the list to zero rows).
                            v_flex().id("vs-results").flex_1().min_h_0().w_full().child(results),
                        ),
                )
                .child(
                    div()
                        .id("vs-preview")
                        .flex_1()
                        .h_full()
                        .min_w_0()
                        .overflow_scroll()
                        .track_scroll(&self.preview_scroll)
                        .child(preview),
                )
                .into_any_element()
        };

        v_flex()
            .size_full()
            .font_family(get_mono_font_family())
            // The type dropdown dispatches these.
            .on_action(cx.listener(|this, filter: &ValueSearchTypeFilter, window, cx| {
                this.set_type(searchable(*filter), window, cx);
            }))
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .h(px(40.))
                    .border_b_1()
                    .border_color(border)
                    .child(
                        Button::new("vs-back")
                            .ghost()
                            .small()
                            .icon(IconName::ArrowLeft)
                            .tooltip(back_to_editor_tooltip(cx))
                            .on_click(|_, _w, cx| {
                                cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                    store.update(cx, |state, cx| state.go_to_view(ServerView::Editor, cx));
                                });
                            }),
                    )
                    .child(Label::new(i18n_value_search(cx, "title")).font_semibold()),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .gap_2()
                    .p_3()
                    .child(header)
                    .child(self.render_status_bar(cx))
                    // Flex wrapper (not a bare block `div()`) so `body` keeps
                    // a definite height — the virtualized results list sizes
                    // its viewport from it.
                    .child(v_flex().flex_1().min_h_0().w_full().child(body)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::init_ui;
    use gpui::{AppContext, TestAppContext, size};
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;

    fn hit(key: &str) -> ValueMatch {
        ValueMatch {
            key: key.to_string(),
            key_type: "string".to_string(),
            location: MatchLocation::Value,
        }
    }

    /// The hits past the match cap are answered, not dropped: their page is
    /// behind the cursors, so a continue is the only way back to them.
    #[test]
    fn hits_past_the_cap_are_held_for_a_continue() {
        let mut matches = vec![hit("a")];
        let held = admit_up_to(&mut matches, vec![hit("b"), hit("c"), hit("d")], 3);
        let keys = |hits: &[ValueMatch]| hits.iter().map(|m| m.key.clone()).collect::<Vec<_>>();
        assert_eq!(keys(&matches), ["a", "b", "c"]);
        assert_eq!(keys(&held), ["d"]);
        assert!(
            admit_up_to(&mut matches, vec![hit("e")], 3).len() == 1,
            "a full list takes none"
        );
    }

    /// A run that fills its match list stops for that, not for the scan cap
    /// it may be far from: the status read "hit scan cap" after 1,024 of
    /// 10,000 keys (#230).
    #[test]
    fn a_full_match_list_stops_for_the_match_cap_not_the_scan_cap() {
        let budget = SearchBudget {
            scan_cap: 10_000,
            time_budget_secs: 10,
            max_matches: 500,
        };
        let early = Duration::from_secs(1);
        assert_eq!(
            budget.stop_after_round(true, false, 1_024, early),
            Some(StopReason::MatchCapped(500))
        );
        assert_eq!(
            budget.stop_after_round(true, true, 1_024, early),
            Some(StopReason::MatchCapped(500)),
            "hits held past the cap outlast the keyspace's end"
        );
        assert_eq!(
            budget.stop_after_round(false, false, 10_000, early),
            Some(StopReason::ScanCapped)
        );
        assert_eq!(
            budget.stop_after_round(false, true, 10_000, early),
            Some(StopReason::Done)
        );
        assert_eq!(
            budget.stop_after_round(false, false, 1_024, Duration::from_secs(10)),
            Some(StopReason::Timeout)
        );
        assert_eq!(budget.stop_after_round(false, false, 1_024, early), None);
    }

    /// The status names the cap that stopped the search, and the match cap
    /// its number.
    #[gpui::test]
    fn the_status_names_the_cap_that_stopped_the_search(cx: &mut TestAppContext) {
        init_ui(cx);
        cx.update(|cx| {
            let store = cx.global::<ZedisGlobalStore>().clone();
            store.update(cx, |state, _| state.set_locale("en".to_string()));
        });
        let server_state = cx.new(|_| ZedisServerState::default());
        let mut panel = None;
        cx.open_window(size(px(1000.), px(600.)), |window, cx| {
            let view = cx.new(|cx| ZedisValueSearch::new(server_state.clone(), window, cx));
            panel = Some(view.clone());
            Root::new(view, window, cx)
        });
        let panel = panel.expect("the window built the panel");
        let status = |reason: StopReason, cx: &mut TestAppContext| {
            panel.update(cx, |this, cx| {
                this.stop_reason = Some(reason);
                this.reason_label(cx).to_string()
            })
        };
        assert_eq!(status(StopReason::MatchCapped(500), cx), "hit match cap (500)");
        assert_eq!(status(StopReason::MatchCapped(2_000), cx), "hit match cap (2,000)");
        assert_eq!(status(StopReason::ScanCapped, cx), "hit scan cap");
    }

    /// Continue is offered for a search that stopped with something left to
    /// read, and only while the form still asks what that search asked.
    #[gpui::test]
    fn continue_is_offered_while_the_form_asks_what_the_stopped_search_did(cx: &mut TestAppContext) {
        init_ui(cx);
        let server_state = cx.new(|_| ZedisServerState::default());
        let mut panel = None;
        let handle = cx.open_window(size(px(1000.), px(600.)), |window, cx| {
            let view = cx.new(|cx| ZedisValueSearch::new(server_state.clone(), window, cx));
            panel = Some(view.clone());
            Root::new(view, window, cx)
        });
        let panel = panel.expect("the window built the panel");
        let shows_continue = |cx: &mut TestAppContext| {
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                window.try_find(CONTINUE_BUTTON_ID).is_some()
            })
            .expect("the window is open")
        };
        let type_into = |cx: &mut TestAppContext, prefix: &'static str, query: &'static str| {
            cx.update_window(handle.into(), |_, window, cx| {
                panel.update(cx, |this, cx| {
                    this.prefix_input.update(cx, |i, cx| i.set_value(prefix, window, cx));
                    this.query_input.update(cx, |i, cx| i.set_value(query, window, cx));
                });
            })
            .expect("the window is open");
        };

        type_into(cx, "user:", "alice");
        assert!(!shows_continue(cx), "nothing has been searched");
        panel.update(cx, |this, _| {
            this.searched = Some((String::new(), 0));
            this.stop_reason = Some(StopReason::Timeout);
            this.resume = Some(Resume {
                prefix: "user:".to_string(),
                query: "alice".to_string(),
                spec: ValueSearchSpec {
                    pattern: "user:*".to_string(),
                    needle_lower: "alice".to_string(),
                    only: None,
                    max_value_bytes: MAX_VALUE_BYTES,
                    max_container_elems: MAX_CONTAINER_ELEMS,
                    page_count: PAGE_COUNT,
                },
                cursors: Some(vec![42]),
                held: Vec::new(),
            });
        });
        assert!(shows_continue(cx), "a search the budget stopped");

        type_into(cx, "user:", "bob");
        assert!(!shows_continue(cx), "another query is a new search");
        type_into(cx, "user:", "alice");
        assert!(shows_continue(cx));

        // The type is picked from the dropdown, whose items dispatch this.
        cx.update_window(handle.into(), |_, window, cx| {
            window.dispatch_action(Box::new(ValueSearchTypeFilter::Hash), cx);
        })
        .expect("the window is open");
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("vs-type").label(), Some("Hash"));
        })
        .expect("the window is open");
        // And the query field says what a hash is searched for (#239).
        let placeholder = |cx: &mut TestAppContext| {
            panel.read_with(cx, |this, cx| {
                this.query_input.read(cx).presentation().placeholder().to_string()
            })
        };
        assert_eq!(placeholder(cx), "Text to find in hash field names or values");
        assert!(!shows_continue(cx), "another type is a new search");
        panel.update(cx, |this, _| this.only = None);
        assert!(shows_continue(cx));

        panel.update(cx, |this, _| {
            if let Some(resume) = this.resume.as_mut() {
                resume.cursors = Some(vec![0]);
            }
        });
        assert!(!shows_continue(cx), "the keyspace is covered");
        panel.update(cx, |this, _| {
            if let Some(resume) = this.resume.as_mut() {
                resume.held = vec![hit("user:9")];
            }
        });
        assert!(shows_continue(cx), "hits held past the cap are still to come");
        panel.update(cx, |this, _| this.stop_reason = Some(StopReason::Done));
        assert!(!shows_continue(cx), "a finished search");
    }
}
