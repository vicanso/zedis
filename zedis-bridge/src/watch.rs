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

//! What the server pushes, for a page: Pub/Sub, keyspace notifications and
//! `MONITOR` (ADR 9, amended 2026-10-17).
//!
//! A page cannot hold a socket to Redis, and a subscribed connection cannot
//! be shared — so the bridge holds the subscription, the same
//! `ChannelSubscription` / `open_monitor_feeds` the desktop's panels read,
//! and buffers what arrives. The page asks for it with a request that waits
//! until something has come or [`MAX_WAIT`] has passed (a long poll): plain
//! request and reply, which every proxy in front of the bridge already
//! forwards, where a server-sent stream or a WebSocket would each be one
//! more thing for a deployment to let through.
//!
//! A watch belongs to the account that opened it, and to nobody else's
//! polls. It ends when the page says so, when nobody has asked for it for
//! [`IDLE_TIMEOUT`] (a closed tab), when the connection under it goes, and —
//! for `MONITOR`, which costs the server on every command it serves — after
//! [`MONITOR_LIMIT`]. What it holds is bounded ([`MAX_BUFFERED`]): past it
//! the oldest is dropped and counted, and the next poll says how many.

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use zedis_connection::{ChannelSubscription, ServerDb, SubscribeKind, open_monitor_feeds};

/// The longest a poll waits for something to arrive. Under the 60 seconds
/// a reverse proxy commonly allows an idle request, with room to spare.
pub const MAX_WAIT: Duration = Duration::from_secs(20);
/// A watch nobody has polled for this long is closed: the page that opened
/// it is gone. Two polls' worth, so a slow round never ends one.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// How long `MONITOR` runs before the bridge stops it.
pub const MONITOR_LIMIT: Duration = Duration::from_secs(10 * 60);
/// What a watch holds between polls; past it the oldest go.
pub const MAX_BUFFERED: usize = 10_000;
/// What one poll hands over.
pub const MAX_BATCH: usize = 1_000;
/// How many watches one account may have open at once.
pub const MAX_PER_ACCOUNT: usize = 8;
/// Patterns or channels one subscription may name, and how long each.
pub const MAX_CHANNELS: usize = 64;
pub const MAX_CHANNEL_LEN: usize = 512;

/// What a watch watches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchKind {
    /// `PSUBSCRIBE` patterns.
    Patterns,
    /// `SSUBSCRIBE` channels (Redis 7+).
    Sharded,
    /// Keyspace notifications, on every master.
    KeyspaceEvents,
    /// `MONITOR`, on every master.
    Monitor,
}

impl WatchKind {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "patterns" => Some(Self::Patterns),
            "sharded" => Some(Self::Sharded),
            "keyspace_events" => Some(Self::KeyspaceEvents),
            "monitor" => Some(Self::Monitor),
            _ => None,
        }
    }

    fn subscribe_kind(self) -> Option<SubscribeKind> {
        match self {
            Self::Patterns => Some(SubscribeKind::Patterns),
            Self::Sharded => Some(SubscribeKind::Sharded),
            Self::KeyspaceEvents => Some(SubscribeKind::KeyspaceEvents),
            Self::Monitor => None,
        }
    }
}

/// One thing that arrived.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Item {
    /// A Pub/Sub message: its channel, and its payload in base64 — a
    /// payload is bytes, and JSON has no other way to carry them.
    Message { channel: String, payload: String },
    /// A `MONITOR` line, and the node that wrote it.
    Line { node: String, line: String },
}

/// What a poll answers.
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct Batch {
    pub items: Vec<Item>,
    /// Dropped since the last poll because the buffer was full.
    pub dropped: u64,
    /// Why the watch has ended, once it has. The page stops asking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed: Option<String>,
}

#[derive(Default)]
struct Buffer {
    items: VecDeque<Item>,
    dropped: u64,
    closed: Option<String>,
}

struct Watch {
    account: String,
    server_id: String,
    kind: WatchKind,
    buffer: Mutex<Buffer>,
    notify: Notify,
    last_polled: Mutex<Instant>,
    /// When `MONITOR` is stopped, whoever is polling.
    ends_at: Option<Instant>,
    /// The readers; aborted when the watch goes, which drops the
    /// subscription and with it the connection.
    readers: Mutex<Vec<JoinHandle<()>>>,
}

