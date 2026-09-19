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

//! The server list as the app state sees it: which entry is selected, and
//! connecting, adding, editing, reordering and removing entries — including
//! the `redis://` link path. Split out of `app.rs`; the methods are
//! `ZedisAppState`'s as before.

use super::*;

impl ZedisAppState {
    pub fn selected_server(&self) -> Option<&(String, usize)> {
        self.selected_server.as_ref()
    }

    /// Drop the active connection (Home click): clear the snapshot, announce
    /// the empty selection, and route to Home.
    pub fn clear_selected_server(&mut self, cx: &mut Context<Self>) {
        self.selected_server = None;
        cx.emit(GlobalEvent::ServerSelected(SharedString::default(), 0));
        self.go_to(Route::Home, cx);
    }

    /// Connect to a server, landing on the editor — the sidebar / server-card
    /// / palette / tray entry points. The status-bar DB switch, which keeps
    /// the current view, goes through `set_selected_server` instead.
    ///
    /// These entry points mean "(re)connect", not just "navigate", so the
    /// selection is announced *unconditionally* before routing. `apply_route`'s
    /// dedupe compares against the persisted `selected_server` snapshot, which
    /// after a restart can point at this server while nothing is loaded in the
    /// session yet — relying on it alone would skip the `ServerSelected` that
    /// actually loads the connection (the "tray click does nothing" bug).
    pub fn connect_server(&mut self, id: String, db: usize, cx: &mut Context<Self>) {
        self.last_db.insert(id.clone(), db);
        self.selected_server = Some((id.clone(), db));
        cx.emit(GlobalEvent::ServerSelected(id.clone().into(), db));
        self.go_to(
            Route::Server {
                id: id.into(),
                db,
                view: ServerView::Editor,
            },
            cx,
        );
    }

    /// Reveal the tab already bound to `(id, db)`, or open a new one when none
    /// exists (sidebar ⌘/Ctrl+click). The root (`Zedis` in main.rs) owns the
    /// tab list, so this only broadcasts the request; the root re-activates or
    /// creates the tab and then projects the selection back through
    /// `connect_server`.
    pub fn reveal_or_open_server_tab(&mut self, id: String, db: usize, cx: &mut Context<Self>) {
        cx.emit(GlobalEvent::ServerOpenInNewTab(id.into(), db, false));
    }

    /// Always open `(id, db)` in a fresh workspace tab, even when one is already
    /// bound to it (sidebar ⌘/Ctrl+Shift+click) — lets the user run two
    /// workspaces on the same connection. Same broadcast path as
    /// [`Self::reveal_or_open_server_tab`], with dedup skipped by the root.
    pub fn open_server_in_new_tab(&mut self, id: String, db: usize, cx: &mut Context<Self>) {
        cx.emit(GlobalEvent::ServerOpenInNewTab(id.into(), db, true));
    }

    /// Activate a connection: routes to it, keeping the current server view
    /// when one is active (the status-bar DB switch) and falling back to the
    /// editor otherwise (sidebar / server-card / tray connects). Route
    /// transition, snapshot, `last_db` and persistence all flow through
    /// `apply_route`.
    pub fn set_selected_server(&mut self, selected_server: (String, usize), cx: &mut Context<Self>) {
        let (server_id, db) = selected_server;
        if server_id.is_empty() {
            return self.clear_selected_server(cx);
        }
        let view = self.route.server_view().unwrap_or(ServerView::Editor);
        self.go_to(
            Route::Server {
                id: server_id.into(),
                db,
                view,
            },
            cx,
        );
    }

