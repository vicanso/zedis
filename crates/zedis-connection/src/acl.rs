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
use redis::{Value, cmd};

type Result<T, E = Error> = std::result::Result<T, E>;

/// One additional permission group of an ACL v2 user (Redis 7.0
/// "selectors"): an independent (commands, keys, channels) tuple. A command
/// runs if the root permissions *or* any selector allow it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AclSelector {
    /// `-@all +lpush …` command spec.
    pub commands: String,
    pub keys: Vec<String>,
    pub channels: Vec<String>,
}

impl AclSelector {
    /// The selector as the `( … )` rule group `ACL SETUSER` accepts — one
    /// single argument, spaces included.
    pub fn to_rule_token(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if !self.commands.is_empty() {
            parts.push(self.commands.as_str());
        }
        for k in &self.keys {
            parts.push(k.as_str());
        }
        for c in &self.channels {
            parts.push(c.as_str());
        }
        format!("({})", parts.join(" "))
    }
}

/// One Redis ACL user as returned by `ACL GETUSER`.
///
/// We project the raw map into typed fields, but keep the original `rules`
/// string so the editor round-trips an exact `ACL SETUSER` invocation when
/// the user edits a single line.
#[derive(Debug, Clone, Default)]
pub struct AclUser {
    pub username: String,
    pub flags: Vec<String>,
    /// Hex sha-256 prefixes of stored passwords, length-truncated for display.
    pub password_digests: Vec<String>,
    /// `+@all -@dangerous +set ...` command spec.
    pub commands: String,
    /// `~prefix:* %R~foo &chan:*` patterns flattened into one line.
    pub keys: Vec<String>,
    pub channels: Vec<String>,
    /// ACL v2 selectors — additional independent permission groups. Empty
    /// on Redis < 7.0 (the `GETUSER` field doesn't exist there).
    pub selectors: Vec<AclSelector>,
    /// True iff the user has the `on` flag.
    pub enabled: bool,
    /// True iff the user has the `nopass` flag.
    pub nopass: bool,
}

impl AclUser {
    /// Compose the multi-rule string that `ACL SETUSER` accepts. This is
    /// what we feed back into the textarea editor; users are free to rewrite
    /// it line-by-line, and we send the whole thing on save. Selectors ride
    /// along as `( … )` groups — see [`split_acl_rules`] for why they must
    /// survive tokenization as single arguments.
    ///
    /// Unlike the root rules, `( … )` groups are **append** operations —
    /// re-applying this text would duplicate every selector on each save
    /// (verified live on 8.6.1). `clearselectors` in front of the groups
    /// makes the text authoritative for selectors: what you see is exactly
    /// what the user ends up with, save after save.
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
        if !self.selectors.is_empty() {
            parts.push("clearselectors".to_string());
            for selector in &self.selectors {
                parts.push(selector.to_rule_token());
            }
        }
        parts.join(" ")
    }
}

/// Split a rules line into `ACL SETUSER` arguments: whitespace-separated
/// tokens, except a `( … )` selector group — which contains spaces but must
/// reach the server as **one** argument. Selectors don't nest. An unclosed
/// `(` keeps the rest of the line glued to it, so the server's own syntax
/// error names the real problem instead of a mangled fragment.
pub fn split_acl_rules(text: &str) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let mut group: Option<String> = None;
    for token in text.split_whitespace() {
        if let Some(acc) = group.as_mut() {
            acc.push(' ');
            acc.push_str(token);
            if token.ends_with(')')
                && let Some(done) = group.take()
            {
                args.push(done);
            }
            continue;
        }
        if token.starts_with('(') && !token.ends_with(')') {
            group = Some(token.to_string());
        } else {
            args.push(token.to_string());
        }
    }
    if let Some(unclosed) = group {
        args.push(unclosed);
    }
    args
}

/// Outcome of `ACL LIST`. `unsupported` is true when the server returned an
/// "unknown command" error — Redis < 6 or environments that disabled the
/// module. Callers render an empty-state explainer in that case.
#[derive(Debug, Clone, Default)]
pub struct AclListing {
    pub usernames: Vec<String>,
    pub unsupported: bool,
}

