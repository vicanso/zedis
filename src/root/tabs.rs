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

//! The workspace tab strip: one tab per connection, opened, activated,
//! moved, closed and remembered for the next launch, and the strip that draws
//! them as pills.

use super::*;

/// Upper bound on workspace tabs — each tab holds its own `ZedisServerState`
/// (heartbeat, pooled connections, loaded keys), so the cap keeps a runaway
/// tab strip from piling up background Redis traffic.
pub(crate) const MAX_TABS: usize = 8;

/// One workspace tab: a content column bound to a connection. `server_id`
/// stays empty until a server is selected in this tab.
pub(crate) struct ContentTab {
    pub(super) server_id: String,
    pub(super) db: usize,
    pub(super) content: Entity<ZedisContent>,
    /// The tab's server state, watched for its link health: the sidebar's
    /// dots follow every tab's heartbeat, not only the active one's.
    _health: Subscription,
}

impl ContentTab {
    pub(super) fn new(server_id: String, db: usize, content: Entity<ZedisContent>, cx: &mut Context<Zedis>) -> Self {
        let server_state = content.read(cx).server_state();
        let _health = cx.subscribe(&server_state, |this, _server_state, event, cx| {
            if matches!(event, ServerEvent::ConnectionHealthChanged) {
                this.sync_sidebar_health(cx);
            }
        });
        Self {
            server_id,
            db,
            content,
            _health,
        }
    }
}

/// Context-menu actions on a workspace tab (dispatched by the tab strip's
/// right-click menu, handled on the `Zedis` root).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Action)]
pub(crate) enum TabAction {
    Close(usize),
    CloseOthers(usize),
    CloseRight(usize),
}

/// Drag payload for reordering workspace tabs.
pub(crate) struct DraggedTab {
    from: usize,
}

/// Floating preview shown while a tab is dragged.
pub(crate) struct TabDragPreview {
    title: SharedString,
}

impl Render for TabDragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(cx.theme().background)
            .border_1()
            .border_color(cx.theme().border)
            .child(Label::new(self.title.clone()).text_sm())
    }
}

/// Where the active index lands after the tab at `from` is moved to `to`
/// (`Vec::remove` + `insert` semantics): the active tab follows itself, and a
/// tab crossing over it pushes the index one step the other way. Pure so the
/// arithmetic is unit-tested (see `tests` below).
pub(crate) fn moved_active_index(active: usize, from: usize, to: usize) -> usize {
    if active == from {
        to
    } else if from < active && to >= active {
        active - 1
    } else if from > active && to <= active {
        active + 1
    } else {
        active
    }
}

/// The strip once some of the connections its tabs were on are gone: the
/// tabs that stay (by index, in order) and which of them is active. A tab
/// on a connection that `exists` no more is dropped; a Home tab (no
/// connection) always stays. The active tab stays active if it stays, else
/// the nearest one left of it takes over, else the first. When none stays,
/// the active one is kept and `home` is set: the strip always has a tab, so
/// that one goes Home rather than going away.
struct SurvivingTabs {
    keep: Vec<usize>,
    active: usize,
    home: bool,
}

fn tabs_surviving_removal(ids: &[&str], active: usize, exists: impl Fn(&str) -> bool) -> SurvivingTabs {
    let keep: Vec<usize> = ids
        .iter()
        .enumerate()
        .filter(|(_, id)| id.is_empty() || exists(id))
        .map(|(ix, _)| ix)
        .collect();
    if keep.is_empty() {
        return SurvivingTabs {
            keep: vec![active],
            active: 0,
            home: true,
        };
    }
    let active = keep
        .iter()
        .position(|&ix| ix == active)
        .or_else(|| keep.iter().rposition(|&ix| ix < active))
        .unwrap_or(0);
    SurvivingTabs {
        keep,
        active,
        home: false,
    }
}

