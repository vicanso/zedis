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

//! Keyspace Notifications subscriber.
//!
//! Subscribes to both `__keyspace@*__:*` and `__keyevent@*__:*` on the
//! connected server so the operator can answer "which client just
//! touched key X" without leaving the GUI. Same background-task pattern
//! as the generic Pub/Sub editor, with three additions on top:
//!
//! 1. Channel-name parsing into `(db, key, event, source)` — so the
//!    table can show key/event in their own columns instead of dumping
//!    the raw `__keyspace@0__:foo` string in one cell.
//! 2. Event-type chip filter and key-pattern substring filter on the
//!    captured rows (filters apply *post-subscription* so toggling them
//!    doesn't cycle the connection).
//! 3. An inline config banner that lights up when
//!    `notify-keyspace-events` is empty (no event categories enabled
//!    means the subscription connects fine but never receives a
//!    message), with a one-click "Enable (AKE)" button gated by the // spellchecker:disable-line
//!    standard confirm dialog on PROD-tagged servers.

use crate::connection::{get_connection_manager, get_server};
use crate::error::Error;
use crate::states::{
    Route, ZedisGlobalStore, ZedisServerState, dialog_button_props, i18n_common, i18n_keyspace_notifications,
};
use ahash::AHashSet;
use chrono::Local;
use gpui::{Entity, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    scroll::ScrollableElement,
    v_flex,
};
use std::sync::Arc;
use tracing::error;
use zedis_ui::ZedisDialog;

/// Subscribe to both `__keyspace@*__:*` and `__keyevent@*__:*` in one
/// `PSUBSCRIBE` call — every key write emits two events (one per
/// channel family), and we want both so the user can spot which family
/// the message came from.
const KEYSPACE_PATTERN: &str = "__keyspace@*__:*";
const KEYEVENT_PATTERN: &str = "__keyevent@*__:*";
/// Default value passed to `CONFIG SET notify-keyspace-events` when the
/// user clicks Enable. `K`=keyspace, `E`=keyevent, `A`=all-events.
/// Equivalent to `KEA` (Redis aliases the shorthand).
const ENABLE_FLAGS: &str = "AKE"; // spellchecker:disable-line
/// Cap on retained messages. A hot key on a busy server can emit
/// hundreds per second — without a bound we'd run away with memory in
/// minutes. 1000 keeps the last few minutes visible on most workloads.
const RING_BUFFER_CAPACITY: usize = 1000;

#[derive(Clone, Debug)]
struct NotificationRow {
    timestamp: SharedString,
    /// Redis DB number extracted from the channel name (`__keyspace@N__`).
    db: u32,
    /// The key the event happened to.
    key: SharedString,
    /// Event verb — `set`, `del`, `expire`, `expired`, `hset`, …
    /// Lowercased so chip filtering is case-insensitive.
    event: SharedString,
    /// Which channel family delivered this row — keyspace vs keyevent.
    /// Each write surfaces twice (once per family), and surfacing the
    /// source helps operators spot stuck/missing subscriptions.
    source: NotificationSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NotificationSource {
    Keyspace,
    Keyevent,
}

impl NotificationSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Keyspace => "keyspace",
            Self::Keyevent => "keyevent",
        }
    }
}

/// Default event-type chips shown in the filter row. Order roughly
/// matches frequency on a typical workload — set/del/expire first, the
/// long tail (hset/sadd/...) follows.
const DEFAULT_EVENT_CHIPS: &[&str] = &[
    "set",
    "del",
    "expire",
    "expired",
    "evicted",
    "rename_from",
    "rename_to",
    "hset",
    "hdel",
    "sadd",
    "srem",
    "lpush",
    "rpush",
    "zadd",
    "zrem",
    "xadd",
];

