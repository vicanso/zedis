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

//! The wire format, and the only part of this server that knows what RESP is.
//!
//! The bridge forwards opaque frames: a request arrives as the bytes
//! `Cmd::get_packed_command()` produced, and a reply leaves as bytes the
//! caller hands to `redis::parse_redis_value`. No Redis command is named
//! anywhere in this crate, which is what keeps the protocol from growing
//! when Zedis gains a feature (ADR 9).
//!
//! Two directions, and they are not symmetric. Inbound we only need the
//! argument list, because redis-rs rebuilds the command from it. Outbound
//! we have a parsed [`Value`] and have to put it back on the wire, so this
//! module carries a RESP3 encoder.

use redis::{Cmd, Value, VerbatimFormat};
use std::fmt::Write as _;

/// A frame that could not be understood. Both directions fail the request
/// rather than guess: a bridge that silently repairs a malformed frame would
/// turn a client bug into a wrong answer against real data.
#[derive(Debug)]
pub struct RespError(pub String);

impl std::fmt::Display for RespError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RespError {}

type Result<T> = std::result::Result<T, RespError>;

/// The argument list of a packed command, as raw bytes.
///
/// Bytes, not `String`: Redis keys and values are binary, and a lossy
/// conversion here would corrupt them on the way to the server. The policy
/// layer does its own lossy read for classification only.
pub fn decode_command(frame: &[u8]) -> Result<Vec<Vec<u8>>> {
    let value = redis::parse_redis_value(frame).map_err(|e| RespError(format!("not a RESP frame: {e}")))?;
    let Value::Array(items) = value else {
        return Err(RespError("a command must be a RESP array".to_string()));
    };
    if items.is_empty() {
        return Err(RespError("a command must have at least one argument".to_string()));
    }
    items
        .into_iter()
        .map(|item| match item {
            Value::BulkString(bytes) => Ok(bytes),
            Value::SimpleString(text) => Ok(text.into_bytes()),
            other => Err(RespError(format!(
                "a command argument must be a bulk string, got {other:?}"
            ))),
        })
        .collect()
}

/// Rebuild the command redis-rs will send from a decoded argument list.
pub fn command_from_args(args: &[Vec<u8>]) -> Cmd {
    let mut cmd = Cmd::new();
    for arg in args {
        cmd.arg(arg.as_slice());
    }
    cmd
}

