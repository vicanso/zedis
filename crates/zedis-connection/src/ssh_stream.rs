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

use russh::ChannelStream;
use russh::client::Msg;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// A Redis-compatible stream wrapper around an SSH channel.
///
/// This struct wraps an SSH `ChannelStream` and implements Tokio's `AsyncRead` and `AsyncWrite` traits,
/// making it compatible with the Redis client library. It enables Redis connections to be tunneled
/// through SSH by providing a transparent stream adapter.
pub struct SshRedisStream {
    /// The underlying SSH channel stream, pinned for safe async operations
    inner: Pin<Box<ChannelStream<Msg>>>,
}

impl SshRedisStream {
    /// Creates a new Redis-compatible stream from an SSH channel stream.
    ///
    /// # Arguments
    ///
    /// * `stream` - The SSH channel stream to wrap
    ///
    /// # Returns
    ///
    /// A new `SshRedisStream` instance ready for Redis communication
    pub fn new(stream: ChannelStream<Msg>) -> Self {
        Self {
            inner: Box::pin(stream),
        }
    }
}

impl AsyncRead for SshRedisStream {
    /// Attempts to read data from the SSH channel into the provided buffer.
    ///
    /// This delegates to the underlying SSH channel stream's read implementation.
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        AsyncRead::poll_read(self.inner.as_mut(), cx, buf)
    }
}

impl AsyncWrite for SshRedisStream {
    /// Attempts to write data to the SSH channel from the provided buffer.
    ///
    /// This delegates to the underlying SSH channel stream's write implementation.
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        AsyncWrite::poll_write(self.inner.as_mut(), cx, buf)
    }

    /// Attempts to flush any buffered data to the SSH channel.
    ///
    /// This ensures all written data is sent over the network.
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        AsyncWrite::poll_flush(self.inner.as_mut(), cx)
    }

    /// Attempts to gracefully shut down the SSH channel.
    ///
    /// This closes the stream and signals the end of communication.
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        AsyncWrite::poll_shutdown(self.inner.as_mut(), cx)
    }
}
