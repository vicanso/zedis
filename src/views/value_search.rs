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
//! - **Scan + time caps** — stops after [`SCAN_CAP`] keys or
//!   [`TIME_BUDGET_SECS`], whichever first; cancellable via the Stop button
//!   (dropping the task ends the loop at its next await).
//! - **Per-value size gate** — values larger than [`MAX_VALUE_BYTES`] are
//!   skipped (not pulled down just to grep), and counted.
//! - **Sampling semantics** — the summary states what was scanned / matched /
//!   skipped and why it stopped; results are never claimed exhaustive.
//!
//! Clicking a hit previews its value **inline** in the right-hand pane (the
//! "Open" button there still jumps to the editor). MVP scope: string values,
//! case-insensitive substring. (Hash / list / set / zset members and regex
//! are deliberately left for later.)

use crate::connection::{MatchLocation, ValueMatch, ValueSearchRound, get_connection_manager};
use crate::helpers::{build_csv, get_mono_font_family};
use crate::states::{Route, ZedisGlobalStore, ZedisServerState, i18n_common, i18n_value_search};
use crate::views::export_to_file;
use gpui::{Context, Entity, ScrollHandle, SharedString, Task, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, IconName, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    label::Label,
    v_flex,
};
use std::time::Instant;

/// Hard cap on keys examined per search.
const SCAN_CAP: usize = 10_000;
/// Wall-clock budget per search.
const TIME_BUDGET_SECS: u64 = 10;
/// Values larger than this (bytes) are skipped, not read.
const MAX_VALUE_BYTES: u64 = 1024 * 1024;
/// Containers (hash/list/set/zset) with more than this many elements are
/// skipped, not read whole.
const MAX_CONTAINER_ELEMS: u64 = 10_000;
/// `SCAN COUNT` per master per round — kept modest so cancellation / the time
/// check stay responsive between rounds.
const PAGE_COUNT: u64 = 128;
/// Cap on matches kept (a search shouldn't flood the UI with thousands).
const MAX_MATCHES: usize = 500;
/// Bytes of a value rendered in the preview pane before truncating.
const PREVIEW_MAX_BYTES: usize = 64 * 1024;

/// Why a search loop stopped — drives the honest summary line.
#[derive(Clone, Copy, PartialEq)]
enum StopReason {
    /// Whole (prefix-scoped) keyspace covered.
    Done,
    /// Hit the scan-count or match cap.
    Capped,
    /// Ran out the time budget.
    Timeout,
    /// User pressed Stop.
    Cancelled,
}

/// State of the inline value preview for the selected match.
enum Preview {
    Loading,
    Value(SharedString),
    Error(SharedString),
}

pub struct ZedisValueSearch {
    server_state: Entity<ZedisServerState>,
    server_id: String,
    db: usize,
    prefix_input: Entity<InputState>,
    query_input: Entity<InputState>,
    running: bool,
    matches: Vec<ValueMatch>,
    scanned: usize,
    skipped: usize,
    /// Matches hit [`MAX_MATCHES`] and further hits were dropped.
    truncated: bool,
    stop_reason: Option<StopReason>,
    error: Option<SharedString>,
    task: Option<Task<()>>,
    scroll: ScrollHandle,

    /// Currently-previewed match (also the highlighted row).
    selected: Option<SharedString>,
    preview: Option<Preview>,
    preview_task: Option<Task<()>>,
    preview_scroll: ScrollHandle,
}