impl Watch {
    fn push(&self, item: Item) {
        {
            let mut buffer = self.buffer.lock().unwrap_or_else(PoisonError::into_inner);
            if buffer.items.len() >= MAX_BUFFERED {
                buffer.items.pop_front();
                buffer.dropped += 1;
            }
            buffer.items.push_back(item);
        }
        self.notify.notify_waiters();
    }

    /// Mark it ended, keeping the first reason.
    fn end(&self, reason: &str) {
        {
            let mut buffer = self.buffer.lock().unwrap_or_else(PoisonError::into_inner);
            buffer.closed.get_or_insert_with(|| reason.to_string());
        }
        self.notify.notify_waiters();
    }

    /// Up to [`MAX_BATCH`] items, when there is anything to say.
    fn take(&self) -> Option<Batch> {
        let mut buffer = self.buffer.lock().unwrap_or_else(PoisonError::into_inner);
        if buffer.items.is_empty() && buffer.dropped == 0 && buffer.closed.is_none() {
            return None;
        }
        let n = buffer.items.len().min(MAX_BATCH);
        let items = buffer.items.drain(..n).collect();
        let dropped = std::mem::take(&mut buffer.dropped);
        // Ended, but what came before the end goes out first.
        let closed = buffer.items.is_empty().then(|| buffer.closed.clone()).flatten();
        Some(Batch { items, dropped, closed })
    }

    fn abort(&self) {
        for reader in self.readers.lock().unwrap_or_else(PoisonError::into_inner).drain(..) {
            reader.abort();
        }
    }

    /// The reason it should end now, if it should.
    fn expiry(&self, now: Instant) -> Option<&'static str> {
        if self.ends_at.is_some_and(|at| now >= at) {
            return Some("time limit");
        }
        let idle = now.saturating_duration_since(*self.last_polled.lock().unwrap_or_else(PoisonError::into_inner));
        (idle >= IDLE_TIMEOUT).then_some("idle")
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.abort();
    }
}

/// A watch that was opened, as the route answers it.
pub struct Opened {
    pub token: String,
    /// The nodes a `MONITOR` is reading, as `host:port`.
    pub nodes: Vec<String>,
    /// The nodes it could not start on, and why.
    pub failures: Vec<(String, String)>,
}

/// Why a watch could not be opened or polled.
#[derive(Debug, PartialEq, Eq)]
pub enum WatchError {
    /// The account has [`MAX_PER_ACCOUNT`] open already.
    TooMany,
    /// No such watch, or not this account's.
    Unknown,
    /// The request itself is wrong (no channels, too many, too long).
    Invalid(String),
    /// Redis would not have it.
    Upstream(String),
}

/// Every open watch, by token.
#[derive(Clone, Default)]
pub struct Watches(Arc<Mutex<HashMap<String, Arc<Watch>>>>);

impl Watches {
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribe, or start `MONITOR`, on `server_id` for `account`. The
    /// caller has decided that the account may.
    pub async fn open(
        &self,
        account: &str,
        server_id: &str,
        db: usize,
        kind: WatchKind,
        channels: &[String],
    ) -> Result<Opened, WatchError> {
        if self.count_for(account) >= MAX_PER_ACCOUNT {
            return Err(WatchError::TooMany);
        }
        let at = ServerDb::new(server_id, db);
        let now = Instant::now();
        let watch = Arc::new(Watch {
            account: account.to_string(),
            server_id: server_id.to_string(),
            kind,
            buffer: Mutex::default(),
            notify: Notify::new(),
            last_polled: Mutex::new(now),
            ends_at: (kind == WatchKind::Monitor).then(|| now + MONITOR_LIMIT),
            readers: Mutex::default(),
        });
        let (nodes, failures) = match kind.subscribe_kind() {
            Some(subscribe) => {
                check_channels(channels)?;
                let names: Vec<&str> = channels.iter().map(String::as_str).collect();
                let subscription = ChannelSubscription::open(&at, subscribe, &names)
                    .await
                    .map_err(|e| WatchError::Upstream(e.to_string()))?;
                let reader = tokio::spawn(read_messages(subscription, Arc::downgrade(&watch)));
                watch
                    .readers
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(reader);
                (Vec::new(), Vec::new())
            }
            None => {
                let opened = open_monitor_feeds(&at)
                    .await
                    .map_err(|e| WatchError::Upstream(e.to_string()))?;
                let failures: Vec<(String, String)> = opened
                    .failures
                    .into_iter()
                    .map(|(node, e)| (node, e.to_string()))
                    .collect();
                if opened.feeds.is_empty() {
                    let why = failures
                        .first()
                        .map(|(node, e)| format!("{node}: {e}"))
                        .unwrap_or_else(|| "no node to monitor".to_string());
                    return Err(WatchError::Upstream(why));
                }
                let live = Arc::new(std::sync::atomic::AtomicUsize::new(opened.feeds.len()));
                let mut nodes = Vec::with_capacity(opened.feeds.len());
                let mut readers = watch.readers.lock().unwrap_or_else(PoisonError::into_inner);
                for feed in opened.feeds {
                    nodes.push(feed.node().to_string());
                    readers.push(tokio::spawn(read_lines(feed, Arc::downgrade(&watch), live.clone())));
                }
                drop(readers);
                (nodes, failures)
            }
        };
        let token = uuid::Uuid::new_v4().simple().to_string();
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(token.clone(), watch);
        Ok(Opened { token, nodes, failures })
    }