impl Zedis {
    /// Switch the active tab: the outgoing tab stops reacting to global
    /// route/server broadcasts, the incoming one resumes, and the palettes
    /// are rebound to the incoming tab's server state. No-op when `ix` is
    /// already active (or out of range).
    ///
    /// When `window` is provided, defers a focus reclaim onto the incoming
    /// content: a mouse click on the tab pill leaves focus on the strip (a
    /// sibling of the page), so content-local keybinding handlers never see
    /// actions until the user clicks the page. ⌘F is also handled at the
    /// window root as a second line of defense.
    pub(super) fn activate_tab(&mut self, ix: usize, window: Option<&mut Window>, cx: &mut Context<Self>) {
        if ix == self.active_tab || ix >= self.tabs.len() {
            return;
        }
        self.tabs[self.active_tab]
            .content
            .update(cx, |content, cx| content.set_active(false, cx));
        self.active_tab = ix;
        self.tabs[ix]
            .content
            .update(cx, |content, cx| content.set_active(true, cx));
        // ⌘1–8 can pick a pill scrolled out of the strip.
        self.tab_scroll.scroll_to_item(ix);
        self.rebind_palettes(cx);
        if let Some(window) = window {
            // After the click/key event settles (focus may still be on the
            // tab pill), move it onto the page root so Esc/⌘F have a path.
            cx.defer_in(window, |this, window, cx| {
                this.active_content().update(cx, |content, cx| {
                    content.reclaim_focus(window, cx);
                });
            });
        }
        cx.notify();
    }