pub async fn acl_list(conn: &mut RedisAsyncConn) -> Result<AclListing> {
    let res: redis::RedisResult<Vec<String>> = cmd("ACL").arg("USERS").query_async(conn).await;
    match res {
        Ok(users) => Ok(AclListing {
            usernames: users.into_iter().collect(),
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

pub async fn acl_whoami(conn: &mut RedisAsyncConn) -> Result<String> {
    let res: redis::RedisResult<String> = cmd("ACL").arg("WHOAMI").query_async(conn).await;
    match res {
        Ok(name) => Ok(name),
        Err(e) if is_unsupported(&e) => Ok(String::new()),
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

/// One `ACL LOG` entry: a command, key, channel or authentication the
/// server refused. This is where a `NOPERM` an application hit shows up
/// with the rule that produced it, which is the question an ACL editor
/// cannot answer on its own.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AclLogEntry {
    /// How often this same event repeated before the server reported it.
    pub count: u64,
    /// What was refused: `command`, `key`, `channel` or `auth`.
    pub reason: String,
    /// Where the call came from: `toplevel`, `multi` or `lua`.
    pub context: String,
    /// The command name, key pattern or channel that was refused.
    pub object: String,
    pub username: String,
    /// Seconds since the event was last seen.
    pub age_seconds: f64,
    /// `CLIENT INFO` of the connection that tripped it.
    pub client_info: String,
    /// Redis 7.0+ fields; `0` on a server that predates them.
    pub entry_id: u64,
    /// Unix **milliseconds**, Redis 7.0+.
    pub timestamp_created: i64,
    pub timestamp_last_updated: i64,
}

/// The `count` newest entries of `ACL LOG`, newest first (the server's own
/// order).
pub async fn acl_log(conn: &mut RedisAsyncConn, count: u64) -> Result<Vec<AclLogEntry>> {
    let value: Value = cmd("ACL").arg("LOG").arg(count).query_async(conn).await?;
    let Value::Array(items) = value else {
        return Ok(Vec::new());
    };
    // A malformed entry is skipped, never fatal: the log is diagnostic, and
    // one unreadable row must not hide the rest.
    Ok(items.iter().filter_map(parse_log_entry).collect())
}

/// `ACL LOG RESET` — drop every recorded event.
pub async fn acl_log_reset(conn: &mut RedisAsyncConn) -> Result<()> {
    let _: () = cmd("ACL").arg("LOG").arg("RESET").query_async(conn).await?;
    Ok(())
}

/// `ACL SAVE` — write the running ACL to the server's `aclfile`. Errors
/// when the server keeps its users in the config file instead, which
/// [`acl_file`] tells the caller in advance.
pub async fn acl_save(conn: &mut RedisAsyncConn) -> Result<()> {
    let _: () = cmd("ACL").arg("SAVE").query_async(conn).await?;
    Ok(())
}

/// `ACL LOAD` — replace the running ACL with the `aclfile`'s contents,
/// discarding every runtime change.
pub async fn acl_load(conn: &mut RedisAsyncConn) -> Result<()> {
    let _: () = cmd("ACL").arg("LOAD").query_async(conn).await?;
    Ok(())
}

/// `ACL GENPASS [bits]` — a password from the server's CSPRNG, hex encoded.
pub async fn acl_genpass(conn: &mut RedisAsyncConn, bits: Option<u32>) -> Result<String> {
    let mut c = cmd("ACL");
    c.arg("GENPASS");
    if let Some(bits) = bits {
        c.arg(bits);
    }
    Ok(c.query_async(conn).await?)
}

/// The server's `aclfile`, or `None` when it keeps its users in the main
/// config — which is what decides whether `ACL SAVE` / `LOAD` mean
/// anything at all. A server that will not answer `CONFIG GET` is also
/// `None`: better to hide the two buttons than to offer ones that error.
pub async fn acl_file(conn: &mut RedisAsyncConn) -> Result<Option<String>> {
    let res: redis::RedisResult<Vec<String>> = cmd("CONFIG").arg("GET").arg("aclfile").query_async(conn).await;
    let Ok(pair) = res else {
        return Ok(None);
    };
    Ok(pair.get(1).filter(|path| !path.is_empty()).cloned())
}

/// What `ACL DRYRUN` said about one user running one command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AclDryRun {
    /// The user may run it.
    Allowed,
    /// The server's own explanation of what the user is missing.
    Denied(String),
}

/// `ACL DRYRUN username command [arg …]` — would this user be allowed to
/// run this, without running it. `args` is the command and its arguments,
/// already split. Redis 7.0+ (see `floors::ACL_V2`).
pub async fn acl_dryrun(conn: &mut RedisAsyncConn, username: &str, args: &[String]) -> Result<AclDryRun> {
    if args.is_empty() {
        return Err(Error::Invalid {
            message: "ACL DRYRUN needs a command to test".to_string(),
        });
    }
    let mut c = cmd("ACL");
    c.arg("DRYRUN").arg(username);
    for arg in args {
        c.arg(arg.as_str());
    }
    let value: Value = c.query_async(conn).await?;
    // `OK` means allowed; anything else is the server spelling out why not.
    Ok(match &value {
        Value::Okay => AclDryRun::Allowed,
        _ => match parse_simple_string(&value) {
            Some(text) if text == "OK" => AclDryRun::Allowed,
            Some(text) => AclDryRun::Denied(text),
            None => AclDryRun::Denied(String::new()),
        },
    })
}

/// One `ACL LOG` entry — the same flat-array-or-map shape as `GETUSER`,
/// with fields that only exist from Redis 7.0 left at zero on older
/// servers.
fn parse_log_entry(value: &Value) -> Option<AclLogEntry> {
    let mut entry = AclLogEntry::default();
    for (key, val) in extract_pairs(value)? {
        match key.as_str() {
            "count" => entry.count = parse_u64(&val),
            "reason" => entry.reason = parse_simple_string(&val).unwrap_or_default(),
            "context" => entry.context = parse_simple_string(&val).unwrap_or_default(),
            "object" => entry.object = parse_simple_string(&val).unwrap_or_default(),
            "username" => entry.username = parse_simple_string(&val).unwrap_or_default(),
            "age-seconds" => entry.age_seconds = parse_f64(&val),
            "client-info" => entry.client_info = parse_simple_string(&val).unwrap_or_default(),
            "entry-id" => entry.entry_id = parse_u64(&val),
            "timestamp-created" => entry.timestamp_created = parse_u64(&val) as i64,
            "timestamp-last-updated" => entry.timestamp_last_updated = parse_u64(&val) as i64,
            _ => {}
        }
    }
    Some(entry)
}

fn parse_u64(v: &Value) -> u64 {
    match v {
        Value::Int(n) => (*n).max(0) as u64,
        _ => parse_simple_string(v).and_then(|s| s.parse().ok()).unwrap_or(0),
    }
}

/// `age-seconds` comes as a bulk string with decimals ("12.345").
fn parse_f64(v: &Value) -> f64 {
    match v {
        Value::Int(n) => *n as f64,
        Value::Double(d) => *d,
        _ => parse_simple_string(v).and_then(|s| s.parse().ok()).unwrap_or(0.0),
    }
}

fn is_unsupported(err: &redis::RedisError) -> bool {
    let msg = err.to_string();
    msg.contains("unknown command") || msg.contains("ERR unknown") || msg.contains("not available")
}

/// Convert the redis-rs `Value` shape returned by `ACL GETUSER` (a flat
/// array of [key, value, key, value, ...]) into an `AclUser`.
///
/// Defensive: `GETUSER` keys vary across Redis versions (`selectors` exists
/// only on 7.0+), so unknown keys are ignored rather than failing the parse.
fn parse_get_user(username: &str, value: &Value) -> Option<AclUser> {
    let entries = match value {
        Value::Array(_) | Value::Map(_) => extract_pairs(value)?,
        _ => return None,
    };

    let mut user = AclUser {
        username: username.to_string(),
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
                        format!("#{preview}…")
                    })
                    .collect();
            }
            "commands" => {
                user.commands = parse_simple_string(&val).unwrap_or_default();
            }
            "keys" => {
                user.keys = parse_keys_or_channels(&val);
            }
            "channels" => {
                user.channels = parse_keys_or_channels(&val);
            }
            "selectors" => {
                user.selectors = parse_selectors(&val);
            }
            _ => {}
        }
    }
    Some(user)
}

