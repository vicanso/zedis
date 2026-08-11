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

//! ⌘⇧F multi-database search palette.
//!
//! Searches a key name across a selected set of connections. The exact
//! (`TYPE`-probe) pass runs first and its hits render immediately; when it
//! finds nothing the capped `SCAN` pass runs automatically, and when it
//! *does* find hits the SCAN pass waits behind an explicit button so the
//! cheap answer is never delayed by the expensive one.
//!
//! Scope options (persisted via [`MultiSearchScope`]): the active tab's
//! connection, one server group, or an explicit checkbox set of servers
//! (ids only, so renames don't break it). The per-server SCAN cap is
//! persisted too. Clicking a hit connects to that server/db and selects
//! the key through the pending-key handoff consumed in `content.rs`.
//!
//! Query + results survive close/reopen (in memory, per session): after
//! jumping to one hit the palette reopens on the same list so the user can
//! pick a different result without re-searching.

use crate::assets::CustomIconName;
use crate::components::KeyTypeBadge;
use crate::connection::{
    MultiSearchServerResult, get_server, get_server_groups, get_servers, multi_search_exact, multi_search_scan,
};
use crate::helpers::build_csv;
use crate::states::{
    KeyType, MultiSearchScope, ZedisGlobalStore, i18n_common, i18n_multi_search, update_app_state_and_save_quiet,
};
use crate::views::export_to_file_global;
use gpui::{
    ClipboardItem, Context, Entity, FocusHandle, Hsla, KeyDownEvent, SharedString, Subscription, Task, Window, div,
    prelude::*, px, uniform_list,
};
use gpui_component::{
    ActiveTheme, Disableable, IconName, Sizable, StyledExt, WindowExt,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    v_flex,
};
use std::collections::HashSet;
use std::sync::Arc;
use zedis_ui::{ZedisSelect, ZedisSelectEvent};

/// UI mirror of [`MultiSearchScope`]'s discriminant.
#[derive(Clone, Copy, PartialEq)]
enum ScopeKind {
    OpenTabs,
    Group,
    Servers,
}

/// One result row (exact or scan hit) with everything the jump needs.
#[derive(Clone)]
struct HitRow {
    server_id: SharedString,
    server_name: SharedString,
    db: usize,
    key: SharedString,
    key_type: SharedString,
    /// Exact (`TYPE` probe) hit vs a SCAN match — drives the row tag.
    exact: bool,
}

pub struct ZedisMultiSearch {
    open: bool,
    query: Entity<InputState>,
    scan_count_input: Entity<InputState>,
    scope_kind: ScopeKind,
    /// Group names snapshot taken on open (order = dropdown order).
    groups: Vec<String>,
    selected_group: Option<String>,
    /// Dropdown for the Group scope; built lazily (needs a `Window`).
    group_select: Option<(Entity<ZedisSelect>, Subscription)>,
    /// `(id, name)` snapshot of all configured servers, taken on open.
    servers: Vec<(SharedString, SharedString)>,
    selected_servers: HashSet<SharedString>,
    searching: bool,
    rows: Vec<HitRow>,
    errors: Vec<SharedString>,
    /// The exact pass found at least one hit → SCAN waits behind a button.
    has_exact: bool,
    /// The SCAN pass ran (auto or via button) for the current query.
    scan_ran: bool,
    truncated: bool,
    /// Bumped per search so a stale task's results are dropped.
    generation: u64,
    task: Option<Task<()>>,
    focus_handle: FocusHandle,
    pending_focus: bool,
    _subscriptions: Vec<Subscription>,
}

