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
use crate::conn::RedisAsyncConn;
use crate::error::Error;
use crate::reply::is_cross_slot;
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

/// Whether a `BITFIELD` argument list only reads (`GET` sub-commands without
/// `SET`, `INCRBY`, or `OVERFLOW`), so it can be sent via `BITFIELD_RO`.
fn bitfield_is_read_only(args: &[String]) -> bool {
    !args.is_empty()
        && !args.iter().any(|arg| {
            arg.eq_ignore_ascii_case("SET")
                || arg.eq_ignore_ascii_case("INCRBY")
                || arg.eq_ignore_ascii_case("OVERFLOW")
        })
}

/// `BITFIELD key <args>` (or `BITFIELD_RO` when `args` only reads and the
/// server supports Redis 6.0+) — the user's own sub-commands, as typed; the
/// reply is one integer per `GET` / `SET` / `INCRBY`.
///
/// One reply per sub-command; `None` where `OVERFLOW FAIL` stopped one — the
/// server answers that with a nil, which a list of integers could not hold,
/// so the whole command read as failed after it had written.
pub async fn bit_field(at: &ServerDb, key: &str, args: &[String]) -> Result<Vec<Option<i64>>> {
    let client = at.client().await?;
    let use_ro = bitfield_is_read_only(args) && client.supports(crate::floors::BITFIELD_RO);
    let mut command = if use_ro { cmd("BITFIELD_RO") } else { cmd("BITFIELD") };
    command.arg(key);
    for arg in args {
        command.arg(arg);
    }
    let mut conn = client.connection();
    Ok(command.query_async(&mut conn).await?)
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

/// Byte-for-byte bitwise combination matching Redis `BITOP` (shorter inputs
/// are zero-padded to the longest input's byte length).
fn combine_bitmaps(op: BitOpKind, inputs: &[Vec<u8>]) -> Vec<u8> {
    let max_len = inputs.iter().map(Vec::len).max().unwrap_or(0);
    if max_len == 0 {
        return Vec::new();
    }
    let mut out = vec![0u8; max_len];
    match op {
        BitOpKind::Not => {
            if let Some(first) = inputs.first() {
                for (i, byte) in out.iter_mut().enumerate() {
                    *byte = !first.get(i).copied().unwrap_or(0);
                }
            }
        }
        BitOpKind::And => {
            out.fill(0xFF);
            for input in inputs {
                for (i, byte) in out.iter_mut().enumerate() {
                    *byte &= input.get(i).copied().unwrap_or(0);
                }
            }
        }
        BitOpKind::Or => {
            for input in inputs {
                for (i, byte) in out.iter_mut().enumerate() {
                    *byte |= input.get(i).copied().unwrap_or(0);
                }
            }
        }
        BitOpKind::Xor => {
            for input in inputs {
                for (i, byte) in out.iter_mut().enumerate() {
                    *byte ^= input.get(i).copied().unwrap_or(0);
                }
            }
        }
    }
    out
}

/// Cross-slot fallback when `BITOP` is refused on a cluster because
/// `destination` and `sources` hash to different slots: reads each source
/// via `GET`, combines the bitmaps locally, and writes (or deletes when
/// empty) `destination`.
async fn bit_op_cross_slot(
    conn: &mut RedisAsyncConn,
    op: BitOpKind,
    destination: &str,
    sources: &[String],
) -> Result<u64> {
    let mut inputs = Vec::with_capacity(sources.len());
    for source in sources {
        let bytes: Option<Vec<u8>> = cmd("GET").arg(source).query_async(conn).await?;
        inputs.push(bytes.unwrap_or_default());
    }
    let combined = combine_bitmaps(op, &inputs);
    if combined.is_empty() {
        let _: () = cmd("DEL").arg(destination).query_async(conn).await?;
        return Ok(0);
    }
    let len = combined.len() as u64;
    let _: () = cmd("SET").arg(destination).arg(&combined).query_async(conn).await?;
    Ok(len)
}

/// `BITOP op destination source [source …]` — returns the destination's
/// length in bytes. On a cluster where `destination` and `sources` hash to
/// different slots (`CrossSlot`), falls back to per-key `GET` + bitwise
/// combination + `SET`.
pub async fn bit_op(at: &ServerDb, op: BitOpKind, destination: &str, sources: &[String]) -> Result<u64> {
    if sources.is_empty() || (op.single_source() && sources.len() != 1) {
        return Err(Error::Invalid {
            message: format!("{} takes exactly one source key", op.word()),
        });
    }
    let conn = &mut at.connection().await?;
    let mut command = cmd("BITOP");
    command.arg(op.word()).arg(destination);
    for source in sources {
        command.arg(source);
    }
    match command.query_async::<u64>(conn).await {
        Ok(len) => Ok(len),
        Err(err) if is_cross_slot(&err) => bit_op_cross_slot(conn, op, destination, sources).await,
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::{BitOpKind, combine_bitmaps};

    #[test]
    fn not_is_the_one_bitop_with_a_single_source() {
        assert!(BitOpKind::Not.single_source());
        for op in [BitOpKind::And, BitOpKind::Or, BitOpKind::Xor] {
            assert!(!op.single_source(), "{}", op.word());
        }
        assert_eq!(BitOpKind::Xor.word(), "XOR");
    }

    #[test]
    fn combine_bitmaps_pads_shorter_inputs_with_zero_bytes() {
        let a = vec![0b1100_0000, 0b1010_1010];
        let b = vec![0b1010_0000];
        assert_eq!(
            combine_bitmaps(BitOpKind::And, &[a.clone(), b.clone()]),
            vec![0b1000_0000, 0]
        );
        assert_eq!(
            combine_bitmaps(BitOpKind::Or, &[a.clone(), b.clone()]),
            vec![0b1110_0000, 0b1010_1010]
        );
        assert_eq!(
            combine_bitmaps(BitOpKind::Xor, &[a.clone(), b]),
            vec![0b0110_0000, 0b1010_1010]
        );
        assert_eq!(combine_bitmaps(BitOpKind::Not, &[a]), vec![0b0011_1111, 0b0101_0101]);
        assert!(combine_bitmaps(BitOpKind::Or, &[Vec::new(), Vec::new()]).is_empty());
    }
}
