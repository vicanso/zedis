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

//! A connection that reads the server's replies itself, one frame at a time.
//!
//! Every other connection in this crate is redis-rs's multiplexed one, which
//! matches each reply to a request and drops any reply nobody asked for. A
//! protocol that answers one command with two replies — the Lua debugger,
//! whose last step is answered with its log *and* the script's result — needs
//! to ask for the second reply after it has seen the first. Here a request
//! is written as packed bytes and the replies are read as they come: the
//! frame is cut off the stream by hand and parsed by redis-rs.
//!
//! The same endpoints the multiplexed connection reaches: TCP, TLS (with the
//! entry's CA, client certificate and server name), a Unix socket, and the
//! entry's SSH tunnel. It speaks RESP2 and is logged in with `AUTH` and moved
//! to its database with `SELECT`. All socket work runs on the tokio runtime
//! (`run_in_tokio`), where the tunnel and TLS streams live.

use crate::async_connection::{client_name, resolve_connection_timeout, resolve_response_timeout};
use crate::config::RedisServer;
use crate::error::Error;
use crate::ssh_tunnel::{TunnelIo, open_tunnel_stream, run_in_tokio};
use crate::tls_connection::{build_tls_connector, tls_server_name};
use futures::lock::Mutex;
use redis::{Cmd, Value, cmd, parse_redis_value};
use rustls::pki_types::ServerName;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

type Result<T, E = Error> = std::result::Result<T, E>;

type Stream = BufReader<Box<dyn TunnelIo>>;

/// One connection, its replies read by hand. Cloning shares it; each call
/// waits for the one before it.
#[derive(Clone)]
pub(crate) struct RawConnection {
    stream: Arc<Mutex<Option<Stream>>>,
    response_timeout: Duration,
}

impl RawConnection {
    /// Dial `config`'s endpoint, log in and select `db`.
    pub(crate) async fn open(config: &RedisServer, db: usize) -> Result<Self> {
        let response_timeout = resolve_response_timeout(config);
        let dial = Dial::new(config)?;
        let handshake = handshake(config, db);
        let stream = run_in_tokio(async move {
            let mut stream = BufReader::new(dial.open().await?);
            tokio::time::timeout(response_timeout, greet(&mut stream, &handshake))
                .await
                .map_err(|_| timed_out(response_timeout))??;
            Ok::<_, Error>(stream)
        })
        .await?;
        Ok(Self {
            stream: Arc::new(Mutex::new(Some(stream))),
            response_timeout,
        })
    }

    /// Write `commands` in one go and read `replies` replies. An error
    /// reply is a value here (`Value::ServerError`), not a failure.
    pub(crate) async fn request(&self, commands: &[Cmd], replies: usize) -> Result<Vec<Value>> {
        let packed: Vec<u8> = commands.iter().flat_map(Cmd::get_packed_command).collect();
        self.exchange(packed, replies).await
    }

    /// Read one more reply without writing anything.
    pub(crate) async fn read_reply(&self) -> Result<Value> {
        let mut replies = self.exchange(Vec::new(), 1).await?;
        replies.pop().ok_or_else(|| Error::Invalid {
            message: "the server sent no reply".to_string(),
        })
    }

    /// Close the connection. Later calls fail.
    pub(crate) async fn close(&self) {
        self.stream.lock().await.take();
    }

    async fn exchange(&self, packed: Vec<u8>, replies: usize) -> Result<Vec<Value>> {
        let mut guard = self.stream.lock().await;
        let Some(stream) = guard.take() else {
            return Err(Error::Invalid {
                message: "the connection is closed".to_string(),
            });
        };
        let timeout = self.response_timeout;
        let (stream, result) = run_in_tokio(async move {
            let mut stream = stream;
            let result = tokio::time::timeout(timeout, async {
                if !packed.is_empty() {
                    stream.write_all(&packed).await?;
                    stream.flush().await?;
                }
                let mut values = Vec::with_capacity(replies);
                for _ in 0..replies {
                    values.push(read_value(&mut stream).await?);
                }
                Ok::<_, Error>(values)
            })
            .await
            .map_err(|_| timed_out(timeout))
            .and_then(|r| r);
            (stream, result)
        })
        .await;
        // A reply cut short leaves the stream out of step: drop it.
        if result.is_ok() {
            *guard = Some(stream);
        }
        result
    }
}

