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

//! What the server pushes on a held connection: Pub/Sub messages and the
//! `MONITOR` feed.
//!
//! These are the streaming panels' half of ADR 10. A view owns the *loop* —
//! a cancellable task it drops to stop, a channel to its drainer — and asks
//! one of these for the next item; it never sees the connection, the
//! `redis::Msg`, or which of the two Pub/Sub transports it got. Dropping the
//! value closes the connection, which is also how one unsubscribes.
//!
//! Native only, like the panels themselves: a held socket has no equivalent
//! over the HTTP bridge (ADR 9).

use crate::async_connection::{open_monitor_connection, open_single_client};
use crate::config::RedisServer;
use crate::error::Error;
use crate::manager::{ShardedPubSub, get_connection_manager, open_push_connection};
use crate::server_db::ServerDb;
use crate::ssh_tunnel::open_ssh_tunnel_monitor;
use futures::{Stream, StreamExt};
use redis::aio::{MultiplexedConnection, PubSub};
use redis::{Msg, PushInfo, PushKind, cmd};
use std::pin::Pin;

type Result<T, E = Error> = std::result::Result<T, E>;

/// One message as it arrived: the channel it was published to (not the
/// pattern that matched it) and the payload's bytes, undecoded — what they
/// mean is the view's business.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelMessage {
    pub channel: String,
    pub payload: Vec<u8>,
}

impl From<Msg> for ChannelMessage {
    fn from(msg: Msg) -> Self {
        Self {
            channel: msg.get_channel_name().to_string(),
            payload: msg.get_payload_bytes().to_vec(),
        }
    }
}

/// Which Pub/Sub a subscription speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscribeKind {
    /// Classic, by pattern: `PSUBSCRIBE news.* alerts.*`, broadcast
    /// cluster-wide.
    Patterns,
    /// Sharded (Redis 7+): `SSUBSCRIBE` with exact channel names, routed to
    /// the node that owns each channel's slot.
    Sharded,
    /// Keyspace notifications: patterns, on every master. A node publishes
    /// the events of the keys it holds and no other, so on a cluster one
    /// subscription heard about one master's keys; a `PUBLISH`ed message,
    /// by contrast, reaches every node, which is why [`Self::Patterns`]
    /// keeps to one and does not hear each message once per master.
    KeyspaceEvents,
}

/// The two transports behind [`SubscribeKind`]: the dedicated RESP2 Pub/Sub
/// connection, or a RESP3 push connection (the only way redis-rs routes
/// `SSUBSCRIBE`).
enum Transport {
    Patterns(Box<PubSub>),
    Sharded(Box<ShardedPubSub>),
    /// RESP3 push connections, their pushes merged on one channel: where
    /// the classic connection cannot go — an SSH tunnel — or where one is
    /// not enough — keyspace events on each master of a cluster.
    Pushes {
        _connections: Vec<MultiplexedConnection>,
        pushes: smol::channel::Receiver<PushInfo>,
    },
}

/// A live subscription. Drop it to unsubscribe.
pub struct ChannelSubscription {
    transport: Transport,
}

impl ChannelSubscription {
    /// Dial a Pub/Sub connection of its own for the server and subscribe.
    /// Pub/Sub has no databases; `at` names the server.
    pub async fn open(at: &ServerDb, kind: SubscribeKind, channels: &[&str]) -> Result<Self> {
        let manager = get_connection_manager();
        let transport = match kind {
            SubscribeKind::Patterns | SubscribeKind::KeyspaceEvents => {
                // The nodes the server resolves to, not the entry's own
                // address: a Sentinel entry names the sentinels, where a
                // subscription heard nothing an application published.
                let masters = at.client().await?.master_servers();
                let nodes: Vec<RedisServer> = if kind == SubscribeKind::KeyspaceEvents {
                    masters
                } else {
                    masters.into_iter().take(1).collect()
                };
                match nodes.as_slice() {
                    [node] if !node.is_ssh_tunnel() => {
                        let mut pubsub = open_single_client(node)?.get_async_pubsub().await?;
                        pubsub.psubscribe(channels).await?;
                        Transport::Patterns(Box::new(pubsub))
                    }
                    _ => pushed_patterns(&nodes, channels).await?,
                }
            }
            SubscribeKind::Sharded => {
                let mut pubsub = manager.get_sharded_pubsub(at.server_id()).await?;
                pubsub.ssubscribe(channels).await?;
                Transport::Sharded(Box::new(pubsub))
            }
        };
        Ok(Self { transport })
    }