    /// Close a tab, dropping its content entity (which tears down the tab's
    /// subscriptions, server state and connections). The last tab can't be
    /// closed; closing the active tab activates its left neighbor (or the
    /// new last tab) and projects that tab's connection.
    pub(super) fn close_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.tabs.len() <= 1 || ix >= self.tabs.len() {
            return;
        }
        let was_active = ix == self.active_tab;
        self.tabs.remove(ix);
        if self.active_tab > ix {
            self.active_tab -= 1;
        } else if was_active {
            self.active_tab = ix.min(self.tabs.len() - 1);
            self.tabs[self.active_tab]
                .content
                .update(cx, |content, cx| content.set_active(true, cx));
            self.rebind_palettes(cx);
            self.project_active_tab(cx);
        }
        self.persist_tabs(cx);
        cx.notify();
    }

    /// Close the tabs of connections that are no longer in the list (#209).
    /// A deleted entry's tab used to stay, titled with its raw id over an
    /// empty tree with the status bar still saying "Connected". The strip is
    /// rebuilt in one step rather than tab by tab: closing them one at a time
    /// made a removed tab the active one, and connected to it, on the way.
    pub(super) fn close_tabs_of_removed_servers(&mut self, cx: &mut Context<Self>) {
        let Ok(servers) = get_servers() else {
            return;
        };
        let surviving = {
            let ids: Vec<&str> = self.tabs.iter().map(|tab| tab.server_id.as_str()).collect();
            tabs_surviving_removal(&ids, self.active_tab, |id| servers.iter().any(|server| server.id == id))
        };
        if surviving.keep.len() == self.tabs.len() && !surviving.home {
            return;
        }
        let active_id = self.tabs[self.active_tab].content.entity_id();
        let mut tabs: Vec<Option<ContentTab>> = std::mem::take(&mut self.tabs).into_iter().map(Some).collect();
        // Dropping a tab's content tears down its subscriptions, server
        // state and connections.
        self.tabs = surviving.keep.iter().filter_map(|&ix| tabs[ix].take()).collect();
        self.active_tab = surviving.active;
        if surviving.home {
            let tab = &mut self.tabs[self.active_tab];
            tab.server_id.clear();
            tab.db = 0;
        }
        let active_changed = self.tabs[self.active_tab].content.entity_id() != active_id;
        if active_changed {
            self.tabs[self.active_tab]
                .content
                .update(cx, |content, cx| content.set_active(true, cx));
            self.rebind_palettes(cx);
        }
        if active_changed || surviving.home {
            self.project_active_tab(cx);
        }
        self.persist_tabs(cx);
        cx.notify();
    }

    /// Close every tab except `ix` (context menu "close others").
    pub(super) fn close_others(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() || self.tabs.len() <= 1 {
            return;
        }
        let was_active = self.active_tab == ix;
        let keep = self.tabs.remove(ix);
        self.tabs.clear();
        self.tabs.push(keep);
        self.active_tab = 0;
        if !was_active {
            self.tabs[0]
                .content
                .update(cx, |content, cx| content.set_active(true, cx));
            self.rebind_palettes(cx);
            self.project_active_tab(cx);
        }
        self.persist_tabs(cx);
        cx.notify();
    }

    /// Close every tab to the right of `ix` (context menu "close right").
    pub(super) fn close_right(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix + 1 >= self.tabs.len() {
            return;
        }
        let active_closed = self.active_tab > ix;
        self.tabs.truncate(ix + 1);
        if active_closed {
            self.active_tab = ix;
            self.tabs[ix]
                .content
                .update(cx, |content, cx| content.set_active(true, cx));
            self.rebind_palettes(cx);
            self.project_active_tab(cx);
        }
        self.persist_tabs(cx);
        cx.notify();
    }

    /// Reorder: move the tab at `from` so it sits at `to` (drag & drop on the
    /// strip). The active index follows its tab.
    pub(super) fn move_tab(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active_tab = moved_active_index(self.active_tab, from, to);
        self.persist_tabs(cx);
        cx.notify();
    }

    /// Persist the strip's `(server_id, db)` list so the next launch restores
    /// the same workspace tabs. Home tabs persist as an empty id — dropping
    /// them here made every Home tab vanish on restart.
    pub(super) fn persist_tabs(&self, cx: &mut Context<Self>) {
        let tabs: Vec<(String, usize)> = self.tabs.iter().map(|tab| (tab.server_id.clone(), tab.db)).collect();
        update_app_state_and_save_quiet(cx, "save_open_tabs", move |state, _| state.set_open_tabs(tabs.clone()));
        // Every change to the set of tabs comes through here, and the
        // sidebar's dots are a view of that same set.
        self.sync_sidebar_health(cx);
    }

    /// The workspace tab strip — hand-rolled pills instead of
    /// gpui-component's `TabBar`, whose `child(impl Into<Tab>)` API can't
    /// carry a context menu / drag-drop / middle-click. Hidden with a single
    /// tab; with more: click activates, × or middle-click closes, drag
    /// reorders, right-click offers close / close-others / close-right.
    /// Wider than the window, it scrolls sideways — a trackpad as it would,
    /// a plain mouse wheel through `ScrollableMask::vertical_wheel`, which a
    /// wheel with only a vertical axis needs to move a horizontal strip.
    pub(super) fn render_tab_bar(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        if self.tabs.len() <= 1 {
            return None;
        }
        let home_label = i18n_sidebar(cx, "home");
        let border = cx.theme().border;
        let foreground = cx.theme().foreground;
        let active_bg = foreground.alpha(0.1);
        let hover_bg = cx.theme().list_hover;
        let radius = cx.theme().radius;
        let muted = cx.theme().muted_foreground;
        let pills = h_flex()
            .id("workspace-tabs")
            .flex_1()
            .min_w_0()
            .gap_1()
            .px_2()
            .py_1()
            .overflow_x_scroll()
            .lock_scroll_axis()
            .track_scroll(&self.tab_scroll)
            .children(self.tabs.iter().enumerate().map(|(ix, tab)| {
                let title: SharedString = if tab.server_id.is_empty() {
                    home_label.clone()
                } else {
                    let name = get_server(&tab.server_id)
                        .map(|server| server.name)
                        .unwrap_or_else(|_| tab.server_id.clone());
                    if tab.db > 0 {
                        format!("{name} [{}]", tab.db).into()
                    } else {
                        name.into()
                    }
                };
                let is_active = ix == self.active_tab;
                // Per-tab jump shortcut (⌘1–8 / Ctrl+1–8): tabs are capped at
                // 8, so every pill gets a hint. Rendered per-platform via
                // `humanize_keystroke`, matching the ⌘/ overlay's symbols.
                // The hint always sits one shade below its title: `muted`
                // under the active tab's full-foreground title, faded muted
                // under an inactive tab's already-muted title.
                let shortcut: SharedString = humanize_keystroke(&format!("cmd-{}", ix + 1)).into();
                let shortcut_color = if is_active { muted } else { muted.alpha(0.6) };
                // `Label` doesn't inherit a parent's `text_color` (its render
                // unconditionally sets the theme foreground before refining
                // with its own style), so the title's active/inactive color
                // must be set on the Label itself.
                let title_color = if is_active { foreground } else { muted };
                let preview_title = title.clone();
                div()
                    .id(("content-tab", ix))
                    .flex_none()
                    .on_drag(DraggedTab { from: ix }, move |_, _, _, cx| {
                        let title = preview_title.clone();
                        cx.new(|_| TabDragPreview { title })
                    })
                    .on_drop(cx.listener(move |this, dragged: &DraggedTab, _window, cx| {
                        this.move_tab(dragged.from, ix, cx);
                    }))
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener(move |this, _, _window, cx| {
                            this.close_tab(ix, cx);
                        }),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.activate_tab(ix, Some(window), cx);
                        this.project_active_tab(cx);
                    }))
                    .child(
                        h_flex()
                            .gap_1()
                            .pl_2()
                            .pr_1()
                            .py_0p5()
                            .rounded(radius)
                            .when(is_active, |this| this.bg(active_bg))
                            .when(!is_active, |this| this.text_color(muted).hover(move |s| s.bg(hover_bg)))
                            // Container-level color still covers the close
                            // button's icon; the Labels set their own below.
                            .child(Label::new(title).text_sm().text_color(title_color).whitespace_nowrap())
                            .child(
                                Label::new(shortcut)
                                    .text_xs()
                                    .text_color(shortcut_color)
                                    .whitespace_nowrap(),
                            )
                            .child(
                                Button::new(("content-tab-close", ix))
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Close)
                                    .tooltip(i18n_common(cx, "tab_close"))
                                    .on_click(cx.listener(move |this, _, _window, cx| {
                                        // Closing is not activating — keep the
                                        // click off the pill underneath.
                                        cx.stop_propagation();
                                        this.close_tab(ix, cx);
                                    })),
                            ),
                    )
                    .context_menu(move |menu, _window, cx| {
                        menu.menu(i18n_common(cx, "tab_close"), Box::new(TabAction::Close(ix)))
                            .menu(
                                i18n_common(cx, "tab_close_others"),
                                Box::new(TabAction::CloseOthers(ix)),
                            )
                            .menu(i18n_common(cx, "tab_close_right"), Box::new(TabAction::CloseRight(ix)))
                    })
            }));
        let strip = h_flex()
            .w_full()
            .flex_none()
            .relative()
            .border_b_1()
            .border_color(border)
            .child(pills)
            // Outside the pills, so their indices are the tabs' own.
            .child(ScrollableMask::vertical_wheel(&self.tab_scroll));
        Some(strip)
    }
}

