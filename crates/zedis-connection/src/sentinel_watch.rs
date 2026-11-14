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

//! Real-time Sentinel Pub/Sub event listener (`+switch-master`, `+sdown`,
//! `-sdown`, `+odown`, `-odown`).
//!
//! Sentinel nodes publish failover and health transitions on their own Pub/Sub
//! channels rather than on the data master. Subscribing directly to a Sentinel
//! seed lets Zedis evict the stale pooled client and refresh topology the
//! instant a failover completes instead of waiting for the next heartbeat or
//! a `READONLY` error.

#[cfg(not(target_family = "wasm"))]
use crate::async_connection::open_single_client;
#[cfg(not(target_family = "wasm"))]
use crate::config::RedisServer;
#[cfg(not(target_family = "wasm"))]
use crate::error::{ConnectionErrorKind, Error};
#[cfg(not(target_family = "wasm"))]
use crate::manager::open_push_connection;
#[cfg(not(target_family = "wasm"))]
use futures::StreamExt;
#[cfg(not(target_family = "wasm"))]
use redis::aio::{MultiplexedConnection, PubSub};
#[cfg(not(target_family = "wasm"))]
use redis::{Msg, PushInfo, PushKind, cmd};
use zedis_core::string::format_host_port;

#[cfg(not(target_family = "wasm"))]
type Result<T, E = Error> = std::result::Result<T, E>;

/// Sentinel Pub/Sub channels watched for instant failover and health updates.
pub const SENTINEL_EVENT_CHANNELS: [&str; 5] = ["+switch-master", "+sdown", "-sdown", "+odown", "-odown"];

/// A parsed event from a Sentinel's Pub/Sub channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SentinelEvent {
    /// `+switch-master <master-name> <old-ip> <old-port> <new-ip> <new-port>`
    SwitchMaster {
        master_name: String,
        old_addr: String,
        new_addr: String,
    },
    /// `+sdown` / `-sdown` / `+odown` / `-odown`
    DownStateChanged {
        channel: String,
        instance_type: String,
        name: String,
        addr: String,
        down: bool,
    },
}

/// Parses a Sentinel Pub/Sub message into a typed [`SentinelEvent`].
pub fn parse_sentinel_event(channel: &str, payload: &[u8]) -> Option<SentinelEvent> {
    let text = std::str::from_utf8(payload).ok()?.trim();
    let mut parts = text.split_whitespace();
    match channel {
        "+switch-master" => {
            let master_name = parts.next()?.to_string();
            let old_ip = parts.next()?;
            let old_port: u16 = parts.next()?.parse().ok()?;
            let new_ip = parts.next()?;
            let new_port: u16 = parts.next()?.parse().ok()?;
            Some(SentinelEvent::SwitchMaster {
                master_name,
                old_addr: format_host_port(old_ip, old_port),
                new_addr: format_host_port(new_ip, new_port),
            })
        }
        "+sdown" | "-sdown" | "+odown" | "-odown" => {
            let instance_type = parts.next()?.to_string();
            let name = parts.next()?.to_string();
            let ip = parts.next()?;
            let port: u16 = parts.next()?.parse().ok()?;
            Some(SentinelEvent::DownStateChanged {
                channel: channel.to_string(),
                instance_type,
                name,
                addr: format_host_port(ip, port),
                down: channel.starts_with('+'),
            })
        }
        _ => None,
    }
}

#[cfg(not(target_family = "wasm"))]
enum SentinelTransport {
    PubSub(Box<PubSub>),
    Push {
        _conn: MultiplexedConnection,
        pushes: smol::channel::Receiver<PushInfo>,
    },
}

/// Dedicated Pub/Sub subscription to a Sentinel node. Drop it to unsubscribe.
#[cfg(not(target_family = "wasm"))]
pub struct SentinelEventSubscription {
    transport: SentinelTransport,
}