    /// The next message; `None` once the connection is gone.
    pub async fn next_message(&mut self) -> Option<ChannelMessage> {
        let msg = match &mut self.transport {
            // `on_message` hands out the connection's one stream, so asking
            // again for every message loses none.
            Transport::Patterns(pubsub) => pubsub.on_message().next().await,
            Transport::Sharded(pubsub) => pubsub.recv().await,
            Transport::Pushes { pushes, .. } => loop {
                // Subscribe acknowledgements and the like are pushes too.
                let info = pushes.recv().await.ok()?;
                if matches!(info.kind, PushKind::PMessage | PushKind::Message)
                    && let Some(msg) = Msg::from_push_info(info)
                {
                    break Some(msg);
                }
            },
        };
        msg.map(ChannelMessage::from)
    }
}

/// `PSUBSCRIBE channels` on a push connection to each of `nodes`, one
/// channel carrying what any of them publishes. An empty `nodes` (a server
/// whose masters are not known) is an error rather than a silent nothing.
async fn pushed_patterns(nodes: &[RedisServer], channels: &[&str]) -> Result<Transport> {
    if nodes.is_empty() {
        return Err(Error::Invalid {
            message: "no node to subscribe on".to_string(),
        });
    }
    let (tx, pushes) = smol::channel::unbounded::<PushInfo>();
    let mut connections = Vec::with_capacity(nodes.len());
    for node in nodes {
        let mut connection = open_push_connection(node, tx.clone()).await?;
        let mut psubscribe = cmd("PSUBSCRIBE");
        for channel in channels {
            psubscribe.arg(*channel);
        }
        psubscribe.exec_async(&mut connection).await?;
        connections.push(connection);
    }
    Ok(Transport::Pushes {
        _connections: connections,
        pushes,
    })
}

/// The `MONITOR` feed of one node.
pub struct MonitorFeed {
    node: String,
    lines: Pin<Box<dyn Stream<Item = String> + Send>>,
}

impl MonitorFeed {
    /// The node it watches, as `host:port`.
    pub fn node(&self) -> &str {
        &self.node
    }

    /// The next line as the server wrote it (`1700000000.1 [0 127.0.0.1:1]
    /// "GET" "k"`); `None` once the connection is gone.
    pub async fn next_line(&mut self) -> Option<String> {
        self.lines.next().await
    }
}

/// One feed per master — a cluster's commands are spread over all of them —
/// and, by node, the ones that could not be opened. Some of each is a normal
/// outcome (one node down, `MONITOR` denied on another), so neither is an
/// `Err`: the caller watches what it got and reports the rest.
pub struct MonitorFeeds {
    pub feeds: Vec<MonitorFeed>,
    pub failures: Vec<(String, Error)>,
}

/// Open a dedicated `MONITOR` connection to every master of the server.
/// `Err` is for not knowing the masters at all.
pub async fn open_monitor_feeds(at: &ServerDb) -> Result<MonitorFeeds> {
    let servers = at.client().await?.master_servers();
    let mut opened = MonitorFeeds {
        feeds: Vec::with_capacity(servers.len()),
        failures: Vec::new(),
    };
    for server in servers {
        let node = format!("{}:{}", server.host, server.port);
        match monitor_lines(&server).await {
            Ok(lines) => opened.feeds.push(MonitorFeed { node, lines }),
            Err(e) => opened.failures.push((node, e)),
        }
    }
    Ok(opened)
}

/// One node's feed: redis-rs's own `Monitor` on a socket, or — for an SSH
/// entry, whose address means something only at the far end of the tunnel —
/// the feed read through the tunnel.
async fn monitor_lines(server: &RedisServer) -> Result<Pin<Box<dyn Stream<Item = String> + Send>>> {
    if server.is_ssh_tunnel() {
        return Ok(Box::pin(open_ssh_tunnel_monitor(server).await?));
    }
    let monitor = open_monitor_connection(server).await?;
    Ok(Box::pin(monitor.into_on_message::<String>()))
}