    /// What has arrived on `token` since the last poll, waiting up to `wait`
    /// (at most [`MAX_WAIT`]) for something to.
    pub async fn poll(&self, token: &str, account: &str, wait: Duration) -> Result<Batch, WatchError> {
        let watch = self.find(token, account)?;
        *watch.last_polled.lock().unwrap_or_else(PoisonError::into_inner) = Instant::now();
        let deadline = Instant::now() + wait.min(MAX_WAIT);
        let batch = loop {
            // Registered before the buffer is looked at, so an item that
            // lands in between still wakes this poll.
            let notified = watch.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(reason) = watch.ends_at.filter(|at| Instant::now() >= *at).map(|_| "time limit") {
                watch.end(reason);
                watch.abort();
            }
            if let Some(batch) = watch.take() {
                break batch;
            }
            let until = watch.ends_at.map_or(deadline, |at| at.min(deadline));
            if tokio::time::timeout_at(until, notified).await.is_err() && Instant::now() >= deadline {
                break Batch::default();
            }
        };
        *watch.last_polled.lock().unwrap_or_else(PoisonError::into_inner) = Instant::now();
        if batch.closed.is_some() {
            self.remove(token);
        }
        Ok(batch)
    }

    /// Which server and [`WatchKind`] `token` watches, if it belongs to `account`.
    pub fn inspect(&self, token: &str, account: &str) -> Result<(String, WatchKind), WatchError> {
        let watch = self.find(token, account)?;
        Ok((watch.server_id.clone(), watch.kind))
    }

    /// End `token` now. Answers whether there was such a watch of `account`'s.
    pub fn close(&self, token: &str, account: &str) -> bool {
        let mut watches = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let mine = watches.get(token).is_some_and(|watch| watch.account == account);
        if mine && let Some(watch) = watches.remove(token) {
            watch.end("closed");
            watch.abort();
        }
        mine
    }

    /// End and remove every watch bound to `server_id` (when the entry is
    /// edited, handed over, or deleted).
    pub fn close_server(&self, server_id: &str) -> usize {
        let mut watches = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let before = watches.len();
        watches.retain(|_, watch| {
            if watch.server_id == server_id {
                watch.end("server changed");
                watch.abort();
                false
            } else {
                true
            }
        });
        before - watches.len()
    }

    /// Close every watch past its idle timeout or its time limit. Answers
    /// how many went.
    pub fn sweep(&self) -> usize {
        let now = Instant::now();
        let mut watches = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let before = watches.len();
        watches.retain(|_, watch| match watch.expiry(now) {
            Some(reason) => {
                watch.end(reason);
                watch.abort();
                false
            }
            None => true,
        });
        before - watches.len()
    }

    pub fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    fn count_for(&self, account: &str) -> usize {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .filter(|watch| watch.account == account)
            .count()
    }

    fn find(&self, token: &str, account: &str) -> Result<Arc<Watch>, WatchError> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(token)
            .filter(|watch| watch.account == account)
            .cloned()
            .ok_or(WatchError::Unknown)
    }

    fn remove(&self, token: &str) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).remove(token);
    }
}

