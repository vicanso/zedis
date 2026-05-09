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

use super::async_connection::RedisAsyncConn;
use crate::error::Error;
use gpui::SharedString;
use redis::{Value, cmd};

type Result<T, E = Error> = std::result::Result<T, E>;

/// One Redis ACL user as returned by `ACL GETUSER`.
///
/// We project the raw map into typed fields, but keep the original `rules`
/// string so the editor round-trips an exact `ACL SETUSER` invocation when
/// the user edits a single line.
#[derive(Debug, Clone, Default)]
pub struct AclUser {
    pub username: SharedString,
    pub flags: Vec<SharedString>,
    /// Hex sha-256 prefixes of stored passwords, length-truncated for display.
    pub password_digests: Vec<SharedString>,
    /// `+@all -@dangerous +set ...` command spec.
    pub commands: SharedString,
    /// `~prefix:* %R~foo &chan:*` patterns flattened into one line.
    pub keys: Vec<SharedString>,
    pub channels: Vec<SharedString>,
    /// True iff the user has the `on` flag.
    pub enabled: bool,
    /// True iff the user has the `nopass` flag.
    pub nopass: bool,
}

impl AclUser {
    /// Compose the multi-rule string that `ACL SETUSER` accepts. This is
    /// what we feed back into the textarea editor; users are free to rewrite
    /// it line-by-line, and we send the whole thing on save.
    pub fn to_rules_text(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for flag in &self.flags {
            parts.push(flag.to_string());
        }
        if !self.commands.is_empty() {
            parts.push(self.commands.to_string());
        }
        for k in &self.keys {
            parts.push(k.to_string());
        }
        for c in &self.channels {
            parts.push(c.to_string());
        }
        parts.join(" ")
    }
}

/// Outcome of `ACL LIST`. `unsupported` is true when the server returned an
/// "unknown command" error — Redis < 6 or environments that disabled the
/// module. Callers render an empty-state explainer in that case.
#[derive(Debug, Clone, Default)]
pub struct AclListing {
    pub usernames: Vec<SharedString>,
    pub unsupported: bool,
}

pub async fn acl_list(conn: &mut RedisAsyncConn) -> Result<AclListing> {
    let res: redis::RedisResult<Vec<String>> = cmd("ACL").arg("USERS").query_async(conn).await;
    match res {
        Ok(users) => Ok(AclListing {
            usernames: users.into_iter().map(SharedString::from).collect(),
            unsupported: false,
        }),
        Err(e) => {
            if is_unsupported(&e) {
                Ok(AclListing {
                    unsupported: true,
                    ..Default::default()
                })
            } else {
                Err(e.into())
            }
        }
    }
}

pub async fn acl_get_user(conn: &mut RedisAsyncConn, username: &str) -> Result<AclUser> {
    let value: Value = cmd("ACL").arg("GETUSER").arg(username).query_async(conn).await?;
    parse_get_user(username, &value).ok_or_else(|| Error::Invalid {
        message: format!("ACL GETUSER {username} returned unexpected shape"),
    })
}

pub async fn acl_whoami(conn: &mut RedisAsyncConn) -> Result<SharedString> {
    let res: redis::RedisResult<String> = cmd("ACL").arg("WHOAMI").query_async(conn).await;
    match res {
        Ok(name) => Ok(name.into()),
        Err(e) if is_unsupported(&e) => Ok(SharedString::default()),
        Err(e) => Err(e.into()),
    }
}

pub async fn acl_set_user(conn: &mut RedisAsyncConn, username: &str, rules: &[String]) -> Result<()> {
    let mut c = cmd("ACL");
    c.arg("SETUSER").arg(username);
    for rule in rules {
        let trimmed = rule.trim();
        if !trimmed.is_empty() {
            c.arg(trimmed);
        }
    }
    let _: () = c.query_async(conn).await?;
    Ok(())
}

pub async fn acl_del_user(conn: &mut RedisAsyncConn, username: &str) -> Result<()> {
    let _: () = cmd("ACL").arg("DELUSER").arg(username).query_async(conn).await?;
    Ok(())
}

fn is_unsupported(err: &redis::RedisError) -> bool {
    let msg = err.to_string();
    msg.contains("unknown command") || msg.contains("ERR unknown") || msg.contains("not available")
}

