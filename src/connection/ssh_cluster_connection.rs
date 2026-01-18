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

use crate::connection::config::get_config;
use crate::connection::ssh_tunnel::open_single_ssh_tunnel_connection;
use redis::aio::{ConnectionLike, MultiplexedConnection};
use redis::cluster_async::Connect;
use redis::{AsyncConnectionConfig, IntoConnectionInfo};
use redis::{Cmd, ErrorKind, RedisError, RedisFuture, Value};

#[derive(Clone)]
pub struct SshMultiplexedConnection {
    inner: MultiplexedConnection,
}

impl ConnectionLike for SshMultiplexedConnection {
    fn req_packed_command<'a>(&'a mut self, cmd: &'a Cmd) -> RedisFuture<'a, Value> {
        self.inner.req_packed_command(cmd)
    }
    fn req_packed_commands<'a>(
        &'a mut self,
        pipeline: &'a redis::Pipeline,
        offset: usize,
        count: usize,
    ) -> RedisFuture<'a, Vec<Value>> {
        self.inner.req_packed_commands(pipeline, offset, count)
    }
    fn get_db(&self) -> i64 {
        0
    }
}

impl Connect for SshMultiplexedConnection {
    fn connect_with_config<'a, T>(info: T, _config: AsyncConnectionConfig) -> RedisFuture<'a, Self>
    where
        T: IntoConnectionInfo + Send + 'a,
    {
        // 019bc56a-c857-7df2-8996-71dd17b63212
        Box::pin(async move {
            let connection_info = info.into_connection_info()?;
            let id = connection_info.redis_settings().username().unwrap_or_default();
            let mut config =
                get_config(id).map_err(|e| (ErrorKind::InvalidClientConfig, "get_config", e.to_string()))?;
            let (target_host, target_port) = match connection_info.addr() {
                redis::ConnectionAddr::Tcp(h, p) => (h, p),
                redis::ConnectionAddr::TcpTls { host, port, .. } => (host, port),
                _ => {
                    return Err(RedisError::from((
                        ErrorKind::InvalidClientConfig,
                        "Ssh tunnel supports tcp only",
                    )));
                }
            };
            config.host = target_host.to_string();
            config.port = *target_port;
            let connection = open_single_ssh_tunnel_connection(&config).await.map_err(|e| {
                (
                    ErrorKind::InvalidClientConfig,
                    "open_single_ssh_tunnel_connection",
                    e.to_string(),
                )
            })?;

            Ok(SshMultiplexedConnection { inner: connection })
        })
    }
}
