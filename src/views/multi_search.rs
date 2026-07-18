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

use crate::components::KeyTypeBadge;
use crate::connection::{
    MultiSearchServerResult, get_server, get_server_groups, get_servers, multi_search_exact, multi_search_scan,
};
use crate::states::{KeyType, MultiSearchScope, ZedisGlobalStore, i18n_multi_search, update_app_state_and_save_quiet};
use gpui::{
    Context, Entity, FocusHandle, KeyDownEvent, SharedString, Subscription, Task, Window, div, prelude::*, px,
    uniform_list,
};
use gpui_component::{
    ActiveTheme, Disableable, IconName, Sizable, StyledExt,
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

/// A line in the virtualized result list: a section header ("Exact
/// matches" / "Scan matches") or a clickable hit. Headers share the
/// uniform row height, which is what lets grouping ride `uniform_list`.
#[derive(Clone)]
enum DisplayRow {
    Header(SharedString),
    Hit(HitRow),
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
    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        self.open = !self.open;
        if self.open {
            self.pending_focus = true;
            self.clear_results();
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
            // `save_open_tabs` on each tab change. Deduped: two tabs can't
            // normally share a connection, but stay safe if that changes.
            ScopeKind::OpenTabs => {
                let mut seen = HashSet::new();
                let mut targets: Vec<(String, usize)> = store
                    .open_tabs()
                    .iter()
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
                        let db = store.last_db_for(&s.id);
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
                    let db = store.last_db_for(&id);
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
    /// connect in place — no tab churn. `open_tabs()` (the server-bound tab
    /// list, synced on every tab change) stands in for strip visibility; a
    /// lone Home tab beside one connection counts as single here and simply
    /// degrades to the in-place connect. The already-active connection also
    /// reconnects in place: `connect_server` re-emits `ServerSelected`
    /// unconditionally, which is what consumes the pending key.
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
                    state.open_server_in_new_tab(server_id, db, cx);
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
        let scope_button = |id: &'static str, label: SharedString, kind: ScopeKind, current: ScopeKind| {
            let button = Button::new(id).small().label(label);
            if kind == current {
                button.primary()
            } else {
                button.ghost()
            }
        };
        let mut row = h_flex()
            .flex_wrap()
            .items_center()
            .gap_1p5()
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
                Some((select, _)) => row.child(div().w(px(200.)).child(select.clone())),
                None => row.child(
                    Label::new(i18n_multi_search(cx, "no_groups"))
                        .text_xs()
                        .text_color(muted),
                ),
            };
        }
        row.into_any_element()
    }

    fn render_server_picker(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let all_selected = !self.servers.is_empty() && self.selected_servers.len() == self.servers.len();
        let mut list = v_flex().gap_0p5().child(
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
        div()
            .id("multi-search-server-picker")
            .max_h(px(160.))
            .overflow_y_scroll()
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
                SharedString::default()
            };
            if message.is_empty() {
                return div().into_any_element();
            }
            return div()
                .p_3()
                .child(Label::new(message).text_sm().text_color(muted))
                .into_any_element();
        }

        let hover = cx.theme().list_active;
        // Group into sections: exact hits first under their own header,
        // then scan hits under theirs. `rows` is already ordered
        // exact-before-scan (pass 1 pushes before pass 2 appends), so a
        // stable partition by the flag is a plain split.
        let mut display: Vec<DisplayRow> = Vec::with_capacity(self.rows.len() + 2);
        for (want_exact, header_key) in [(true, "section_exact"), (false, "section_scan")] {
            let mut any = false;
            for row in self.rows.iter().filter(|r| r.exact == want_exact) {
                if !any {
                    display.push(DisplayRow::Header(i18n_multi_search(cx, header_key)));
                    any = true;
                }
                display.push(DisplayRow::Hit(row.clone()));
            }
        }
        // `uniform_list` scrolls internally and needs a definite height —
        // fit short lists to their content (rows × row height) and cap the
        // rest, so a couple of exact hits don't leave a 300px blank gap
        // above the "also scan" button (see the scroll gotcha in CLAUDE.md:
        // never `max_h` here).
        let list_height = px((display.len() as f32 * 36.).min(320.));
        let display: Arc<Vec<DisplayRow>> = Arc::new(display);
        let entity = cx.entity();
        uniform_list("multi-search-results", display.len(), move |range, _window, _cx| {
            let mut out = Vec::with_capacity(range.len());
            for ix in range {
                match &display[ix] {
                    DisplayRow::Header(label) => out.push(
                        h_flex()
                            .id(("multi-search-header", ix))
                            .w_full()
                            .h(px(36.))
                            .items_end()
                            .px_2()
                            .pb_1()
                            .child(Label::new(label.clone()).text_xs().font_semibold().text_color(muted)),
                    ),
                    DisplayRow::Hit(row) => {
                        let entity = entity.clone();
                        let row_for_click = row.clone();
                        let target: SharedString = format!("{} · db {}", row.server_name, row.db).into();
                        out.push(
                            h_flex()
                                .id(("multi-search-row", ix))
                                .w_full()
                                .h(px(36.))
                                .items_center()
                                .gap_2()
                                .px_2()
                                .rounded_md()
                                .cursor_pointer()
                                .hover(move |s| s.bg(hover))
                                .on_click(move |_, window, cx| {
                                    let row = row_for_click.clone();
                                    entity.update(cx, |this, cx| this.execute(&row, window, cx));
                                })
                                .child(KeyTypeBadge::new(KeyType::from(row.key_type.as_ref())).plain(true))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(Label::new(row.key.clone()).text_sm().truncate()),
                                )
                                .child(Label::new(target).text_xs().text_color(muted)),
                        );
                    }
                }
            }
            out
        })
        .h(list_height)
        .w_full()
        .into_any_element()
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
                state.set_value(SharedString::default(), window, cx);
                state.focus(window, cx);
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
        let radius_lg = theme.radius_lg;

        // Stays visible *through* the scan (loading spinner + disabled)
        // so clicking it doesn't just make it vanish with no feedback;
        // it only leaves once the scan finished.
        let show_scan_button = self.has_exact && !self.scan_ran;

        let panel = v_flex()
            .w(px(680.))
            .mt_20()
            .rounded(radius_lg)
            .border_1()
            .border_color(border)
            .bg(panel_bg)
            .shadow_lg()
            .p_3()
            .gap_2()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.query)))
                    .child(
                        Label::new(i18n_multi_search(cx, "scan_count"))
                            .text_xs()
                            .text_color(muted),
                    )
                    // Same control size as the query input beside it — a
                    // `.small()` box next to a regular one read as misaligned.
                    .child(div().w(px(112.)).child(Input::new(&self.scan_count_input))),
            )
            .child(self.render_scope_row(window, cx))
            .when(self.scope_kind == ScopeKind::Servers, |this| {
                this.child(self.render_server_picker(cx))
            })
            .child(self.render_results(cx))
            // "Also scan" sits *below* the exact matches (scan hasn't run
            // yet at this point, so every listed row is an exact hit).
            .when(show_scan_button, |this| {
                this.child(
                    Button::new("multi-search-run-scan")
                        .outline()
                        // The icon slot is required for feedback: Button's
                        // loading spinner renders by *replacing the icon*
                        // (`when_some(self.icon, …)` in gpui-component), so a
                        // label-only button would just grey out with no
                        // spinner while the scan runs.
                        .icon(IconName::Search)
                        .label(i18n_multi_search(cx, "load_scan"))
                        .loading(self.searching)
                        .disabled(self.searching)
                        .on_click(cx.listener(|this, _, _w, cx| this.run_scan(cx))),
                )
            })
            .when(self.truncated, |this| {
                this.child(
                    Label::new(i18n_multi_search(cx, "truncated"))
                        .text_xs()
                        .text_color(cx.theme().warning),
                )
            })
            .children(
                self.errors
                    .iter()
                    .map(|e| Label::new(e.clone()).text_xs().text_color(cx.theme().danger)),
            );

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .justify_center()
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