impl ZedisMultiSearch {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(i18n_multi_search(cx, "placeholder")));
        let scan_count_input = cx.new(|cx| InputState::new(window, cx));
        let mut subscriptions = Vec::new();
        // Enter in the query starts (or restarts) the search.
        subscriptions.push(cx.subscribe(&query, |this, _state, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.start_search(cx);
            }
        }));
        // Persist the scan cap as it's edited (quietly — no visual effect).
        subscriptions.push(cx.subscribe(&scan_count_input, |_this, state, event, cx| {
            if matches!(event, InputEvent::Change) {
                let count = state.read(cx).value().trim().parse::<usize>().unwrap_or(0);
                update_app_state_and_save_quiet(cx, "save_multi_search_scan_count", move |state, _| {
                    state.set_multi_search_scan_count(count);
                });
            }
        }));
        Self {
            open: false,
            query,
            scan_count_input,
            scope_kind: ScopeKind::OpenTabs,
            groups: Vec::new(),
            selected_group: None,
            group_select: None,
            servers: Vec::new(),
            selected_servers: HashSet::new(),
            searching: false,
            rows: Vec::new(),
            errors: Vec::new(),
            has_exact: false,
            scan_ran: false,
            truncated: false,
            generation: 0,
            task: None,
            focus_handle: cx.focus_handle(),
            pending_focus: false,
            _subscriptions: subscriptions,
        }
    }

    /// Open (or close). Loads the persisted scope + scan cap and snapshots
    /// the server/group lists; input focus is deferred to `render` (the
    /// global action handler has no `Window`).
    ///
    /// The previous query and results are deliberately **kept**: jumping to
    /// a hit closes the palette, and reopening it should show the same list
    /// so the user can pick a different result without re-searching. A new
    /// search (Enter) still clears everything via `start_search`.
    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        self.open = !self.open;
        if self.open {
            self.pending_focus = true;
            self.servers = get_servers()
                .unwrap_or_default()
                .into_iter()
                .map(|s| (SharedString::from(s.id), SharedString::from(s.name)))
                .collect();
            self.groups = get_server_groups();
            match cx.global::<ZedisGlobalStore>().read(cx).multi_search_scope() {
                MultiSearchScope::OpenTabs => self.scope_kind = ScopeKind::OpenTabs,
                MultiSearchScope::Group(name) => {
                    self.scope_kind = ScopeKind::Group;
                    self.selected_group = Some(name);
                }
                MultiSearchScope::Servers(ids) => {
                    self.scope_kind = ScopeKind::Servers;
                    self.selected_servers = ids.into_iter().map(SharedString::from).collect();
                }
            }
            // Rebuilt in `render` (dropdown creation needs a Window).
            self.group_select = None;
        }
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = false;
        // Return focus to the window root so global hotkeys keep a
        // dispatch path (same as the command palette).
        window.blur();
        cx.notify();
    }

    fn clear_results(&mut self) {
        self.rows.clear();
        self.errors.clear();
        self.has_exact = false;
        self.scan_ran = false;
        self.truncated = false;
        self.searching = false;
    }

    /// Persist the current UI scope (ids only for the explicit set).
    fn persist_scope(&self, cx: &mut Context<Self>) {
        let scope = match self.scope_kind {
            ScopeKind::OpenTabs => MultiSearchScope::OpenTabs,
            ScopeKind::Group => MultiSearchScope::Group(self.selected_group.clone().unwrap_or_default()),
            ScopeKind::Servers => {
                MultiSearchScope::Servers(self.selected_servers.iter().map(|s| s.to_string()).collect())
            }
        };
        update_app_state_and_save_quiet(cx, "save_multi_search_scope", move |state, _| {
            state.set_multi_search_scope(scope.clone());
        });
    }

    /// Resolve the current scope into concrete `(server_id, db)` targets.
    /// Non-open servers use their remembered last db (0 if never opened).
    fn targets(&self, cx: &Context<Self>) -> Vec<(String, usize)> {
        let store = cx.global::<ZedisGlobalStore>().read(cx);
        match self.scope_kind {
            // Every open workspace tab's `(server, db)` — kept in sync by
            // `save_open_tabs` on each tab change. Home tabs (empty id) are
            // not connections, so they're skipped. Deduped: two tabs can't
            // normally share a connection, but stay safe if that changes.
            ScopeKind::OpenTabs => {
                let mut seen = HashSet::new();
                let mut targets: Vec<(String, usize)> = store
                    .open_tabs()
                    .iter()
                    .filter(|(id, _)| !id.is_empty())
                    .filter(|target| seen.insert((*target).clone()))
                    .cloned()
                    .collect();
                // Fresh session parked on Home: no tab has persisted a
                // connection yet, but the remembered selection may still
                // point somewhere — search that instead of reporting
                // "no connections in scope".
                if targets.is_empty()
                    && let Some(target) = store.selected_server().cloned()
                {
                    targets.push(target);
                }
                targets
            }
            ScopeKind::Group => {
                let Some(group) = self.selected_group.as_deref() else {
                    return Vec::new();
                };
                get_servers()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|s| s.group.as_deref().map(str::trim) == Some(group))
                    .map(|s| {
                        let db = store.open_db_for(&s.id);
                        (s.id, db)
                    })
                    .collect()
            }
            ScopeKind::Servers => self
                .servers
                .iter()
                .filter(|(id, _)| self.selected_servers.contains(id))
                .map(|(id, _)| {
                    let id = id.to_string();
                    let db = store.open_db_for(&id);
                    (id, db)
                })
                .collect(),
        }
    }

    fn start_search(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value().trim().to_string();
        if query.is_empty() {
            return;
        }
        self.clear_results();
        self.generation += 1;
        let generation = self.generation;
        let targets = self.targets(cx);
        if targets.is_empty() {
            self.errors.push(i18n_multi_search(cx, "no_targets"));
            cx.notify();
            return;
        }
        self.searching = true;
        cx.notify();
        let scan_cap = cx.global::<ZedisGlobalStore>().read(cx).multi_search_scan_count();
        self.task = Some(cx.spawn(async move |this, cx| {
            // Pass 1 — exact TYPE probes; cheap, so it always runs first.
            let exact = multi_search_exact(targets.clone(), query.clone()).await;
            let auto_scan = this
                .update(cx, |this, cx| {
                    if this.generation != generation {
                        return false;
                    }
                    this.apply_results(exact, true, cx);
                    // No exact hit anywhere → fall through to SCAN without
                    // asking; otherwise the button offers it.
                    !this.has_exact
                })
                .unwrap_or(false);
            if !auto_scan {
                let _ = this.update(cx, |this, cx| {
                    if this.generation == generation {
                        this.searching = false;
                        cx.notify();
                    }
                });
                return;
            }
            // Pass 2 — capped SCAN across the same targets.
            let scan = multi_search_scan(targets, query, scan_cap).await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.apply_results(scan, false, cx);
                this.scan_ran = true;
                this.searching = false;
                cx.notify();
            });
        }));
    }

    /// Run the SCAN pass on demand (the "also scan" button after exact hits).
    fn run_scan(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value().trim().to_string();
        if query.is_empty() || self.searching {
            return;
        }
        let generation = self.generation;
        let targets = self.targets(cx);
        let scan_cap = cx.global::<ZedisGlobalStore>().read(cx).multi_search_scan_count();
        self.searching = true;
        cx.notify();
        self.task = Some(cx.spawn(async move |this, cx| {
            let scan = multi_search_scan(targets, query, scan_cap).await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.apply_results(scan, false, cx);
                this.scan_ran = true;
                this.searching = false;
                cx.notify();
            });
        }));
    }

    /// Fold one pass's per-server results into the row list. Rows dedupe
    /// against already-present `(server, db, key)` — both across passes
    /// (the exact hit repeats once the pattern also matches it) and
    /// *within* one pass: SCAN's contract allows the same key to be
    /// returned on multiple pages, so `seen` must grow as rows are pushed.
    fn apply_results(&mut self, results: Vec<MultiSearchServerResult>, exact: bool, cx: &mut Context<Self>) {
        let mut seen: HashSet<(SharedString, usize, SharedString)> = self
            .rows
            .iter()
            .map(|r| (r.server_id.clone(), r.db, r.key.clone()))
            .collect();
        for result in results {
            let server_id = SharedString::from(result.server_id.clone());
            let server_name: SharedString = get_server(&result.server_id)
                .map(|s| s.name.into())
                .unwrap_or_else(|_| server_id.clone());
            if let Some(error) = result.error {
                self.errors.push(format!("{server_name}: {error}").into());
            }
            self.truncated |= result.truncated;
            for hit in result.hits {
                let key = SharedString::from(hit.key);
                if !seen.insert((server_id.clone(), result.db, key.clone())) {
                    continue;
                }
                self.rows.push(HitRow {
                    server_id: server_id.clone(),
                    server_name: server_name.clone(),
                    db: result.db,
                    key,
                    key_type: hit.key_type.into(),
                    exact,
                });
            }
        }
        self.has_exact |= exact && self.rows.iter().any(|r| r.exact);
        cx.notify();
    }

    /// Jump to a hit: arm the pending-key handoff, route to the right tab,
    /// close.
    ///
    /// Tab routing: with multiple tabs open, reuse the tab already bound to
    /// this `(server, db)` or append a new one at the end (the root's
    /// `ServerOpenInNewTab` handler does exactly that); with a single tab,
    /// connect in place — no tab churn. `open_tabs()` (the persisted tab
    /// list, Home tabs included, synced on every tab change) stands in for
    /// strip visibility. The already-active connection also
    /// reconnects in place: `connect_server` re-emits `ServerSelected`
    /// unconditionally, which is what consumes the pending key.
    /// Export every listed hit to CSV. Columns lead with the target
    /// (`server`, `db`) because "which instance was it on?" is the whole
    /// point of a cross-database search.
    fn export_csv(&mut self, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let rows: Vec<Vec<String>> = self
            .rows
            .iter()
            .map(|r| {
                vec![
                    r.server_name.to_string(),
                    r.db.to_string(),
                    r.key.to_string(),
                    r.key_type.to_string(),
                ]
            })
            .collect();
        let csv = build_csv(&["server", "db", "key", "type"], &rows);
        let success = i18n_common(cx, "csv_exported");
        let error = i18n_common(cx, "csv_export_failed");
        export_to_file_global(cx, csv.into_bytes(), "multi-search.csv", success, error);
    }

    fn execute(&mut self, row: &HitRow, window: &mut Window, cx: &mut Context<Self>) {
        let server_id = row.server_id.to_string();
        let db = row.db;
        let key = row.key.to_string();
        cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
            store.update(cx, |state, cx| {
                state.set_multi_search_pending_key(server_id.clone(), db, key);
                let same_as_active = state
                    .selected_server()
                    .is_some_and(|(id, active_db)| id == &server_id && *active_db == db);
                let multi_tab = state.open_tabs().len() > 1;
                if multi_tab && !same_as_active {
                    state.reveal_or_open_server_tab(server_id, db, cx);
                } else {
                    state.connect_server(server_id, db, cx);
                }
            });
        });
        self.close(window, cx);
    }

    fn set_scope_kind(&mut self, kind: ScopeKind, window: &mut Window, cx: &mut Context<Self>) {
        self.scope_kind = kind;
        if kind == ScopeKind::Group {
            self.ensure_group_select(window, cx);
        }
        self.persist_scope(cx);
        cx.notify();
    }

    /// Build the group dropdown (idempotent). Selection defaults to the
    /// persisted group when it still exists, else the first group.
    fn ensure_group_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.group_select.is_some() || self.groups.is_empty() {
            return;
        }
        let selected = self
            .selected_group
            .as_deref()
            .and_then(|g| self.groups.iter().position(|name| name == g))
            .unwrap_or(0);
        self.selected_group = self.groups.get(selected).cloned();
        let select = cx.new(|cx| ZedisSelect::new(self.groups.clone(), Some(selected), window, cx));
        let groups = self.groups.clone();
        let subscription = cx.subscribe(&select, move |this, _sel, event: &ZedisSelectEvent, cx| {
            let ZedisSelectEvent::Change(index) = event;
            if let Some(group) = groups.get(*index) {
                this.selected_group = Some(group.clone());
                this.persist_scope(cx);
                cx.notify();
            }
        });
        self.group_select = Some((select, subscription));
    }

    fn render_scope_row(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let muted_bg = cx.theme().muted.opacity(0.35);
        let scope_button = |id: &'static str, label: SharedString, kind: ScopeKind, current: ScopeKind| {
            let button = Button::new(id).small().label(label);
            if kind == current {
                button.primary()
            } else {
                button.ghost()
            }
        };
        // Segmented control on a subtle rail so the three modes read as one
        // control, not three loose buttons floating under the search field.
        let mut row = h_flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .p_0p5()
            .rounded_lg()
            .bg(muted_bg)
            .child(
                scope_button(
                    "multi-search-scope-open-tabs",
                    i18n_multi_search(cx, "scope_open_tabs"),
                    ScopeKind::OpenTabs,
                    self.scope_kind,
                )
                .on_click(cx.listener(|this, _, window, cx| this.set_scope_kind(ScopeKind::OpenTabs, window, cx))),
            )
            .child(
                scope_button(
                    "multi-search-scope-group",
                    i18n_multi_search(cx, "scope_group"),
                    ScopeKind::Group,
                    self.scope_kind,
                )
                .on_click(cx.listener(|this, _, window, cx| this.set_scope_kind(ScopeKind::Group, window, cx))),
            )
            .child(
                scope_button(
                    "multi-search-scope-servers",
                    i18n_multi_search(cx, "scope_servers"),
                    ScopeKind::Servers,
                    self.scope_kind,
                )
                .on_click(cx.listener(|this, _, window, cx| this.set_scope_kind(ScopeKind::Servers, window, cx))),
            );

        if self.scope_kind == ScopeKind::Group {
            self.ensure_group_select(window, cx);
            row = match &self.group_select {
                Some((select, _)) => row.child(div().w(px(180.)).ml_1().child(select.clone())),
                None => row.child(
                    Label::new(i18n_multi_search(cx, "no_groups"))
                        .text_xs()
                        .text_color(muted)
                        .ml_1(),
                ),
            };
        }
        row.into_any_element()
    }

    fn render_server_picker(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let all_selected = !self.servers.is_empty() && self.selected_servers.len() == self.servers.len();
        // Wider gap than the previous `gap_0p5` — dense checkboxes felt glued
        // together; `gap_2` matches the panel's vertical rhythm.
        let mut list = v_flex().gap_2().child(
            Checkbox::new("multi-search-select-all")
                .checked(all_selected)
                .label(i18n_multi_search(cx, "select_all"))
                .on_click(cx.listener(|this, checked: &bool, _w, cx| {
                    this.selected_servers = if *checked {
                        this.servers.iter().map(|(id, _)| id.clone()).collect()
                    } else {
                        HashSet::new()
                    };
                    this.persist_scope(cx);
                    cx.notify();
                })),
        );
        for (index, (id, name)) in self.servers.iter().enumerate() {
            let id_for_click = id.clone();
            list = list.child(
                Checkbox::new(("multi-search-server", index))
                    .checked(self.selected_servers.contains(id))
                    .label(name.clone())
                    .on_click(cx.listener(move |this, checked: &bool, _w, cx| {
                        if *checked {
                            this.selected_servers.insert(id_for_click.clone());
                        } else {
                            this.selected_servers.remove(&id_for_click);
                        }
                        this.persist_scope(cx);
                        cx.notify();
                    })),
            );
        }
        // Definite height (not `max_h`) so the picker scrolls — same scroll
        // gotcha as elsewhere: `max_h` + overflow can clip without a range.
        let picker_h = px(((self.servers.len() + 1) as f32 * 28.).min(180.));
        div()
            .id("multi-search-server-picker")
            .h(picker_h)
            .overflow_y_scroll()
            .child(list)
            .into_any_element()
    }

    /// One section (exact or scan): a compact natural-height header, then a
    /// `uniform_list` of hits only. Headers used to share the list's row
    /// height (`items_end` in a 44px slot), which left a dead band between
    /// the scope tabs and "Exact matches".
    fn render_hit_section(
        &self,
        section_id: &'static str,
        title: SharedString,
        hits: Vec<HitRow>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let hover = cx.theme().list_active;
        // Same recipe as the key tree's leaf zebra (`key_tree/delegate.rs`):
        // faint white-on-dark / black-on-light on odd indices so dense
        // result lists stay scannable. Dark needs a higher alpha.
        let is_dark = cx.theme().is_dark();
        let stripe_bg = if is_dark {
            Hsla::white().alpha(0.06)
        } else {
            Hsla::black().alpha(0.03)
        };
        // Two-line hit rows; definite `h` only (never `max_h` — CLAUDE.md).
        const ROW_H: f32 = 40.;
        let list_height = px((hits.len() as f32 * ROW_H).min(ROW_H * 8.));
        let hits: Arc<Vec<HitRow>> = Arc::new(hits);
        let entity = cx.entity();
        // Bake the section into the ElementId strings — `(&str, &str)` is not
        // a valid id tuple, and exact/scan lists must not share the same id.
        let list_id: SharedString = format!("multi-search-{section_id}").into();
        let row_prefix: SharedString = format!("multi-search-row-{section_id}").into();
        let list = uniform_list(list_id, hits.len(), move |range, _window, _cx| {
            let mut out = Vec::with_capacity(range.len());
            for ix in range {
                let row = &hits[ix];
                let entity = entity.clone();
                let row_for_click = row.clone();
                let target: SharedString = format!("{} · db {}", row.server_name, row.db).into();
                let row_id: SharedString = format!("{row_prefix}-{ix}").into();
                let copy_group: SharedString = format!("{row_prefix}-copy-{ix}").into();
                let copy_key = row.key.clone();
                // 2nd, 4th, … within this section (same leaf banding as key tree).
                let is_stripe = ix % 2 == 1;
                out.push(
                    h_flex()
                        .id(row_id)
                        .w_full()
                        .h(px(ROW_H))
                        .items_center()
                        .gap_2()
                        .px_1()
                        .rounded_md()
                        .cursor_pointer()
                        .group(copy_group.clone())
                        .when(is_stripe, |this| this.bg(stripe_bg))
                        .hover(move |s| s.bg(hover))
                        .on_click(move |_, window, cx| {
                            let row = row_for_click.clone();
                            entity.update(cx, |this, cx| this.execute(&row, window, cx));
                        })
                        .child(KeyTypeBadge::new(KeyType::from(row.key_type.as_ref())).plain(true))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_0()
                                .child(Label::new(row.key.clone()).text_sm().truncate())
                                .child(Label::new(target).text_xs().text_color(muted).truncate()),
                        )
                        // Copy the key without jumping: the wrapper swallows
                        // the click so the row's navigate handler stays put.
                        .child(
                            div()
                                .id((copy_group.clone(), 0_usize))
                                .invisible()
                                .group_hover(copy_group.clone(), |style| style.visible())
                                .flex_none()
                                .on_click(|_, _, cx: &mut gpui::App| cx.stop_propagation())
                                .child(
                                    Button::new((copy_group, 1_usize))
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::Copy)
                                        .on_click(move |_, window, cx: &mut gpui::App| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(copy_key.to_string()));
                                            window.push_notification(
                                                gpui_component::notification::Notification::info(i18n_common(
                                                    cx,
                                                    "copied_to_clipboard",
                                                )),
                                                cx,
                                            );
                                        }),
                                ),
                        ),
                );
            }
            out
        })
        .h(list_height)
        .w_full();
        v_flex()
            .w_full()
            .gap_0p5()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .px_1()
                    .pb_2()
                    .border_b_1()
                    .border_color(border)
                    .child(Label::new(title).text_xs().font_semibold().text_color(muted)),
            )
            .child(list)
            .into_any_element()
    }

    fn render_results(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        if self.rows.is_empty() {
            let message = if self.searching {
                i18n_multi_search(cx, "searching")
            } else if self.scan_ran {
                i18n_multi_search(cx, "no_results")
            } else {
                // Idle: short how-to so the panel doesn't look like a blank box.
                i18n_multi_search(cx, "hint_idle")
            };
            return div()
                .w_full()
                .px_1()
                .py_2()
                .child(Label::new(message).text_sm().text_color(muted))
                .into_any_element();
        }

        // Exact first, then scan — each section is header + list so the
        // header stays content-height (no padded uniform-list slot).
        let mut body = v_flex().w_full().gap_2();
        for (want_exact, header_key, section_id) in [(true, "section_exact", "exact"), (false, "section_scan", "scan")]
        {
            let hits: Vec<HitRow> = self.rows.iter().filter(|r| r.exact == want_exact).cloned().collect();
            if hits.is_empty() {
                continue;
            }
            let title: SharedString = format!("{} · {}", i18n_multi_search(cx, header_key), hits.len()).into();
            body = body.child(self.render_hit_section(section_id, title, hits, cx));
        }
        body.into_any_element()
    }
}