pub struct ZedisKeyspaceNotifications {
    server_state: Entity<ZedisServerState>,
    title: SharedString,
    /// Ring buffer of received notifications, newest first.
    rows: Arc<Vec<NotificationRow>>,
    /// Long-running PSUBSCRIBE task. `Some` ⇒ currently subscribed.
    subscribe_task: Option<Task<()>>,
    /// True while the initial subscribe handshake is in flight (button
    /// shows spinner instead of "Stop").
    subscribing: bool,
    /// Latest read of `notify-keyspace-events`. Empty string ⇒ tracking
    /// is OFF and we surface the banner. Refreshed on every subscribe
    /// attempt (and after a successful Enable).
    notify_flags: SharedString,
    /// Once the flags have been fetched at least once, don't keep
    /// re-firing the request on every render. The fetch itself updates
    /// `notify_flags` asynchronously so we'd otherwise loop a fetch
    /// every frame while it's empty.
    notify_flags_fetched: bool,
    /// Set of event names the user has filtered IN. `None` = show all,
    /// `Some(set)` = show only events in the set (empty set ⇒ nothing).
    selected_events: Option<AHashSet<String>>,
    /// Substring filter applied to the key column. Stored separately
    /// from the InputState so subscriptions can compute filtered rows
    /// in a single &self borrow.
    key_filter: String,
    key_filter_input: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisKeyspaceNotifications {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = server_state.read(cx);
        let server_id = state.server_id();
        let name = get_server(server_id)
            .map(|s| s.name)
            .unwrap_or_else(|_| "--".to_string());
        let title: SharedString = name.into();

        let key_filter_input = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_keyspace_notifications(cx, "key_pattern_placeholder"))
        });

        let mut subscriptions = Vec::new();
        // Key filter is post-subscription: typing into the input only
        // narrows the visible rows, no SUBSCRIBE churn.
        subscriptions.push(
            cx.subscribe_in(&key_filter_input, window, |this, state, event, _window, cx| {
                if let InputEvent::Change = event {
                    this.key_filter = state.read(cx).value().to_string();
                    cx.notify();
                }
            }),
        );

        Self {
            server_state,
            title,
            rows: Arc::new(Vec::new()),
            subscribe_task: None,
            subscribing: false,
            notify_flags: SharedString::default(),
            notify_flags_fetched: false,
            selected_events: None,
            key_filter: String::new(),
            key_filter_input,
            _subscriptions: subscriptions,
        }
    }

    fn is_subscribed(&self) -> bool {
        self.subscribe_task.is_some()
    }

    /// Open the dedicated pub/sub connection and start the read loop.
    fn start_subscribe(&mut self, cx: &mut Context<Self>) {
        if self.is_subscribed() || self.subscribing {
            return;
        }
        let server_id = self.server_state.read(cx).server_id().to_string();
        if server_id.is_empty() {
            return;
        }
        self.subscribing = true;
        cx.notify();

        let entity = cx.entity().downgrade();
        let server_id_for_task = server_id.clone();

        // Also kick a separate config refresh — the banner depends on
        // it and we want it accurate the moment Subscribe lands.
        self.refresh_notify_flags(cx);

        self.subscribe_task = Some(cx.spawn(async move |_handle, cx| {
            let result: Result<_, Error> = cx
                .background_spawn(async move {
                    let mut pubsub = get_connection_manager()
                        .get_pubsub_connection(&server_id_for_task)
                        .await?;
                    // redis-rs's `psubscribe` takes `impl ToRedisArgs`,
                    // which is implemented for `Vec<T>` and `&[T]` but
                    // not the raw `[T; N]` array literal — wrap in a
                    // Vec to match the same path the generic Pub/Sub
                    // editor uses.
                    let patterns: Vec<&str> = vec![KEYSPACE_PATTERN, KEYEVENT_PATTERN];
                    pubsub
                        .psubscribe(patterns)
                        .await
                        .map_err(|e| Error::Invalid { message: e.to_string() })?;
                    Ok(pubsub)
                })
                .await;

            match result {
                Ok(mut pubsub) => {
                    let _ = entity.update(cx, |this, cx| {
                        this.subscribing = false;
                        cx.notify();
                    });
                    use futures::StreamExt;
                    let mut stream = pubsub.on_message();
                    loop {
                        let Some(msg) = stream.next().await else { break };
                        let channel: String = msg.get_channel_name().to_string();
                        let payload = msg.get_payload_bytes();
                        let Some(row) = parse_notification(&channel, payload) else {
                            continue;
                        };
                        let update_result = entity.update(cx, move |this, cx| {
                            this.push_row(row);
                            cx.notify();
                        });
                        // Entity dropped → bail rather than leak.
                        if update_result.is_err() {
                            break;
                        }
                    }
                }
                Err(e) => {
                    error!(error = %e, "Keyspace notifications subscribe failed");
                    let _ = entity.update(cx, |this, cx| {
                        this.subscribing = false;
                        this.subscribe_task = None;
                        cx.notify();
                    });
                }
            }
        }));
    }

    fn stop_subscribe(&mut self, cx: &mut Context<Self>) {
        if self.subscribe_task.take().is_some() {
            self.subscribing = false;
            cx.notify();
        }
    }

    fn clear_rows(&mut self, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        self.rows = Arc::new(Vec::new());
        cx.notify();
    }

    /// Append a row to the buffer, dropping the oldest when capacity is
    /// reached. We mutate via `Arc::make_mut` so the existing snapshot
    /// (in flight to a previous render) stays untouched.
    fn push_row(&mut self, row: NotificationRow) {
        let buf = Arc::make_mut(&mut self.rows);
        if buf.len() >= RING_BUFFER_CAPACITY {
            // Newest-first: drop from the tail (oldest) when at cap.
            buf.pop();
        }
        buf.insert(0, row);
    }

    fn refresh_notify_flags(&mut self, cx: &mut Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            return;
        }
        self.notify_flags_fetched = true;
        let entity = cx.entity().downgrade();
        cx.spawn(async move |_handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                let pair: Vec<String> = redis::cmd("CONFIG")
                    .arg("GET")
                    .arg("notify-keyspace-events")
                    .query_async(&mut conn)
                    .await
                    .map_err(|e| Error::Invalid { message: e.to_string() })?;
                Ok::<_, Error>(pair.get(1).cloned().unwrap_or_default())
            });
            let result = task.await;
            let _ = entity.update(cx, |this, cx| {
                if let Ok(flags) = result {
                    this.notify_flags = flags.into();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Drive the Enable banner click. Non-PROD goes straight to CONFIG
    /// SET; PROD-tag detours through the standard alert dialog so a
    /// stray click doesn't silently change a production config.
    fn enable_notifications(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        if server_id.is_empty() {
            return;
        }
        let high_risk = get_server(&server_id).map(|s| s.is_high_risk_tag()).unwrap_or(false);
        if high_risk {
            self.open_enable_confirm(window, cx);
        } else {
            self.run_enable(cx);
        }
    }

    fn open_enable_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = i18n_keyspace_notifications(cx, "enable_confirm_title");
        let body = i18n_keyspace_notifications(cx, "enable_confirm_body");
        let editor = cx.entity().downgrade();
        ZedisDialog::new_alert(title, body.to_string())
            .button_props(dialog_button_props(cx))
            .on_ok(move |_, window, cx| {
                if let Some(editor) = editor.upgrade() {
                    editor.update(cx, |this, cx| this.run_enable(cx));
                }
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    fn run_enable(&mut self, cx: &mut Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        let entity = cx.entity().downgrade();
        cx.spawn(async move |_handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                redis::cmd("CONFIG")
                    .arg("SET")
                    .arg("notify-keyspace-events")
                    .arg(ENABLE_FLAGS)
                    .query_async::<()>(&mut conn)
                    .await
                    .map_err(|e| Error::Invalid { message: e.to_string() })?;
                Ok::<_, Error>(())
            });
            let _ = task.await;
            let _ = entity.update(cx, |this, cx| {
                this.refresh_notify_flags(cx);
            });
        })
        .detach();
    }

    fn toggle_event_chip(&mut self, event: &str, cx: &mut Context<Self>) {
        // First click on any chip transitions from "show all" (None) to
        // an explicit set with just that chip selected. Click the same
        // chip again to remove it. The "All" pill resets back to None.
        let set = self.selected_events.get_or_insert_with(AHashSet::new);
        if !set.insert(event.to_string()) {
            set.remove(event);
        }
        cx.notify();
    }

    fn reset_event_filter(&mut self, cx: &mut Context<Self>) {
        if self.selected_events.take().is_some() {
            cx.notify();
        }
    }

    /// Materialise the visible rows: pass through if no filters are
    /// active, otherwise filter by event-type set and key substring.
    fn visible_rows(&self) -> Vec<NotificationRow> {
        self.rows
            .iter()
            .filter(|r| match &self.selected_events {
                None => true,
                Some(set) => set.contains(r.event.as_ref()),
            })
            .filter(|r| {
                if self.key_filter.is_empty() {
                    true
                } else {
                    r.key.contains(&self.key_filter)
                }
            })
            .cloned()
            .collect()
    }

    // ── Render helpers ────────────────────────────────────────────────

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let subscribed = self.is_subscribed();
        let subscribing = self.subscribing;

        let action_btn = if subscribed {
            Button::new("ksn-stop")
                .outline()
                .small()
                .label(i18n_keyspace_notifications(cx, "stop"))
                .icon(IconName::CircleX)
                .on_click(cx.listener(|this, _, _w, cx| this.stop_subscribe(cx)))
        } else {
            Button::new("ksn-subscribe")
                .primary()
                .small()
                .loading(subscribing)
                .disabled(subscribing)
                .label(i18n_keyspace_notifications(cx, "subscribe"))
                .icon(IconName::Play)
                .on_click(cx.listener(|this, _, _w, cx| this.start_subscribe(cx)))
        };

        let rows_total = self.rows.len();
        let count_label: SharedString = if rows_total == 0 {
            SharedString::default()
        } else {
            format!("({rows_total})").into()
        };

        h_flex()
            .w_full()
            .h(px(40.))
            .px_4()
            .justify_between()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("ksn-back")
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
                    .child(Label::new(i18n_keyspace_notifications(cx, "title")).font_semibold())
                    .child(Label::new(self.title.clone()).text_color(theme.muted_foreground))
                    .child(Label::new(count_label).text_xs().text_color(theme.muted_foreground)),
            )
            .child(
                h_flex().gap_2().child(action_btn).child(
                    Button::new("ksn-clear")
                        .ghost()
                        .small()
                        .label(i18n_keyspace_notifications(cx, "clear"))
                        .disabled(self.rows.is_empty())
                        .on_click(cx.listener(|this, _, _w, cx| this.clear_rows(cx))),
                ),
            )
    }

    fn render_config_banner(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.notify_flags.is_empty() {
            return None;
        }
        let theme = cx.theme();
        Some(
            div()
                .mx_4()
                .my_2()
                .p_3()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.warning)
                .bg(theme.warning.opacity(0.1))
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Icon::new(IconName::Info).text_color(theme.warning))
                        .child(
                            Label::new(i18n_keyspace_notifications(cx, "banner_disabled"))
                                .text_sm()
                                .text_color(theme.warning)
                                .flex_1(),
                        )
                        .child(
                            Button::new("ksn-enable")
                                .primary()
                                .small()
                                .label(i18n_keyspace_notifications(cx, "enable_button"))
                                .on_click(cx.listener(|this, _, w, cx| this.enable_notifications(w, cx))),
                        ),
                )
                .into_any_element(),
        )
    }

    fn render_filter_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let chips: Vec<gpui::AnyElement> = DEFAULT_EVENT_CHIPS
            .iter()
            .enumerate()
            .map(|(i, event)| {
                let event_owned = (*event).to_string();
                let active = match &self.selected_events {
                    None => true,
                    Some(set) => set.contains(*event),
                };
                let mut btn = Button::new(("ksn-chip", i as u32)).xsmall().label(*event);
                btn = if active { btn.primary() } else { btn.outline() };
                btn.on_click(cx.listener(move |this, _, _w, cx| {
                    this.toggle_event_chip(&event_owned, cx);
                }))
                .into_any_element()
            })
            .collect();

        v_flex()
            .w_full()
            .gap_2()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .flex_wrap()
                    .child(
                        Label::new(i18n_keyspace_notifications(cx, "filter_event_types"))
                            .text_xs()
                            .text_color(theme.muted_foreground),
                    )
                    .child({
                        // "All" pill resets the explicit filter back to
                        // "no filter" — distinct from un-clicking every
                        // chip individually (which would leave the
                        // selected_events set as empty and show nothing).
                        let active = self.selected_events.is_none();
                        let mut btn = Button::new("ksn-chip-all")
                            .xsmall()
                            .label(i18n_keyspace_notifications(cx, "filter_event_all"));
                        btn = if active { btn.primary() } else { btn.outline() };
                        btn.on_click(cx.listener(|this, _, _w, cx| this.reset_event_filter(cx)))
                    })
                    .children(chips),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Label::new(i18n_keyspace_notifications(cx, "filter_key_pattern"))
                            .text_xs()
                            .text_color(theme.muted_foreground),
                    )
                    .child(Input::new(&self.key_filter_input).flex_1()),
            )
    }

    fn render_message_table(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let visible = self.visible_rows();

        let header = h_flex()
            .px_3()
            .py_2()
            .gap_4()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div().w(px(160.)).child(
                    Label::new(i18n_keyspace_notifications(cx, "col_time"))
                        .text_xs()
                        .text_color(muted),
                ),
            )
            .child(
                div().w(px(40.)).child(
                    Label::new(i18n_keyspace_notifications(cx, "col_db"))
                        .text_xs()
                        .text_color(muted),
                ),
            )
            .child(
                div().flex_1().child(
                    Label::new(i18n_keyspace_notifications(cx, "col_key"))
                        .text_xs()
                        .text_color(muted),
                ),
            )
            .child(
                div().w(px(110.)).child(
                    Label::new(i18n_keyspace_notifications(cx, "col_event"))
                        .text_xs()
                        .text_color(muted),
                ),
            )
            .child(
                div().w(px(80.)).child(
                    Label::new(i18n_keyspace_notifications(cx, "col_source"))
                        .text_xs()
                        .text_color(muted),
                ),
            );

        let rows: Vec<gpui::AnyElement> = visible
            .iter()
            .map(|r| self.render_row(r, cx).into_any_element())
            .collect();

        v_flex().flex_1().min_h_0().child(header).child(
            div()
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .child(v_flex().children(rows)),
        )
    }

    fn render_row(&self, r: &NotificationRow, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let event_color = match r.event.as_ref() {
            "del" | "expired" | "evicted" => theme.red,
            "set" | "hset" | "sadd" | "zadd" | "lpush" | "rpush" | "xadd" => theme.green,
            "expire" => theme.yellow,
            _ => theme.foreground,
        };

        h_flex()
            .px_3()
            .py_1()
            .gap_4()
            .border_b_1()
            .border_color(theme.border.opacity(0.5))
            .child(
                div()
                    .w(px(160.))
                    .child(Label::new(r.timestamp.clone()).text_xs().text_color(muted)),
            )
            .child(div().w(px(40.)).child(Label::new(format!("{}", r.db)).text_xs()))
            .child(div().flex_1().child(Label::new(r.key.clone()).text_sm()))
            .child(
                div()
                    .w(px(110.))
                    .child(Label::new(r.event.clone()).text_sm().text_color(event_color)),
            )
            .child(
                div()
                    .w(px(80.))
                    .child(Label::new(r.source.as_str()).text_xs().text_color(muted)),
            )
    }
}

