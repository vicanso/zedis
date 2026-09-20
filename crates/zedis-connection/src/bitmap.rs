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

//! Bitmaps: what the bitmap viewer shows and does.

#[cfg(target_family = "wasm")]
use crate::bridge::BridgeQuery as _;
use crate::error::Error;
use crate::server_db::ServerDb;
use redis::cmd;

type Result<T, E = Error> = std::result::Result<T, E>;

/// A bitmap key as the viewer shows it: a window of its first bytes, and
/// whole-key statistics.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BitmapInfo {
    /// The first `window_bytes` of the value, for the grid.
    pub bytes: Vec<u8>,
    /// `STRLEN key * 8` — the full bit length.
    pub total_bits: u64,
    /// `BITCOUNT key` — set bits in the whole key.
    pub set_bits: i64,
    /// `BITPOS key 1` — the first set bit, or -1.
    pub first_set: i64,
    /// `BITPOS key 0` — the first clear bit, or -1.
    pub first_clear: i64,
    /// The key is longer than the window.
    pub truncated: bool,
}

impl BitmapInfo {
    /// How many bits the window holds — what the grid draws.
    pub fn rendered_bits(&self) -> usize {
        self.bytes.len() * 8
    }
}

/// The first `window_bytes` of `key` plus whole-key `BITCOUNT` / `BITPOS`.
/// Only `STRLEN` is fatal; the window and the statistics are best-effort.
pub async fn bitmap_info(at: &ServerDb, key: &str, window_bytes: usize) -> Result<BitmapInfo> {
    let mut conn = at.connection().await?;
    let len: i64 = cmd("STRLEN").arg(key).query_async(&mut conn).await?;
    let bytes: Vec<u8> = if len <= 0 || window_bytes == 0 {
        Vec::new()
    } else {
        let end = len.min(window_bytes as i64) - 1;
        cmd("GETRANGE")
            .arg(key)
            .arg(0)
            .arg(end)
            .query_async(&mut conn)
            .await
            .unwrap_or_default()
    };
    let set_bits: i64 = cmd("BITCOUNT").arg(key).query_async(&mut conn).await.unwrap_or(0);
    let first_set: i64 = cmd("BITPOS").arg(key).arg(1).query_async(&mut conn).await.unwrap_or(-1);
    let first_clear: i64 = cmd("BITPOS").arg(key).arg(0).query_async(&mut conn).await.unwrap_or(-1);
    let total_bits = (len.max(0) as u64) * 8;
    let truncated = total_bits > (bytes.len() * 8) as u64;
    Ok(BitmapInfo {
        bytes,
        total_bits,
        set_bits,
        first_set,
        first_clear,
        truncated,
    })
}

/// `SETBIT key offset value`. Returns the bit's previous value.
pub async fn set_bit(at: &ServerDb, key: &str, offset: u64, value: bool) -> Result<bool> {
    let previous: i64 = cmd("SETBIT")
        .arg(key)
        .arg(offset)
        .arg(i64::from(value))
        .query_async(&mut at.connection().await?)
        .await?;
    Ok(previous != 0)
}

/// `BITFIELD key <args>` — the user's own sub-commands, as typed; the reply is
/// one integer per `GET` / `SET` / `INCRBY`.
pub async fn bit_field(at: &ServerDb, key: &str, args: &[String]) -> Result<Vec<i64>> {
    let mut command = cmd("BITFIELD");
    command.arg(key);
    for arg in args {
        command.arg(arg);
    }
    Ok(command.query_async(&mut at.connection().await?).await?)
}

/// The bitwise operations `BITOP` accepts. `NOT` is the odd one out: it
/// takes exactly one source, and the server rejects more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitOpKind {
    And,
    Or,
    Xor,
    Not,
}

impl BitOpKind {
    pub const ALL: [BitOpKind; 4] = [BitOpKind::And, BitOpKind::Or, BitOpKind::Xor, BitOpKind::Not];

    pub const fn word(self) -> &'static str {
        match self {
            BitOpKind::And => "AND",
            BitOpKind::Or => "OR",
            BitOpKind::Xor => "XOR",
            BitOpKind::Not => "NOT",
        }
    }

    /// `NOT` inverts a single bitmap; the rest combine any number.
    pub const fn single_source(self) -> bool {
        matches!(self, BitOpKind::Not)
    }
}

/// `BITOP op destination source [source …]` — returns the destination's
/// length in bytes.
pub async fn bit_op(at: &ServerDb, op: BitOpKind, destination: &str, sources: &[String]) -> Result<u64> {
    if sources.is_empty() || (op.single_source() && sources.len() != 1) {
        return Err(Error::Invalid {
            message: format!("{} takes exactly one source key", op.word()),
        });
    }
    let mut command = cmd("BITOP");
    command.arg(op.word()).arg(destination);
    for source in sources {
        command.arg(source);
    }
    Ok(command.query_async(&mut at.connection().await?).await?)
}

#[cfg(test)]
mod tests {
    use super::BitOpKind;

    #[test]
    fn not_is_the_one_bitop_with_a_single_source() {
        assert!(BitOpKind::Not.single_source());
        for op in [BitOpKind::And, BitOpKind::Or, BitOpKind::Xor] {
            assert!(!op.single_source(), "{}", op.word());
        }
        assert_eq!(BitOpKind::Xor.word(), "XOR");
    }
}