/// Convert the redis-rs `Value` shape returned by `ACL GETUSER` (a flat
/// array of [key, value, key, value, ...]) into an `AclUser`.
///
/// Defensive: `GETUSER` keys vary across Redis versions (selectors landed in
/// 7.x), but the legacy keys we read here are stable. Unknown keys are
/// ignored rather than failing the parse.
fn parse_get_user(username: &str, value: &Value) -> Option<AclUser> {
    let entries = match value {
        Value::Array(_) | Value::Map(_) => extract_pairs(value)?,
        _ => return None,
    };

    let mut user = AclUser {
        username: SharedString::from(username.to_string()),
        ..Default::default()
    };

    for (key, val) in entries {
        match key.as_str() {
            "flags" => {
                let flags = parse_string_array(&val).unwrap_or_default();
                user.enabled = flags.iter().any(|f| f == "on");
                user.nopass = flags.iter().any(|f| f == "nopass");
                user.flags = flags
                    .into_iter()
                    .filter(|f| f != "on" && f != "off" && f != "nopass")
                    .map(SharedString::from)
                    .collect();
                if user.enabled {
                    user.flags.insert(0, "on".into());
                } else {
                    user.flags.insert(0, "off".into());
                }
                if user.nopass {
                    user.flags.push("nopass".into());
                }
            }
            "passwords" => {
                user.password_digests = parse_string_array(&val)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|s| {
                        let preview = s.chars().take(8).collect::<String>();
                        SharedString::from(format!("#{preview}…"))
                    })
                    .collect();
            }
            "commands" => {
                user.commands = parse_simple_string(&val).unwrap_or_default().into();
            }
            "keys" => {
                user.keys = parse_keys_or_channels(&val);
            }
            "channels" => {
                user.channels = parse_keys_or_channels(&val);
            }
            _ => {}
        }
    }
    Some(user)
}

fn extract_pairs(v: &Value) -> Option<Vec<(String, Value)>> {
    match v {
        Value::Array(items) => {
            // Redis 6 returns alternating key/value entries.
            let mut out = Vec::with_capacity(items.len() / 2);
            for pair in items.chunks(2) {
                if pair.len() != 2 {
                    return None;
                }
                let key = parse_simple_string(&pair[0])?;
                out.push((key, pair[1].clone()));
            }
            Some(out)
        }
        Value::Map(items) => Some(
            items
                .iter()
                .filter_map(|(k, v)| Some((parse_simple_string(k)?, v.clone())))
                .collect(),
        ),
        _ => None,
    }
}

fn parse_simple_string(v: &Value) -> Option<String> {
    match v {
        Value::SimpleString(s) | Value::VerbatimString { text: s, .. } => Some(s.clone()),
        Value::BulkString(bytes) => String::from_utf8(bytes.clone()).ok(),
        Value::Int(n) => Some(n.to_string()),
        _ => None,
    }
}

fn parse_string_array(v: &Value) -> Option<Vec<String>> {
    match v {
        Value::Array(items) => Some(items.iter().filter_map(parse_simple_string).collect()),
        _ => None,
    }
}

/// `keys` / `channels` may come as an `Array<BulkString>` (one pattern each)
/// or as a single space-joined `BulkString`. Normalize both into a token list.
fn parse_keys_or_channels(v: &Value) -> Vec<SharedString> {
    if let Some(items) = parse_string_array(v) {
        return items.into_iter().map(SharedString::from).collect();
    }
    if let Some(joined) = parse_simple_string(v) {
        return joined
            .split_whitespace()
            .map(|s| SharedString::from(s.to_string()))
            .collect();
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use redis::Value;

    fn bs(s: &str) -> Value {
        Value::BulkString(s.as_bytes().to_vec())
    }

    #[test]
    fn parse_basic_user() {
        let raw = Value::Array(vec![
            bs("flags"),
            Value::Array(vec![bs("on"), bs("allkeys")]),
            bs("passwords"),
            Value::Array(vec![bs("0123456789abcdef0000000000000000")]),
            bs("commands"),
            bs("+@all -@dangerous"),
            bs("keys"),
            Value::Array(vec![bs("~user:*"), bs("%R~ro:*")]),
            bs("channels"),
            Value::Array(vec![bs("&events:*")]),
        ]);
        let user = parse_get_user("alice", &raw).expect("parse failed");
        assert_eq!(user.username.as_ref(), "alice");
        assert!(user.enabled);
        assert_eq!(user.commands.as_ref(), "+@all -@dangerous");
        assert_eq!(user.keys.len(), 2);
        assert_eq!(user.channels.len(), 1);
        assert_eq!(user.password_digests.len(), 1);
        assert!(user.password_digests[0].starts_with("#01234567"));
    }

    #[test]
    fn keys_can_be_a_single_string() {
        let raw = Value::Array(vec![
            bs("flags"),
            Value::Array(vec![bs("off")]),
            bs("commands"),
            bs("-@all"),
            bs("keys"),
            bs("~user:* ~order:*"),
        ]);
        let user = parse_get_user("bob", &raw).expect("parse failed");
        assert!(!user.enabled);
        let expected: Vec<SharedString> = vec!["~user:*".into(), "~order:*".into()];
        assert_eq!(user.keys, expected);
    }

    #[test]
    fn rules_text_concatenates() {
        let user = AclUser {
            username: "alice".into(),
            flags: vec!["on".into()],
            commands: "+@read".into(),
            keys: vec!["~ro:*".into()],
            channels: vec!["&log:*".into()],
            ..Default::default()
        };
        assert_eq!(user.to_rules_text(), "on +@read ~ro:* &log:*");
    }
}
