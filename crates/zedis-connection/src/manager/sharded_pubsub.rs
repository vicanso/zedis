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

//! Sharded Pub/Sub (`SSUBSCRIBE` / `SPUBLISH`, Redis 7+).
//!
//! Unlike classic Pub/Sub, sharded messages are not broadcast cluster-wide:
//! they are routed by the channel's hash slot, so a subscriber must sit on
//! the node that owns the slot. redis-rs only exposes that routing through
//! a RESP3 connection with a push sender (the RESP2 `aio::PubSub` used for
//! classic subscriptions has no `SSUBSCRIBE` support), hence this dedicated
//! wrapper: acks and `smessage` payloads arrive as [`PushInfo`] frames on a
//! smol channel, and dropping the wrapper closes the connection — which is
//! also the unsubscribe path, mirroring the Monitor/Pub/Sub task pattern.

use super::{ConnectionManager, ServerType};
use crate::async_connection::{resolve_connection_timeout, resolve_response_timeout};
use crate::error::Error;
use crate::ssh_cluster_connection::SshMultiplexedConnection;
use crate::ssh_tunnel::open_single_ssh_tunnel_push_connection;
use redis::{
    AsyncConnectionConfig, Client, IntoConnectionInfo, Msg, ProtocolVersion, PushInfo, PushKind,
    aio::MultiplexedConnection, cluster::ClusterClientBuilder, cluster_async::ClusterConnection, cmd,
};

type Result<T, E = Error> = std::result::Result<T, E>;

/// The dedicated connection backing a sharded subscription. Cluster arms
/// route `SSUBSCRIBE` to the slot owner and transparently resubscribe
/// after failovers; the single arm covers Standalone and the Sentinel
/// resolved master.
enum ShardedPubSubConn {
    Single(MultiplexedConnection),
    Cluster(Box<ClusterConnection>),
    SshCluster(Box<ClusterConnection<SshMultiplexedConnection>>),
}

/// A live sharded Pub/Sub subscription: subscribe with [`ssubscribe`],
/// then pull messages with [`recv`]. Drop to unsubscribe (the connection
/// closes with the value).
///
/// [`ssubscribe`]: ShardedPubSub::ssubscribe
/// [`recv`]: ShardedPubSub::recv
pub struct ShardedPubSub {
    conn: ShardedPubSubConn,
    rx: smol::channel::Receiver<PushInfo>,
}

impl ShardedPubSub {
    /// Subscribes to the given sharded channels (exact names — sharded
    /// Pub/Sub has no pattern variant).
    pub async fn ssubscribe(&mut self, channels: &[&str]) -> Result<()> {
        let mut c = cmd("SSUBSCRIBE");
        for channel in channels {
            c.arg(*channel);
        }
        match &mut self.conn {
            ShardedPubSubConn::Single(conn) => c.exec_async(conn).await?,
            ShardedPubSubConn::Cluster(conn) => c.exec_async(conn.as_mut()).await?,
            ShardedPubSubConn::SshCluster(conn) => c.exec_async(conn.as_mut()).await?,
        }
        Ok(())
    }

    /// Waits for the next `smessage`. Returns `None` once the connection
    /// is gone (the push channel closed). Non-message pushes (subscribe
    /// acks, invalidations) are skipped.
    pub async fn recv(&self) -> Option<Msg> {
        loop {
            let info = self.rx.recv().await.ok()?;
            if info.kind == PushKind::SMessage
                && let Some(msg) = Msg::from_push_info(info)
            {
                return Some(msg);
            }
        }
    }
}

impl ConnectionManager {
    /// Opens a dedicated sharded Pub/Sub connection for the server.
    /// Requires Redis 7+ (`SSUBSCRIBE` errors on older servers).
    pub async fn get_sharded_pubsub(&self, server_id: &str) -> Result<ShardedPubSub> {
        let (nodes, server_type) = self.get_redis_nodes(server_id).await?;
        let Some(first_node) = nodes.first() else {
            return Err(Error::Invalid {
                message: "no nodes found".to_string(),
            });
        };
        let (tx, rx) = smol::channel::unbounded::<PushInfo>();

        let conn = match server_type {
            ServerType::Cluster => {
                let addrs: Vec<String> = nodes.iter().map(|n| n.server.get_connection_url()).collect();
                let mut builder = ClusterClientBuilder::new(addrs)
                    .connection_timeout(resolve_connection_timeout(&first_node.server))
                    .response_timeout(resolve_response_timeout(&first_node.server))
                    .use_protocol(ProtocolVersion::RESP3)
                    .push_sender(move |info: PushInfo| tx.try_send(info));
                if let Some(certificates) = first_node.server.tls_certificates() {
                    builder = builder.certs(certificates);
                }
                if first_node.server.insecure.unwrap_or(false) {
                    builder = builder.danger_accept_invalid_hostnames(true);
                }
                if first_node.server.is_ssh_tunnel() {
                    builder = builder.username(server_id);
                    let client = builder.build()?;
                    ShardedPubSubConn::SshCluster(Box::new(client.get_async_generic_connection().await?))
                } else {
                    let client = builder.build()?;
                    ShardedPubSubConn::Cluster(Box::new(client.get_async_connection().await?))
                }
            }
            // Standalone, or the master a Sentinel resolved for us.
            _ => {
                let config = &first_node.server;
                let conn = if config.is_ssh_tunnel() {
                    open_single_ssh_tunnel_push_connection(config, tx).await?
                } else {
                    let info = config.get_connection_url().as_str().into_connection_info()?;
                    let redis_settings = info.redis_settings().clone().set_protocol(ProtocolVersion::RESP3);
                    let info = info.set_redis_settings(redis_settings);
                    let client = if let Some(certificates) = config.tls_certificates() {
                        Client::build_with_tls(info, certificates)?
                    } else {
                        Client::open(info)?
                    };
                    let cfg = AsyncConnectionConfig::new()
                        .set_connection_timeout(Some(resolve_connection_timeout(config)))
                        .set_response_timeout(Some(resolve_response_timeout(config)))
                        .set_push_sender(move |info: PushInfo| tx.try_send(info));
                    client.get_multiplexed_async_connection_with_config(&cfg).await?
                };
                ShardedPubSubConn::Single(conn)
            }
        };

        Ok(ShardedPubSub { conn, rx })
    }
}