/// The strip a session ended with, and which of its tabs to activate.
pub(super) fn restore_tabs(window: &mut Window, cx: &mut Context<Zedis>) -> (Vec<ContentTab>, usize) {
    let content = cx.new(|cx| ZedisContent::new(window, cx));
    let mut tabs = vec![ContentTab::new(String::new(), 0, content, cx)];
    let mut active_tab = 0;
    // Restore the last session's workspace tabs. Only the strip layout is
    // rebuilt here — a restored tab's connection loads lazily on first
    // activation (its server state stays empty, so its heartbeat is idle),
    // and the startup composer below activates the remembered
    // `selected_server`, which lands on the matching tab.
    let (saved_tabs, selected) = {
        let store = cx.global::<ZedisGlobalStore>().read(cx);
        // An empty id is a persisted Home tab; server tabs are dropped
        // only when their server no longer exists.
        let saved: Vec<(String, usize)> = store
            .open_tabs()
            .iter()
            .filter(|(id, _)| id.is_empty() || get_server(id).is_ok())
            // Re-resolve each tab's DB so a pinned server restores onto
            // its pin, matching the connection the composer below
            // activates — otherwise the strip and the active connection
            // would disagree about the same server.
            .map(|(id, db)| {
                let db = if id.is_empty() {
                    *db
                } else {
                    store.open_db_from(id, *db)
                };
                (id.clone(), db)
            })
            .collect();
        (saved, store.selected_server().cloned())
    };
    if let Some((id, db)) = saved_tabs.first() {
        tabs[0].server_id = id.clone();
        tabs[0].db = *db;
    }
    for (id, db) in saved_tabs.iter().skip(1) {
        let content = cx.new(|cx| ZedisContent::new(window, cx));
        content.update(cx, |content, cx| content.set_active(false, cx));
        tabs.push(ContentTab::new(id.clone(), *db, content, cx));
    }
    // Reactivate the tab the user left off on: the remembered selection's
    // tab, or — when the session ended on Home (no selection) — the first
    // Home tab, so a restored Home tab actually hosts the Home route
    // instead of tab 0 showing Home under a server-bound tab.
    let remembered = match &selected {
        Some((id, db)) => tabs.iter().position(|tab| &tab.server_id == id && tab.db == *db),
        None => tabs.iter().position(|tab| tab.server_id.is_empty()),
    };
    if let Some(ix) = remembered
        && ix != 0
    {
        active_tab = ix;
        tabs[0].content.update(cx, |content, cx| content.set_active(false, cx));
        tabs[ix].content.update(cx, |content, cx| content.set_active(true, cx));
    }
    (tabs, active_tab)
}