impl ZedisValueSearch {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = server_state.read(cx);
        let server_id = state.server_id().to_string();
        let db = state.db();
        let prefix_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n_value_search(cx, "prefix_placeholder")));
        let query_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n_value_search(cx, "query_placeholder")));
        Self {
            server_state,
            server_id,
            db,
            prefix_input,
            query_input,
            running: false,
            matches: Vec::new(),
            scanned: 0,
            skipped: 0,
            truncated: false,
            stop_reason: None,
            error: None,
            task: None,
            scroll: ScrollHandle::new(),
            selected: None,
            preview: None,
            preview_task: None,
            preview_scroll: ScrollHandle::new(),
        }
    }

    fn start_search(&mut self, cx: &mut Context<Self>) {
        let prefix = self.prefix_input.read(cx).value().trim().to_string();
        let query = self.query_input.read(cx).value().trim().to_string();
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
            prefix
        } else {
            format!("{prefix}*")
        };
        let needle = query.to_lowercase();

        self.matches.clear();
        self.scanned = 0;
        self.skipped = 0;
        self.truncated = false;
        self.stop_reason = None;
        self.error = None;
        self.selected = None;
        self.preview = None;
        self.preview_task = None;
        self.running = true;
        cx.notify();

        let server_id = self.server_id.clone();
        let db = self.db;
        self.task = Some(cx.spawn(async move |this, cx| {
            let client = match get_connection_manager().get_client(&server_id, db).await {
                Ok(c) => c,
                Err(e) => {
                    let _ = this.update(cx, |this, cx| this.finish_error(e.to_string().into(), cx));
                    return;
                }
            };
            let start = Instant::now();
            let mut cursors = None;
            loop {
                let round = match client
                    .scan_values_round(
                        &pattern,
                        &needle,
                        MAX_VALUE_BYTES,
                        MAX_CONTAINER_ELEMS,
                        cursors.clone(),
                        PAGE_COUNT,
                    )
                    .await
                {
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
                let stop = this.update(cx, |this, cx| {
                    this.scanned += scanned;
                    this.skipped += skipped_oversized;
                    for m in matches {
                        if this.matches.len() >= MAX_MATCHES {
                            this.truncated = true;
                            break;
                        }
                        this.matches.push(m);
                    }
                    cx.notify();
                    if done {
                        Some(StopReason::Done)
                    } else if this.scanned >= SCAN_CAP || this.truncated {
                        Some(StopReason::Capped)
                    } else if start.elapsed().as_secs() >= TIME_BUDGET_SECS {
                        Some(StopReason::Timeout)
                    } else {
                        None
                    }
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

    /// Preview a matched key's value inline (right pane), without leaving.
    fn select_result(&mut self, key: SharedString, cx: &mut Context<Self>) {
        self.selected = Some(key.clone());
        self.preview = Some(Preview::Loading);
        cx.notify();
        let server_id = self.server_id.clone();
        let db = self.db;
        self.preview_task = Some(cx.spawn(async move |this, cx| {
            let fetched = async {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                client.get_value_preview(&key).await
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                this.preview = Some(match fetched {
                    Ok(text) => Preview::Value(truncate_preview(text)),
                    Err(e) => Preview::Error(e.to_string().into()),
                });
                cx.notify();
            });
        }));
    }

    /// Optional jump from the preview: select the key and switch to the editor.
    fn open_key(&mut self, key: SharedString, cx: &mut Context<Self>) {
        self.server_state.update(cx, |state, cx| state.select_key(key, cx));
        cx.global::<ZedisGlobalStore>()
            .clone()
            .update(cx, |state, cx| state.go_to(Route::Editor, cx));
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

    fn summary_line(&self, cx: &Context<Self>) -> SharedString {
        let reason = match self.stop_reason {
            Some(StopReason::Done) => i18n_value_search(cx, "reason_done"),
            Some(StopReason::Capped) => i18n_value_search(cx, "reason_capped"),
            Some(StopReason::Timeout) => i18n_value_search(cx, "reason_timeout"),
            Some(StopReason::Cancelled) => i18n_value_search(cx, "reason_cancelled"),
            None => i18n_value_search(cx, "searching"),
        };
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        rust_i18n::t!(
            "value_search.summary",
            scanned = self.scanned,
            matched = self.matches.len(),
            skipped = self.skipped,
            reason = reason,
            locale = locale
        )
        .to_string()
        .into()
    }

    fn render_results(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        if self.matches.is_empty() {
            // Nothing typed/run yet → empty; running → searching; done → none.
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
        // Stronger than the usual `table_even` so the match list is easy to
        // scan row-by-row.
        let stripe = cx.theme().muted.opacity(0.5);
        let active = cx.theme().list_active;
        let selected = self.selected.clone();
        let mut list = v_flex().w_full();
        for (ix, vm) in self.matches.iter().enumerate() {
            let is_stripe = ix % 2 != 0;
            let is_selected = selected.as_ref() == Some(&vm.key);
            let key_click = vm.key.clone();
            // A muted second line names where the needle matched (field /
            // index / member); plain string values carry no location.
            let loc: Option<SharedString> = match &vm.location {
                MatchLocation::Value => None,
                MatchLocation::Field(f) => Some(format!("{}: {f}", i18n_value_search(cx, "loc_field")).into()),
                MatchLocation::Index(i) => Some(format!("[{i}]").into()),
                MatchLocation::Member(m) => Some(format!("{}: {m}", i18n_value_search(cx, "loc_member")).into()),
            };
            let mut content = v_flex()
                .min_w_0()
                .child(Label::new(vm.key.clone()).text_xs().truncate());
            if let Some(loc) = loc {
                content = content.child(Label::new(loc).text_xs().text_color(muted).truncate());
            }
            list = list.child(
                div()
                    .id(SharedString::from(format!("vs-row-{ix}")))
                    .w_full()
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .when(is_stripe && !is_selected, |this| this.bg(stripe))
                    .when(is_selected, |this| this.bg(active))
                    .on_click(cx.listener(move |this, _, _w, cx| this.select_result(key_click.clone(), cx)))
                    .child(content),
            );
        }
        if self.truncated {
            list = list.child(
                div().px_3().py_1().child(
                    Label::new(i18n_value_search(cx, "truncated"))
                        .text_xs()
                        .text_color(cx.theme().yellow),
                ),
            );
        }
        list.into_any_element()
    }

    fn render_preview(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        match &self.preview {
            None => div()
                .p_4()
                .child(
                    Label::new(i18n_value_search(cx, "preview_hint"))
                        .text_sm()
                        .text_color(muted),
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
                v_flex()
                    .w_full()
                    .gap_2()
                    .p_3()
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(Label::new(key).text_xs().font_semibold().truncate()),
                            )
                            .child(
                                Button::new("vs-open")
                                    .small()
                                    .ghost()
                                    .label(i18n_value_search(cx, "open"))
                                    .on_click(cx.listener(move |this, _, _w, cx| this.open_key(key_open.clone(), cx))),
                            ),
                    )
                    .child(div().w_full().text_xs().child(text.clone()))
                    .into_any_element()
            }
        }
    }
}

/// Truncate a type-aware preview string to [`PREVIEW_MAX_BYTES`] (char-safe).
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let action = if self.running {
            Button::new("vs-stop")
                .outline()
                .label(i18n_value_search(cx, "stop"))
                .on_click(cx.listener(|this, _, _w, cx| this.stop_search(cx)))
        } else {
            Button::new("vs-search")
                .primary()
                .label(i18n_value_search(cx, "search"))
                .on_click(cx.listener(|this, _, _w, cx| this.start_search(cx)))
        };

        let header = h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .child(div().w(px(260.)).child(Input::new(&self.prefix_input)))
            .child(div().flex_1().child(Input::new(&self.query_input)))
            .child(action)
            .when(!self.matches.is_empty(), |this| {
                this.child(
                    Button::new("vs-export-csv")
                        .outline()
                        .label(i18n_common(cx, "export_csv"))
                        .on_click(cx.listener(|this, _, _w, cx| this.export_csv(cx))),
                )
            });

        // Error takes precedence; otherwise show the sampling summary once a
        // search has started.
        let status = if let Some(err) = self.error.clone() {
            Some(Label::new(err).text_xs().text_color(cx.theme().danger))
        } else if self.running || self.stop_reason.is_some() {
            Some(
                Label::new(self.summary_line(cx))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground),
            )
        } else {
            None
        };

        let results = self.render_results(cx);
        let preview = self.render_preview(cx);
        let border = cx.theme().border;
        v_flex()
            .size_full()
            .font_family(get_mono_font_family())
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        Button::new("vs-back")
                            .ghost()
                            .small()
                            .icon(IconName::ArrowLeft)
                            .tooltip(i18n_common(cx, "back_to_editor"))
                            .on_click(|_, _w, cx| {
                                cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                    store.update(cx, |state, cx| state.go_to(Route::Editor, cx));
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
                    .when_some(status, |this, label| this.child(label))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .gap_2()
                            .child(
                                div()
                                    .id("vs-results")
                                    .w(px(320.))
                                    .h_full()
                                    .overflow_y_scroll()
                                    .track_scroll(&self.scroll)
                                    .child(results),
                            )
                            .child(
                                div()
                                    .id("vs-preview")
                                    .flex_1()
                                    .h_full()
                                    .min_w_0()
                                    .border_l_1()
                                    .border_color(border)
                                    .overflow_scroll()
                                    .track_scroll(&self.preview_scroll)
                                    .child(preview),
                            ),
                    ),
            )
    }
}