fn check_channels(channels: &[String]) -> Result<(), WatchError> {
    if channels.is_empty() {
        return Err(WatchError::Invalid("name at least one channel or pattern".to_string()));
    }
    if channels.len() > MAX_CHANNELS {
        return Err(WatchError::Invalid(format!(
            "at most {MAX_CHANNELS} channels or patterns at once"
        )));
    }
    if channels.iter().any(|c| c.is_empty() || c.len() > MAX_CHANNEL_LEN) {
        return Err(WatchError::Invalid(format!(
            "a channel or pattern is 1 to {MAX_CHANNEL_LEN} bytes"
        )));
    }
    Ok(())
}

/// Buffer every message until the subscription ends or the watch goes.
async fn read_messages(mut subscription: ChannelSubscription, watch: std::sync::Weak<Watch>) {
    while let Some(message) = subscription.next_message().await {
        let Some(watch) = watch.upgrade() else {
            return;
        };
        watch.push(Item::Message {
            channel: message.channel,
            payload: B64.encode(&message.payload),
        });
    }
    if let Some(watch) = watch.upgrade() {
        watch.end("the connection closed");
    }
}

/// Buffer one node's `MONITOR` lines; the watch ends with the last feed.
async fn read_lines(
    mut feed: zedis_connection::MonitorFeed,
    watch: std::sync::Weak<Watch>,
    live: Arc<std::sync::atomic::AtomicUsize>,
) {
    let node = feed.node().to_string();
    while let Some(line) = feed.next_line().await {
        let Some(watch) = watch.upgrade() else {
            return;
        };
        watch.push(Item::Line {
            node: node.clone(),
            line,
        });
    }
    if live.fetch_sub(1, std::sync::atomic::Ordering::AcqRel) == 1
        && let Some(watch) = watch.upgrade()
    {
        watch.end("the connection closed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A watch made by hand, with no reader behind it.
    fn watch(account: &str, kind: WatchKind) -> Arc<Watch> {
        Arc::new(Watch {
            account: account.to_string(),
            server_id: "srv".to_string(),
            kind,
            buffer: Mutex::default(),
            notify: Notify::new(),
            last_polled: Mutex::new(Instant::now()),
            ends_at: (kind == WatchKind::Monitor).then(|| Instant::now() + MONITOR_LIMIT),
            readers: Mutex::default(),
        })
    }

    fn insert(watches: &Watches, token: &str, watch: Arc<Watch>) {
        watches.0.lock().expect("lock").insert(token.to_string(), watch);
    }

    fn line(n: usize) -> Item {
        Item::Line {
            node: "n".to_string(),
            line: n.to_string(),
        }
    }

    /// A poll waits for what arrives, hands over what came, and wakes at
    /// once for an item pushed while it waits.
    #[tokio::test(start_paused = true)]
    async fn a_poll_waits_for_what_arrives() {
        let watches = Watches::new();
        let w = watch("alice", WatchKind::Patterns);
        insert(&watches, "t", w.clone());

        let empty = watches.poll("t", "alice", Duration::from_secs(5)).await.expect("poll");
        assert_eq!(empty, Batch::default(), "nothing came in five seconds");

        let pusher = w.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            pusher.push(Item::Message {
                channel: "news".to_string(),
                payload: "aGk=".to_string(),
            });
        });
        let started = Instant::now();
        let batch = watches.poll("t", "alice", MAX_WAIT).await.expect("poll");
        assert_eq!(batch.items.len(), 1);
        assert_eq!(
            started.elapsed(),
            Duration::from_secs(2),
            "woken by the push, not the timeout"
        );
    }

    /// A watch is its account's alone.
    #[tokio::test(start_paused = true)]
    async fn another_account_cannot_poll_or_close_it() {
        let watches = Watches::new();
        insert(&watches, "t", watch("alice", WatchKind::KeyspaceEvents));
        assert_eq!(
            watches.poll("t", "mallory", Duration::ZERO).await,
            Err(WatchError::Unknown)
        );
        assert!(!watches.close("t", "mallory"));
        assert_eq!(watches.len(), 1);
        assert!(watches.close("t", "alice"));
        assert_eq!(watches.len(), 0);
    }

    /// Past the buffer the oldest go, and the next poll says how many.
    #[tokio::test(start_paused = true)]
    async fn a_full_buffer_drops_the_oldest_and_says_so() {
        let watches = Watches::new();
        let w = watch("alice", WatchKind::Monitor);
        insert(&watches, "t", w.clone());
        for n in 0..MAX_BUFFERED + 5 {
            w.push(line(n));
        }
        let first = watches.poll("t", "alice", Duration::ZERO).await.expect("poll");
        assert_eq!(first.dropped, 5);
        assert_eq!(first.items.len(), MAX_BATCH);
        assert_eq!(first.items[0], line(5), "the oldest five went");
        let second = watches.poll("t", "alice", Duration::ZERO).await.expect("poll");
        assert_eq!(second.dropped, 0, "said once");
    }

    /// What arrived before the end goes out before the end does, and the
    /// ended watch is gone after it.
    #[tokio::test(start_paused = true)]
    async fn the_end_comes_after_what_came_before_it() {
        let watches = Watches::new();
        let w = watch("alice", WatchKind::Patterns);
        insert(&watches, "t", w.clone());
        w.push(line(1));
        w.end("the connection closed");
        let batch = watches.poll("t", "alice", Duration::ZERO).await.expect("poll");
        assert_eq!(batch.items, [line(1)]);
        assert_eq!(batch.closed.as_deref(), Some("the connection closed"));
        assert_eq!(watches.len(), 0);
    }

    /// `MONITOR` stops at its limit, whoever is polling; a page that stops
    /// polling loses its watch after the idle timeout.
    #[tokio::test(start_paused = true)]
    async fn monitor_stops_at_its_limit_and_a_quiet_watch_is_swept() {
        let watches = Watches::new();
        insert(&watches, "monitor", watch("alice", WatchKind::Monitor));
        insert(&watches, "quiet", watch("bob", WatchKind::Patterns));

        tokio::time::advance(IDLE_TIMEOUT).await;
        assert_eq!(watches.sweep(), 2, "nobody polled either for a minute");

        let monitor = watch("alice", WatchKind::Monitor);
        insert(&watches, "monitor", monitor.clone());
        tokio::time::advance(MONITOR_LIMIT - Duration::from_secs(1)).await;
        *monitor.last_polled.lock().expect("lock") = Instant::now();
        let batch = watches.poll("monitor", "alice", MAX_WAIT).await.expect("poll");
        assert_eq!(batch.closed.as_deref(), Some("time limit"), "ended inside the wait");
        assert_eq!(watches.len(), 0);
    }

    /// An account has a few at a time, not a connection per click.
    #[tokio::test(start_paused = true)]
    async fn an_account_may_hold_a_few_at_once() {
        let watches = Watches::new();
        for n in 0..MAX_PER_ACCOUNT {
            insert(&watches, &n.to_string(), watch("alice", WatchKind::Patterns));
        }
        let refused = watches
            .open("alice", "srv", 0, WatchKind::Patterns, &["news".to_string()])
            .await;
        assert!(matches!(refused, Err(WatchError::TooMany)));
    }

    #[test]
    fn channels_are_named_and_bounded() {
        assert!(check_channels(&[]).is_err());
        assert!(check_channels(&["".to_string()]).is_err());
        assert!(check_channels(&vec!["c".to_string(); MAX_CHANNELS + 1]).is_err());
        assert!(check_channels(&["x".repeat(MAX_CHANNEL_LEN + 1)]).is_err());
        assert!(check_channels(&["news.*".to_string()]).is_ok());
        assert_eq!(WatchKind::from_name("keyspace_events"), Some(WatchKind::KeyspaceEvents));
        assert_eq!(WatchKind::from_name("subscribe"), None);
    }

    #[test]
    fn inspect_checks_owner_and_close_server_drops_bound_watches() {
        let watches = Watches::new();
        insert(&watches, "t1", watch("alice", WatchKind::Monitor));
        let other_srv = Arc::new(Watch {
            account: "bob".to_string(),
            server_id: "other".to_string(),
            kind: WatchKind::Patterns,
            buffer: Mutex::default(),
            notify: Notify::new(),
            last_polled: Mutex::new(Instant::now()),
            ends_at: None,
            readers: Mutex::default(),
        });
        insert(&watches, "t2", other_srv);

        assert_eq!(
            watches.inspect("t1", "alice"),
            Ok(("srv".to_string(), WatchKind::Monitor))
        );
        assert_eq!(watches.inspect("t1", "bob"), Err(WatchError::Unknown));
        assert_eq!(watches.close_server("srv"), 1);
        assert_eq!(watches.inspect("t1", "alice"), Err(WatchError::Unknown));
        assert_eq!(
            watches.inspect("t2", "bob"),
            Ok(("other".to_string(), WatchKind::Patterns))
        );
    }
}