impl Render for ZedisKeyspaceNotifications {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Lazy first-time config read on first render so the banner
        // appears without the user having to click Subscribe first.
        // Guarded by `notify_flags_fetched` to avoid re-firing on every
        // frame while the async fetch is still in flight.
        if !self.notify_flags_fetched {
            self.refresh_notify_flags(cx);
        }

        let mut body = v_flex().size_full();
        body = body.child(self.render_header(cx));
        if let Some(banner) = self.render_config_banner(cx) {
            body = body.child(banner);
        }
        body = body.child(self.render_filter_bar(cx));
        body = body.child(self.render_message_table(cx));
        body.into_any_element()
    }
}

/// Parse a Redis keyspace notification channel + payload into the
/// structured row the table renders. Returns `None` for unrecognised
/// channels (defensively — Redis only emits the two `__keyspace@N__`
/// / `__keyevent@N__` shapes, but a clever user could SUBSCRIBE
/// anything from `redis-cli` mid-session).
fn parse_notification(channel: &str, payload: &[u8]) -> Option<NotificationRow> {
    // Channel shapes:
    //   __keyspace@<db>__:<key>   payload = event verb
    //   __keyevent@<db>__:<event> payload = key
    let inner = channel.strip_prefix("__")?;
    let (kind, rest) = inner.split_once('@')?;
    let (db_str, after_db) = rest.split_once("__:")?;
    let db: u32 = db_str.parse().ok()?;
    if after_db.is_empty() {
        return None;
    }
    let payload_str = String::from_utf8_lossy(payload).into_owned();
    let timestamp = Local::now().format("%H:%M:%S%.3f").to_string().into();

    let (key, event, source) = match kind {
        "keyspace" => (after_db.to_string(), payload_str, NotificationSource::Keyspace),
        "keyevent" => (payload_str, after_db.to_string(), NotificationSource::Keyevent),
        _ => return None,
    };

    Some(NotificationRow {
        timestamp,
        db,
        key: key.into(),
        event: event.to_lowercase().into(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::{NotificationSource, parse_notification};

    #[test]
    fn parses_keyspace_channel() {
        let row = parse_notification("__keyspace@0__:user:42", b"del").expect("parse");
        assert_eq!(row.db, 0);
        assert_eq!(row.key.as_ref(), "user:42");
        assert_eq!(row.event.as_ref(), "del");
        assert_eq!(row.source, NotificationSource::Keyspace);
    }

    #[test]
    fn parses_keyevent_channel_swaps_key_and_event() {
        // For __keyevent@N__:<event> Redis puts the key in the payload,
        // not in the channel — verify we swap correctly.
        let row = parse_notification("__keyevent@3__:expire", b"session:abc").expect("parse");
        assert_eq!(row.db, 3);
        assert_eq!(row.key.as_ref(), "session:abc");
        assert_eq!(row.event.as_ref(), "expire");
        assert_eq!(row.source, NotificationSource::Keyevent);
    }

    #[test]
    fn rejects_non_keyspace_channels() {
        // Generic pub/sub channels accidentally caught by a wider
        // pattern must not produce rows — they'd corrupt the table.
        assert!(parse_notification("my.app.channel", b"hello").is_none());
        // Empty key after the colon — degenerate channel name.
        assert!(parse_notification("__keyspace@0__:", b"del").is_none());
    }

    #[test]
    fn lowercases_event_for_chip_filter_match() {
        // Redis modules can emit events with mixed case; chip filter
        // uses lowercase keys so we normalise on the way in.
        let row = parse_notification("__keyspace@0__:k", b"DEL").expect("parse");
        assert_eq!(row.event.as_ref(), "del");
    }
}