    pub fn remove_server(&mut self, id: &str, cx: &mut Context<Self>) {
        let id = id.to_string();
        cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut servers = get_servers()?;
                servers.retain(|s| s.id != id);
                save_servers(servers.clone()).await?;
                Ok(())
            });
            let result: Result<()> = task.await;
            if let Err(e) = &result {
                error!(error = %e, "Failed to remove server");
            }
            handle.update(cx, |_this, cx| {
                cx.emit(GlobalEvent::ServerListUpdated);
                cx.notify();
            })
        })
        .detach();
    }

    /// Swap the `sort_order` of `server_id` with the adjacent neighbor
    /// in the same `group`, in the requested direction. No-op at the
    /// edge of the group. Persists and broadcasts `ServerListUpdated`
    /// so the grid re-renders in the new order.
    ///
    /// Implementation note: instead of swapping just two sort_order
    /// values, we (a) collect the in-group entries in their current
    /// sorted-on-display order, (b) renumber the entire group as
    /// `0..n` so legacy entries with `sort_order: None` get real
    /// values, then (c) perform the index swap. This guarantees the
    /// operation is observable even on data saved before this field
    /// existed (everyone tied at `None` would otherwise no-op).
    pub fn reorder_server(&mut self, server_id: &str, direction: ReorderDirection, cx: &mut Context<Self>) {
        let server_id = server_id.to_string();
        cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut servers = get_servers()?;
                let Some(target_idx) = servers.iter().position(|s| s.id == server_id) else {
                    return Ok::<(), Error>(());
                };
                let group_key = servers[target_idx]
                    .group
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from);
                let in_same_group = |s: &RedisServer| {
                    s.group
                        .as_deref()
                        .map(str::trim)
                        .filter(|g| !g.is_empty())
                        .map(String::from)
                        == group_key
                };

                // (a) Collect indices belonging to the target's group,
                // in current display order. `get_servers()` already
                // returns canonical sort order, so this list is
                // monotonic.
                let group_indices: Vec<usize> = servers
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| in_same_group(s))
                    .map(|(i, _)| i)
                    .collect();
                let Some(pos_in_group) = group_indices.iter().position(|&i| i == target_idx) else {
                    return Ok(());
                };

                // (c) Determine the swap partner's position in-group.
                let swap_pos = match direction {
                    ReorderDirection::Up if pos_in_group > 0 => pos_in_group - 1,
                    ReorderDirection::Down if pos_in_group + 1 < group_indices.len() => pos_in_group + 1,
                    _ => return Ok(()), // at edge — nothing to do
                };

                // (b) Renumber 0..n in current order, then write the
                // swapped positions back. This means even legacy data
                // (`sort_order: None`) ends up with a stable, distinct
                // sort_order after the first reorder click.
                let mut new_order: Vec<i64> = (0..group_indices.len() as i64).collect();
                new_order.swap(pos_in_group, swap_pos);
                for (slot, &server_idx) in group_indices.iter().enumerate() {
                    servers[server_idx].sort_order = Some(new_order[slot]);
                }

                save_servers(servers).await?;
                Ok(())
            });
            let _: Result<()> = task.await;
            handle.update(cx, |_this, cx| {
                cx.emit(GlobalEvent::ServerListUpdated);
                cx.notify();
            })
        })
        .detach();
    }

    /// Deep link (`redis://…` from the OS or a second launch). The saved
    /// connection with the same host / port / TLS / user wins — a link
    /// carries no name, so that is its identity — otherwise the link is
    /// saved as a new connection; either way the editor opens on it, in the
    /// link's `/db` when it names one.
    pub fn open_server_from_uri(&mut self, server: RedisServer, cx: &mut Context<Self>) {
        let db = server.default_db.map(usize::from);
        let existing = get_servers().ok().and_then(|servers| {
            servers.into_iter().find(|saved| {
                saved.host == server.host
                    && saved.port == server.port
                    && saved.tls.unwrap_or(false) == server.tls.unwrap_or(false)
                    && saved.username == server.username
            })
        });
        if let Some(found) = existing {
            let db = db.unwrap_or_else(|| self.open_db_for(&found.id));
            self.go_to(
                Route::Server {
                    id: found.id.into(),
                    db,
                    view: ServerView::Editor,
                },
                cx,
            );
            return;
        }
        let id = server.id.clone();
        self.upsert_server_then(server, cx, move |state, cx| {
            let db = db.unwrap_or_else(|| state.open_db_for(&id));
            state.go_to(
                Route::Server {
                    id: id.into(),
                    db,
                    view: ServerView::Editor,
                },
                cx,
            );
        });
    }

    pub fn upsert_server(&mut self, server: RedisServer, cx: &mut Context<Self>) {
        self.upsert_server_then(server, cx, |_, _| {});
    }

    /// [`Self::upsert_server`] plus a continuation that runs once the list
    /// is saved — the moment a freshly added server can be selected.
    pub(super) fn upsert_server_then(
        &mut self,
        mut server: RedisServer,
        cx: &mut Context<Self>,
        after: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) {
        if server.id.is_empty() {
            server.id = Uuid::now_v7().to_string();
        }
        server.updated_at = Some(Local::now().to_string());
        cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                if server.name.is_empty() {
                    return Err(Error::Invalid {
                        message: "Server name is required".to_string(),
                    });
                }
                let mut servers = get_servers()?;
                if let Some(existing_server) = servers.iter_mut().find(|s| s.id == server.id) {
                    // Preserve the existing sort_order on update unless
                    // the caller explicitly supplied one (reorder
                    // buttons set it; the edit form leaves it None).
                    if server.sort_order.is_none() {
                        server.sort_order = existing_server.sort_order;
                    }
                    // The desktop form has no notion of an owner, so its
                    // entry arrives without one; a bridge's file edited here
                    // must not turn someone's private entry into everyone's.
                    // In the browser the form does say, and the bridge decides.
                    #[cfg(not(target_family = "wasm"))]
                    if server.owner.is_none() {
                        server.owner = existing_server.owner.clone();
                    }
                    *existing_server = server;
                } else {
                    // New server: append to the tail of its group by
                    // assigning max(sort_order)+1 within that group.
                    if server.sort_order.is_none() {
                        let new_group = server.group.as_deref().map(str::trim).filter(|s| !s.is_empty());
                        let next = servers
                            .iter()
                            .filter(|s| s.group.as_deref().map(str::trim).filter(|g| !g.is_empty()) == new_group)
                            .filter_map(|s| s.sort_order)
                            .max()
                            .map(|m| m + 1)
                            .unwrap_or(0);
                        server.sort_order = Some(next);
                    }
                    servers.push(server);
                }
                save_servers(servers.clone()).await?;
                Ok(())
            });
            let result: Result<()> = task.await;

            handle.update(cx, |this, cx| {
                if let Err(e) = &result {
                    error!(error = %e, "Failed to upsert server");
                    cx.emit(GlobalEvent::Notification(NotificationAction::new_error(
                        e.to_string().into(),
                    )));
                    return;
                }
                cx.emit(GlobalEvent::ServerListUpdated);
                cx.notify();
                after(this, cx);
            })
        })
        .detach();
    }

    /// Insert or update **multiple** servers in one atomic read-modify-save.
    ///
    /// Calling [`Self::upsert_server`] in a loop races: each call is an
    /// independent detached task that reads the whole list, appends one entry,
    /// and writes the list back — so concurrent saves clobber each other and
    /// only one entry survives. Batching reads the list once and saves once.
    pub fn upsert_servers(&mut self, servers: Vec<RedisServer>, cx: &mut Context<Self>) {
        if servers.is_empty() {
            return;
        }
        cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut current = get_servers()?;
                for mut server in servers {
                    // Skip nameless entries rather than abort the whole batch.
                    if server.name.is_empty() {
                        continue;
                    }
                    if server.id.is_empty() {
                        server.id = Uuid::now_v7().to_string();
                    }
                    server.updated_at = Some(Local::now().to_string());
                    if let Some(existing) = current.iter_mut().find(|s| s.id == server.id) {
                        if server.sort_order.is_none() {
                            server.sort_order = existing.sort_order;
                        }
                        *existing = server;
                    } else {
                        // Append to the tail of its group; `sort_order` is
                        // computed against the in-progress list so a batch
                        // gets sequential indices.
                        if server.sort_order.is_none() {
                            let new_group = server.group.as_deref().map(str::trim).filter(|s| !s.is_empty());
                            let next = current
                                .iter()
                                .filter(|s| s.group.as_deref().map(str::trim).filter(|g| !g.is_empty()) == new_group)
                                .filter_map(|s| s.sort_order)
                                .max()
                                .map(|m| m + 1)
                                .unwrap_or(0);
                            server.sort_order = Some(next);
                        }
                        current.push(server);
                    }
                }
                save_servers(current.clone()).await?;
                Ok(())
            });
            let result: Result<()> = task.await;

            handle.update(cx, |_this, cx| {
                if let Err(e) = &result {
                    error!(error = %e, "Failed to upsert servers");
                    cx.emit(GlobalEvent::Notification(NotificationAction::new_error(
                        e.to_string().into(),
                    )));
                    return;
                }
                cx.emit(GlobalEvent::ServerListUpdated);
                cx.notify();
            })
        })
        .detach();
    }
}