/// Where to dial, read off the entry before the work moves onto tokio.
enum Dial {
    Tunnel(Box<RedisServer>),
    Tcp {
        host: String,
        port: u16,
        timeout: Duration,
        tls: Option<(tokio_rustls::TlsConnector, ServerName<'static>)>,
    },
    #[cfg(unix)]
    Unix { path: String, timeout: Duration },
}

impl Dial {
    fn new(config: &RedisServer) -> Result<Self> {
        if config.is_ssh_tunnel() {
            return Ok(Dial::Tunnel(Box::new(config.clone())));
        }
        let timeout = resolve_connection_timeout(config);
        if config.is_unix_socket() {
            return unix_dial(config, timeout);
        }
        let (host, port) = config.primary_endpoint();
        let tls = if config.tls.unwrap_or(false) {
            let name = tls_server_name(config).unwrap_or_else(|| host.clone());
            let name = ServerName::try_from(name.clone())
                .map_err(|_| Error::Invalid {
                    message: format!("Invalid TLS server name: {name}"),
                })?
                .to_owned();
            Some((build_tls_connector(config)?, name))
        } else {
            None
        };
        Ok(Dial::Tcp {
            host,
            port,
            timeout,
            tls,
        })
    }

    async fn open(self) -> Result<Box<dyn TunnelIo>> {
        match self {
            Dial::Tunnel(config) => open_tunnel_stream(&config).await,
            Dial::Tcp {
                host,
                port,
                timeout,
                tls,
            } => {
                let tcp = tokio::time::timeout(timeout, tokio::net::TcpStream::connect((host.as_str(), port)))
                    .await
                    .map_err(|_| Error::Invalid {
                        message: format!("connect to {host}:{port} timed out"),
                    })??;
                let _ = tcp.set_nodelay(true);
                match tls {
                    None => Ok(Box::new(tcp)),
                    Some((connector, name)) => {
                        let stream = connector.connect(name, tcp).await.map_err(|e| Error::Invalid {
                            message: format!("TLS handshake failed: {e}"),
                        })?;
                        Ok(Box::new(stream))
                    }
                }
            }
            #[cfg(unix)]
            Dial::Unix { path, timeout } => {
                let stream = tokio::time::timeout(timeout, tokio::net::UnixStream::connect(&path))
                    .await
                    .map_err(|_| Error::Invalid {
                        message: format!("connect to {path} timed out"),
                    })??;
                Ok(Box::new(stream))
            }
        }
    }
}

#[cfg(unix)]
fn unix_dial(config: &RedisServer, timeout: Duration) -> Result<Dial> {
    Ok(Dial::Unix {
        path: config.host.clone(),
        timeout,
    })
}

#[cfg(not(unix))]
fn unix_dial(_config: &RedisServer, _timeout: Duration) -> Result<Dial> {
    Err(Error::Invalid {
        message: "Unix sockets are not available on this system".to_string(),
    })
}

/// `(command, must succeed)` for the log-in: `AUTH` and `SELECT` must, the
/// connection's name is a courtesy.
fn handshake(config: &RedisServer, db: usize) -> Vec<(Cmd, bool)> {
    let mut steps = Vec::with_capacity(3);
    if let Some(password) = config.password.as_deref().filter(|p| !p.is_empty()) {
        let mut auth = cmd("AUTH");
        if let Some(user) = config.username.as_deref().filter(|u| !u.trim().is_empty()) {
            auth.arg(user);
        }
        auth.arg(password);
        steps.push((auth, true));
    }
    let mut setname = cmd("CLIENT");
    setname.arg("SETNAME").arg(client_name());
    steps.push((setname, false));
    if db != 0 {
        let mut select = cmd("SELECT");
        select.arg(db);
        steps.push((select, true));
    }
    steps
}

async fn greet(stream: &mut Stream, steps: &[(Cmd, bool)]) -> Result<()> {
    let packed: Vec<u8> = steps.iter().flat_map(|(command, _)| command.get_packed_command()).collect();
    stream.write_all(&packed).await?;
    stream.flush().await?;
    for (_, required) in steps {
        let reply = read_value(stream).await?;
        if let Value::ServerError(error) = reply
            && *required
        {
            return Err(redis::RedisError::from(error).into());
        }
    }
    Ok(())
}

/// Read one whole reply off the stream and parse it.
async fn read_value(stream: &mut Stream) -> Result<Value> {
    let mut frame = Vec::new();
    // Frames still to read: an aggregate header adds its children.
    let mut pending: i64 = 1;
    while pending > 0 {
        pending -= 1;
        let start = frame.len();
        if stream.read_until(b'\n', &mut frame).await? == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the server closed the connection",
            )
            .into());
        }
        let header = &frame[start..];
        let length = || -> i64 {
            std::str::from_utf8(&header[1..])
                .ok()
                .and_then(|s| s.trim_end().parse().ok())
                .unwrap_or(-1)
        };
        match header.first() {
            // Bulk string, verbatim string, bulk error: a body follows.
            Some(b'$' | b'=' | b'!') => {
                let len = length();
                if len >= 0 {
                    let mut body = vec![0u8; len as usize + 2];
                    stream.read_exact(&mut body).await?;
                    frame.extend_from_slice(&body);
                }
            }
            Some(b'*' | b'~' | b'>') => pending += length().max(0),
            Some(b'%' | b'|') => pending += length().max(0) * 2,
            _ => {}
        }
    }
    Ok(parse_redis_value(&frame)?)
}