impl Render for ZedisMultiSearch {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().into_any_element();
        }

        if self.pending_focus {
            self.pending_focus = false;
            self.query.update(cx, |state, cx| {
                // Keep the previous query (its results are still listed
                // below) but select it whole, so typing immediately starts
                // a fresh search while Enter re-runs the old one.
                let len = state.value().len();
                state.focus(window, cx);
                state.set_selected_range(0..len, cx);
            });
            let count = cx.global::<ZedisGlobalStore>().read(cx).multi_search_scan_count();
            self.scan_count_input.update(cx, |state, cx| {
                state.set_value(count.to_string(), window, cx);
            });
        }

        let theme = cx.theme();
        let panel_bg = theme.background;
        let border = theme.border;
        let muted = theme.muted_foreground;
        let warning = theme.warning;
        let danger = theme.danger;
        let radius_lg = theme.radius_lg;
        let radius = theme.radius;

        // Stays visible *through* the scan (loading spinner + disabled)
        // so clicking it doesn't just make it vanish with no feedback;
        // it only leaves once the scan finished.
        let show_scan_button = self.has_exact && !self.scan_ran;
        let has_footer_notes = self.truncated || !self.errors.is_empty();

        let panel = v_flex()
            // Slightly narrower than before: the old 680px row left a dead
            // middle between key and server; denser rows + 560px keep the
            // eye on the list.
            .w(px(560.))
            .mt_16()
            .rounded(radius_lg)
            .border_1()
            .border_color(border)
            .bg(panel_bg)
            .shadow_lg()
            .p_3()
            .gap_2()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            // Title row — names the palette and surfaces Esc (the dim
            // backdrop also closes, but a label helps discoverability).
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(Label::new(i18n_multi_search(cx, "title")).text_sm().font_semibold())
                    .child(Label::new("Esc").text_xs().text_color(muted)),
            )
            // Query is the only full-width primary control. Scan limit
            // used to sit beside it and stole visual weight from typing.
            .child(div().w_full().child(Input::new(&self.query)))
            .child(self.render_scope_row(window, cx))
            .when(self.scope_kind == ScopeKind::Servers, |this| {
                this.child(self.render_server_picker(cx))
            })
            .child(self.render_results(cx))
            .when(has_footer_notes, |this| {
                this.child(
                    v_flex()
                        .w_full()
                        .gap_1()
                        .when(self.truncated, |this| {
                            this.child(
                                div()
                                    .w_full()
                                    .px_2()
                                    .py_1()
                                    .rounded(radius)
                                    .border_1()
                                    .border_color(warning.opacity(0.45))
                                    .bg(warning.opacity(0.1))
                                    .child(
                                        Label::new(i18n_multi_search(cx, "truncated"))
                                            .text_xs()
                                            .text_color(warning),
                                    ),
                            )
                        })
                        .children(self.errors.iter().map(|e| {
                            div()
                                .w_full()
                                .px_2()
                                .py_1()
                                .rounded(radius)
                                .border_1()
                                .border_color(danger.opacity(0.4))
                                .bg(danger.opacity(0.08))
                                .child(Label::new(e.clone()).text_xs().text_color(danger))
                        })),
                )
            })
            // Footer: scan limit (left) + "Also scan" or hint (right).
            // Keeping the scan action on the same row as its limit avoids a
            // full-width secondary button between the list and the settings.
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .pt_3()
                    .border_t_1()
                    .border_color(border)
                    .child(
                        Label::new(i18n_multi_search(cx, "scan_count"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(div().w(px(72.)).child(Input::new(&self.scan_count_input).small()))
                    .child(div().flex_1())
                    .when(!self.rows.is_empty(), |this| {
                        this.child(
                            Button::new("multi-search-export-csv")
                                .small()
                                .ghost()
                                .icon(CustomIconName::Download)
                                .label(i18n_common(cx, "export_csv"))
                                .on_click(cx.listener(|this, _, _w, cx| this.export_csv(cx))),
                        )
                    })
                    .when(show_scan_button, |this| {
                        this.child(
                            Button::new("multi-search-run-scan")
                                .small()
                                .outline()
                                // Icon required: Button's loading spinner
                                // replaces the icon slot, not the label.
                                .icon(IconName::Search)
                                .label(i18n_multi_search(cx, "load_scan"))
                                .loading(self.searching)
                                .disabled(self.searching)
                                .on_click(cx.listener(|this, _, _w, cx| this.run_scan(cx))),
                        )
                    })
                    .when(!show_scan_button, |this| {
                        this.child(
                            Label::new(i18n_multi_search(cx, "hint_footer"))
                                .text_xs()
                                .text_color(muted),
                        )
                    }),
            );

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .justify_center()
            // Top-align so the panel stays content-sized (default stretch
            // would pull it to half the window — same gotcha as the ⌘K palette).
            .items_start()
            .bg(gpui::hsla(0., 0., 0., 0.4))
            .track_focus(&self.focus_handle)
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, cx| this.close(window, cx)),
            )
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key.as_str() == "escape" {
                    this.close(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(panel)
            .into_any_element()
    }
}