/// Put a reply back on the wire so the caller can `parse_redis_value` it.
///
/// Every variant `redis::parse_redis_value` can read is written in the form
/// it reads. [`Value`] is `#[non_exhaustive]`, so a variant added by a future
/// redis-rs is an error here rather than a silently dropped reply.
pub fn encode(value: &Value, out: &mut Vec<u8>) -> Result<()> {
    match value {
        Value::Nil => out.extend_from_slice(b"_\r\n"),
        Value::Int(n) => line(out, b':', &n.to_string()),
        Value::BulkString(bytes) => {
            line(out, b'$', &bytes.len().to_string());
            out.extend_from_slice(bytes);
            out.extend_from_slice(b"\r\n");
        }
        Value::Array(items) => {
            line(out, b'*', &items.len().to_string());
            for item in items {
                encode(item, out)?;
            }
        }
        Value::SimpleString(text) => line(out, b'+', text),
        Value::Okay => out.extend_from_slice(b"+OK\r\n"),
        Value::Map(pairs) => {
            line(out, b'%', &pairs.len().to_string());
            for (key, val) in pairs {
                encode(key, out)?;
                encode(val, out)?;
            }
        }
        Value::Attribute { data, attributes } => {
            line(out, b'|', &attributes.len().to_string());
            for (key, val) in attributes {
                encode(key, out)?;
                encode(val, out)?;
            }
            encode(data, out)?;
        }
        Value::Set(items) => {
            line(out, b'~', &items.len().to_string());
            for item in items {
                encode(item, out)?;
            }
        }
        Value::Double(n) => line(out, b',', &format_double(*n)),
        Value::Boolean(b) => out.extend_from_slice(if *b { b"#t\r\n" } else { b"#f\r\n" }),
        Value::VerbatimString { format, text } => {
            let tag = match format {
                VerbatimFormat::Text => "txt",
                VerbatimFormat::Markdown => "mkd",
                VerbatimFormat::Unknown(other) => other.as_str(),
                // Also `#[non_exhaustive]`; refuse rather than mislabel.
                other => {
                    return Err(RespError(format!(
                        "this redis-rs has a verbatim format the bridge cannot encode: {other:?}"
                    )));
                }
            };
            // The blob length counts the `xxx:` prefix the parser splits off.
            line(out, b'=', &(tag.len() + 1 + text.len()).to_string());
            out.extend_from_slice(tag.as_bytes());
            out.push(b':');
            out.extend_from_slice(text.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        Value::BigNumber(n) => line(out, b'(', &n.to_string()),
        Value::Push { kind, data } => {
            // The parser reads the kind back as the first element.
            line(out, b'>', &(data.len() + 1).to_string());
            encode(&Value::BulkString(kind.to_string().into_bytes()), out)?;
            for item in data {
                encode(item, out)?;
            }
        }
        Value::ServerError(err) => {
            let mut text = err.code().to_string();
            if let Some(detail) = err.details() {
                text.push(' ');
                text.push_str(detail);
            }
            line(out, b'-', &text);
        }
        other => {
            return Err(RespError(format!(
                "this redis-rs has a reply kind the bridge cannot encode: {other:?}"
            )));
        }
    }
    Ok(())
}

/// [`encode`] into a fresh buffer.
pub fn encode_to_vec(value: &Value) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    encode(value, &mut out)?;
    Ok(out)
}

fn line(out: &mut Vec<u8>, marker: u8, body: &str) {
    out.push(marker);
    out.extend_from_slice(body.as_bytes());
    out.extend_from_slice(b"\r\n");
}

/// The parser reads a double with `str::parse::<f64>`, which accepts what
/// `{}` prints for infinities and NaN. An integral value still needs a
/// decimal point, or it would print as `3` and read back as `3` — the same
/// number, but the round-trip test below is stricter than it needs to be for
/// a reason: it is how a future formatting change gets caught.
fn format_double(n: f64) -> String {
    if n.is_finite() && n.fract() == 0.0 {
        let mut s = String::new();
        let _ = write!(s, "{n:.1}");
        s
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use redis::PushKind;

    fn round_trip(value: Value) {
        let bytes = encode_to_vec(&value).expect("encode");
        let parsed = redis::parse_redis_value(&bytes)
            .unwrap_or_else(|e| panic!("re-parse {value:?} from {:?}: {e}", String::from_utf8_lossy(&bytes)));
        assert_eq!(parsed, value, "wire form was {:?}", String::from_utf8_lossy(&bytes));
    }

    #[test]
    fn every_reply_kind_survives_the_wire() {
        round_trip(Value::Nil);
        round_trip(Value::Int(-42));
        round_trip(Value::BulkString(b"hello".to_vec()));
        round_trip(Value::BulkString(vec![]));
        round_trip(Value::SimpleString("PONG".to_string()));
        round_trip(Value::Okay);
        round_trip(Value::Boolean(true));
        round_trip(Value::Boolean(false));
        round_trip(Value::Double(1.5));
        round_trip(Value::Double(3.0));
        round_trip(Value::Array(vec![
            Value::Int(1),
            Value::BulkString(b"two".to_vec()),
            Value::Nil,
        ]));
        round_trip(Value::Array(vec![]));
        round_trip(Value::Set(vec![Value::BulkString(b"m".to_vec())]));
        round_trip(Value::Map(vec![(Value::BulkString(b"k".to_vec()), Value::Int(7))]));
        round_trip(Value::VerbatimString {
            format: VerbatimFormat::Text,
            text: "plain".to_string(),
        });
        round_trip(Value::VerbatimString {
            format: VerbatimFormat::Markdown,
            text: "# head".to_string(),
        });
        round_trip(Value::Push {
            kind: PushKind::Message,
            data: vec![
                Value::BulkString(b"chan".to_vec()),
                Value::BulkString(b"payload".to_vec()),
            ],
        });
    }

    #[test]
    fn binary_payloads_are_not_mangled() {
        let raw = vec![0u8, 0xff, b'\r', b'\n', 0x7f];
        round_trip(Value::BulkString(raw.clone()));
        round_trip(Value::Array(vec![Value::BulkString(raw)]));
    }

    #[test]
    fn nesting_survives() {
        round_trip(Value::Array(vec![
            Value::Array(vec![Value::Int(1), Value::Array(vec![Value::Okay])]),
            Value::Map(vec![(
                Value::BulkString(b"inner".to_vec()),
                Value::Set(vec![Value::Int(9)]),
            )]),
        ]));
    }

    #[test]
    fn a_packed_command_decodes_to_its_arguments() {
        let packed = Cmd::new()
            .arg("SET")
            .arg("key")
            .arg(&b"bin\x00ary"[..])
            .get_packed_command();
        let args = decode_command(&packed).expect("decode");
        assert_eq!(args, vec![b"SET".to_vec(), b"key".to_vec(), b"bin\x00ary".to_vec()]);
        // And rebuilding produces the identical frame, which is what makes
        // the bridge a passthrough rather than a rewriter.
        assert_eq!(command_from_args(&args).get_packed_command(), packed);
    }

    #[test]
    fn a_malformed_command_is_refused() {
        assert!(decode_command(b"+OK\r\n").is_err(), "a reply is not a command");
        assert!(decode_command(b"*0\r\n").is_err(), "an empty command has no name");
        assert!(decode_command(b"not resp at all").is_err());
    }
}