fn timed_out(timeout: Duration) -> Error {
    std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!("no reply within {}s", timeout.as_secs()),
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn read_all(bytes: &[u8], count: usize) -> Vec<Value> {
        let reader: Box<dyn TunnelIo> = Box::new(tokio_test_stream(bytes));
        let mut stream = BufReader::new(reader);
        let mut values = Vec::new();
        for _ in 0..count {
            values.push(read_value(&mut stream).await.expect("a reply"));
        }
        values
    }

    /// A duplex whose read side holds `bytes`.
    fn tokio_test_stream(bytes: &[u8]) -> tokio::io::DuplexStream {
        let (client, mut server) = tokio::io::duplex(64 * 1024);
        let bytes = bytes.to_vec();
        get_runtime().spawn(async move {
            let _ = server.write_all(&bytes).await;
            // Keep the write side open until the reader is done.
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        client
    }

    fn get_runtime() -> &'static tokio::runtime::Runtime {
        static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
        RT.get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("runtime")
        })
    }

    #[test]
    fn frames_are_cut_where_the_reply_ends() {
        // The last step of a debugger session as Redis sends it: the log,
        // then the script's reply, back to back in one packet.
        let bytes = b"*2\r\n+<debug> line 3: \"hello\", 1\r\n+<endsession>\r\n*2\r\n:1\r\n$1\r\nx\r\n";
        let values = get_runtime().block_on(read_all(bytes, 2));
        assert_eq!(
            values[0],
            Value::Array(vec![
                Value::SimpleString("<debug> line 3: \"hello\", 1".into()),
                Value::SimpleString("<endsession>".into()),
            ])
        );
        assert_eq!(
            values[1],
            Value::Array(vec![Value::Int(1), Value::BulkString(b"x".to_vec())])
        );
    }

    #[test]
    fn nested_nulls_and_errors_parse() {
        let bytes = b"*2\r\n$-1\r\n*2\r\n$3\r\na\r\n\r\n-ERR no\r\n-ERR whole reply\r\n";
        let values = get_runtime().block_on(read_all(bytes, 2));
        let Value::Array(items) = &values[0] else {
            panic!("an array: {:?}", values[0]);
        };
        assert_eq!(items[0], Value::Nil);
        let Value::Array(inner) = &items[1] else {
            panic!("a nested array: {:?}", items[1]);
        };
        assert_eq!(inner[0], Value::BulkString(b"a\r\n".to_vec()));
        assert!(matches!(inner[1], Value::ServerError(_)), "{:?}", inner[1]);
        assert!(matches!(values[1], Value::ServerError(_)), "{:?}", values[1]);
    }
}