#[cfg(test)]
mod tests {
    use super::{moved_active_index, tabs_surviving_removal};

    /// Deleting a connection takes its tabs with it (#209); the strip keeps
    /// its active tab where it can, and is never left without one.
    #[test]
    fn tabs_on_a_removed_connection_close_and_the_active_one_moves_left() {
        let exists = |id: &str| id != "b";
        // Tab 1 (the active one) was on "b": tab 0 takes over.
        let left = tabs_surviving_removal(&["a", "b", "c"], 1, exists);
        assert_eq!((left.keep, left.active, left.home), (vec![0, 2], 0, false));
        // Another tab was: the active one stays active at its new index.
        let kept = tabs_surviving_removal(&["a", "b", "c"], 2, exists);
        assert_eq!((kept.keep, kept.active, kept.home), (vec![0, 2], 1, false));
        // Nothing left of it: the first that stays.
        let first = tabs_surviving_removal(&["b", "", "c"], 0, exists);
        assert_eq!((first.keep, first.active, first.home), (vec![1, 2], 0, false));
        // Home tabs are on no connection and always stay.
        let home = tabs_surviving_removal(&["", "b"], 1, exists);
        assert_eq!((home.keep, home.active, home.home), (vec![0], 0, false));
        // Every tab was on it: the active one stays and goes Home.
        let last = tabs_surviving_removal(&["b", "b"], 1, exists);
        assert_eq!((last.keep, last.active, last.home), (vec![1], 0, true));
        // Nothing was removed: nothing changes.
        let none = tabs_surviving_removal(&["a", "c"], 1, |_| true);
        assert_eq!((none.keep, none.active, none.home), (vec![0, 1], 1, false));
    }

    /// Reference model: perform the actual remove+insert on tab ids and
    /// locate where the active id ended up.
    fn model(len: usize, active: usize, from: usize, to: usize) -> usize {
        let mut v: Vec<usize> = (0..len).collect();
        let moved = v.remove(from);
        v.insert(to, moved);
        v.iter().position(|&id| id == active).expect("active id present")
    }

    #[test]
    fn active_index_follows_tab_moves_exhaustively() {
        // Exhaustive over every (len ≤ 6, active, from, to) combination,
        // checked against the reference model — locks the drag-reorder
        // index arithmetic in `Zedis::move_tab`.
        for len in 1..=6usize {
            for active in 0..len {
                for from in 0..len {
                    for to in 0..len {
                        assert_eq!(
                            moved_active_index(active, from, to),
                            model(len, active, from, to),
                            "len={len} active={active} from={from} to={to}"
                        );
                    }
                }
            }
        }
    }
}