#[cfg(not(target_family = "wasm"))]
impl SentinelEventSubscription {
    /// Opens a Pub/Sub subscription against the first reachable Sentinel seed.
    pub async fn open(server: &RedisServer) -> Result<Self> {
        let base = if server.has_sentinel_credentials() {
            server.sentinel_login()
        } else {
            server.clone()
        };
        let mut last_error = None;
        for (host, port) in server.seed_endpoints() {
            let mut endpoint = base.clone();
            endpoint.host = host;
            endpoint.port = port;
            match Self::open_endpoint(&endpoint, server.has_sentinel_credentials()).await {
                Ok(sub) => return Ok(sub),
                Err(err) => last_error = Some(err),
            }
        }
        Err(last_error.unwrap_or_else(|| Error::Invalid {
            message: "no sentinel seed address configured".to_string(),
        }))
    }

    async fn open_endpoint(endpoint: &RedisServer, has_sentinel_creds: bool) -> Result<Self> {
        match Self::subscribe_endpoint(endpoint).await {
            Ok(sub) => Ok(sub),
            Err(err) if !has_sentinel_creds && err.connection_kind() == ConnectionErrorKind::Auth => {
                let mut anonymous = endpoint.clone();
                anonymous.password = None;
                Self::subscribe_endpoint(&anonymous).await
            }
            Err(err) => Err(err),
        }
    }

    async fn subscribe_endpoint(endpoint: &RedisServer) -> Result<Self> {
        if endpoint.is_ssh_tunnel() {
            let (tx, pushes) = smol::channel::unbounded::<PushInfo>();
            let mut conn = open_push_connection(endpoint, tx).await?;
            let mut sub = cmd("SUBSCRIBE");
            for channel in SENTINEL_EVENT_CHANNELS {
                sub.arg(channel);
            }
            sub.exec_async(&mut conn).await?;
            return Ok(Self {
                transport: SentinelTransport::Push { _conn: conn, pushes },
            });
        }
        let mut pubsub = open_single_client(endpoint)?.get_async_pubsub().await?;
        pubsub.subscribe(&SENTINEL_EVENT_CHANNELS).await?;
        Ok(Self {
            transport: SentinelTransport::PubSub(Box::new(pubsub)),
        })
    }

    /// Waits for the next recognized Sentinel event, or `None` when the
    /// subscription closes.
    pub async fn next_event(&mut self) -> Option<SentinelEvent> {
        loop {
            let msg = match &mut self.transport {
                SentinelTransport::PubSub(pubsub) => pubsub.on_message().next().await?,
                SentinelTransport::Push { pushes, .. } => loop {
                    let info = pushes.recv().await.ok()?;
                    if matches!(info.kind, PushKind::Message | PushKind::PMessage)
                        && let Some(msg) = Msg::from_push_info(info)
                    {
                        break msg;
                    }
                },
            };
            if let Some(event) = parse_sentinel_event(msg.get_channel_name(), msg.get_payload_bytes()) {
                return Some(event);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_switch_master_and_down_events() {
        assert_eq!(
            parse_sentinel_event("+switch-master", b"mymaster 127.0.0.1 6379 127.0.0.1 6380"),
            Some(SentinelEvent::SwitchMaster {
                master_name: "mymaster".to_string(),
                old_addr: "127.0.0.1:6379".to_string(),
                new_addr: "127.0.0.1:6380".to_string(),
            })
        );
        assert_eq!(
            parse_sentinel_event("+switch-master", b"mymaster ::1 6379 ::1 6380"),
            Some(SentinelEvent::SwitchMaster {
                master_name: "mymaster".to_string(),
                old_addr: "[::1]:6379".to_string(),
                new_addr: "[::1]:6380".to_string(),
            })
        );
        assert_eq!(
            parse_sentinel_event("+sdown", b"master mymaster 10.0.0.1 6379"),
            Some(SentinelEvent::DownStateChanged {
                channel: "+sdown".to_string(),
                instance_type: "master".to_string(),
                name: "mymaster".to_string(),
                addr: "10.0.0.1:6379".to_string(),
                down: true,
            })
        );
        assert_eq!(
            parse_sentinel_event("-odown", b"master mymaster 10.0.0.1 6379 #quorum 2/2"),
            Some(SentinelEvent::DownStateChanged {
                channel: "-odown".to_string(),
                instance_type: "master".to_string(),
                name: "mymaster".to_string(),
                addr: "10.0.0.1:6379".to_string(),
                down: false,
            })
        );
        assert_eq!(parse_sentinel_event("+hello", b"ignored"), None);
        assert_eq!(parse_sentinel_event("+switch-master", b"incomplete"), None);
    }
}