/// The `selectors` field (Redis 7.0+): an array of per-selector maps, each
/// shaped like a miniature `GETUSER` reply (commands / keys / channels).
/// Malformed entries are skipped, never fatal.
fn parse_selectors(v: &Value) -> Vec<AclSelector> {
    let Value::Array(items) = v else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let pairs = extract_pairs(item)?;
            let mut selector = AclSelector::default();
            for (key, val) in pairs {
                match key.as_str() {
                    "commands" => selector.commands = parse_simple_string(&val).unwrap_or_default(),
                    "keys" => selector.keys = parse_keys_or_channels(&val),
                    "channels" => selector.channels = parse_keys_or_channels(&val),
                    _ => {}
                }
            }
            Some(selector)
        })
        .collect()
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
fn parse_keys_or_channels(v: &Value) -> Vec<String> {
    if let Some(items) = parse_string_array(v) {
        return items.into_iter().collect();
    }
    if let Some(joined) = parse_simple_string(v) {
        return joined.split_whitespace().map(|s| s.to_string()).collect();
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bulk(text: &str) -> Value {
        Value::BulkString(text.as_bytes().to_vec())
    }

    /// A Redis 7 entry, in the flat array RESP2 delivers.
    #[test]
    fn a_log_entry_reads_every_field() {
        let entry = Value::Array(vec![
            bulk("count"),
            Value::Int(3),
            bulk("reason"),
            bulk("command"),
            bulk("context"),
            bulk("toplevel"),
            bulk("object"),
            bulk("get"),
            bulk("username"),
            bulk("worker"),
            bulk("age-seconds"),
            bulk("12.345"),
            bulk("client-info"),
            bulk("id=7 addr=127.0.0.1:6379"),
            bulk("entry-id"),
            Value::Int(41),
            bulk("timestamp-created"),
            Value::Int(1_700_000_000_000),
            bulk("timestamp-last-updated"),
            Value::Int(1_700_000_001_000),
        ]);
        let parsed = parse_log_entry(&entry).expect("entry parses");
        assert_eq!(
            parsed,
            AclLogEntry {
                count: 3,
                reason: "command".into(),
                context: "toplevel".into(),
                object: "get".into(),
                username: "worker".into(),
                age_seconds: 12.345,
                client_info: "id=7 addr=127.0.0.1:6379".into(),
                entry_id: 41,
                timestamp_created: 1_700_000_000_000,
                timestamp_last_updated: 1_700_000_001_000,
            }
        );
    }

    /// Redis 6 has no `entry-id` / `timestamp-*`: those stay zero instead of
    /// failing the parse.
    #[test]
    fn a_redis_6_entry_parses_without_the_7_0_fields() {
        let entry = Value::Array(vec![
            bulk("count"),
            Value::Int(1),
            bulk("reason"),
            bulk("auth"),
            bulk("context"),
            bulk("toplevel"),
            bulk("object"),
            bulk("AUTH"),
            bulk("username"),
            bulk("default"),
            bulk("age-seconds"),
            bulk("0.5"),
            bulk("client-info"),
            bulk("id=1"),
        ]);
        let parsed = parse_log_entry(&entry).expect("entry parses");
        assert_eq!(parsed.reason, "auth");
        assert_eq!(parsed.entry_id, 0);
        assert_eq!(parsed.timestamp_created, 0);
    }

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
        assert_eq!(user.username.as_str(), "alice");
        assert!(user.enabled);
        assert_eq!(user.commands.as_str(), "+@all -@dangerous");
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
        let expected: Vec<String> = vec!["~user:*".into(), "~order:*".into()];
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

    #[test]
    fn selectors_parse_and_round_trip_as_groups() {
        // Shape captured live from 8.6.1:
        // `ACL SETUSER app on ~app:* +@read (+lpush ~queue:*)`.
        let raw = Value::Array(vec![
            bs("flags"),
            Value::Array(vec![bs("on")]),
            bs("commands"),
            bs("-@all +@read"),
            bs("keys"),
            bs("~app:*"),
            bs("channels"),
            bs(""),
            bs("selectors"),
            Value::Array(vec![Value::Array(vec![
                bs("commands"),
                bs("-@all +lpush"),
                bs("keys"),
                bs("~queue:*"),
                bs("channels"),
                bs(""),
            ])]),
        ]);
        let user = parse_get_user("app", &raw).expect("parse failed");
        assert_eq!(
            user.selectors,
            vec![AclSelector {
                commands: "-@all +lpush".into(),
                keys: vec!["~queue:*".into()],
                channels: Vec::new(),
            }]
        );
        assert_eq!(user.selectors[0].to_rule_token(), "(-@all +lpush ~queue:*)");
        // `clearselectors` precedes the groups: `( … )` appends server-side,
        // so without it every save would duplicate the selectors.
        assert_eq!(
            user.to_rules_text(),
            "on -@all +@read ~app:* clearselectors (-@all +lpush ~queue:*)"
        );
    }

    #[test]
    fn rule_splitting_keeps_selector_groups_whole() {
        assert_eq!(
            split_acl_rules("on +@read ~app:* (-@all +lpush ~queue:*) &log:*"),
            vec!["on", "+@read", "~app:*", "(-@all +lpush ~queue:*)", "&log:*"]
        );
        // A one-token group and adjacent groups stay intact.
        assert_eq!(
            split_acl_rules("(allkeys) (+get ~a:*) (+set ~b:*)"),
            vec!["(allkeys)", "(+get ~a:*)", "(+set ~b:*)"]
        );
        // Unclosed group: glued together so the server names the problem.
        assert_eq!(split_acl_rules("on (+get ~a:*"), vec!["on", "(+get ~a:*"]);
        assert!(split_acl_rules("  ").is_empty());
    }
}
