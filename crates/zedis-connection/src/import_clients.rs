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

//! Connection importers for other Redis GUIs, so switching to Zedis
//! doesn't mean re-typing every host by hand.
//!
//! * **Another Redis Desktop Manager** — its Settings → Export writes
//!   `connections.ano`: base64-wrapped JSON of
//!   `{"connections": [...], "groups": [...]}`. base64 is only an
//!   envelope, not encryption, so passwords sit in plaintext inside.
//! * **Tiny RDM** — its export is a **zip** containing `connections.yaml`;
//!   we take that extracted YAML (a tree of connections and `type: group`
//!   folders). Parsed by the small reader in [`yaml`] rather than pulling
//!   in a YAML crate for one machine-generated file.
//!
//! Both formats store secrets in the clear; once imported they land in
//! Zedis's per-machine encrypted store, so importing is a step up.

use super::config::{ImportError, RedisServer, SERVER_TYPE_CLUSTER, SERVER_TYPE_SENTINEL};

/// Trim a string field into `Option`, dropping empties.
fn opt(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// `host:port` for the SSH tunnel address; bare host when no port.
fn ssh_addr(host: &str, port: Option<i64>) -> Option<String> {
    let host = host.trim();
    if host.is_empty() {
        return None;
    }
    match port {
        Some(p) if p > 0 => Some(format!("{host}:{p}")),
        _ => Some(host.to_string()),
    }
}

// ─── Another Redis Desktop Manager ───────────────────────────────────────────

/// Detect and convert an ARDM export. Accepts the raw `connections.ano`
/// text (base64) or the already-decoded JSON. Returns `Ok(None)` when the
/// input is not an ARDM payload so the caller can try the next format.
pub fn try_ardm_import(input: &str) -> Result<Option<Vec<RedisServer>>, ImportError> {
    let trimmed = input.trim();
    // Either the decoded JSON directly, or the base64 envelope.
    let decoded;
    let json = if trimmed.starts_with('{') {
        trimmed
    } else {
        use base64::Engine;
        // `connections.ano` may carry line breaks from a copy-paste.
        let compact: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(compact.as_bytes()) else {
            return Ok(None);
        };
        let Ok(text) = String::from_utf8(bytes) else {
            return Ok(None);
        };
        decoded = text;
        decoded.trim()
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return Ok(None);
    };
    // The distinguishing marker: a `connections` array of objects each
    // carrying a `host`. Zedis / Redis Insight payloads never nest that way.
    let Some(connections) = value.get("connections").and_then(|c| c.as_array()) else {
        return Ok(None);
    };
    if !connections.iter().any(|c| c.get("host").is_some()) {
        return Ok(None);
    }

    // groups: [{id, name}] — map a connection's groupId onto the group name.
    let group_names: std::collections::HashMap<String, String> = value
        .get("groups")
        .and_then(|g| g.as_array())
        .map(|groups| {
            groups
                .iter()
                .filter_map(|g| {
                    let id = g.get("id").map(json_scalar_to_string)?;
                    let name = g.get("name").and_then(|n| n.as_str())?;
                    Some((id, name.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();

    let mut servers = Vec::with_capacity(connections.len());
    for conn in connections {
        servers.push(ardm_connection(conn, &group_names)?);
    }
    if servers.is_empty() {
        return Err(ImportError::EmptyRedisInsight);
    }
    Ok(Some(servers))
}

/// ARDM ids may be numbers or strings; normalize for the group lookup.
fn json_scalar_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn ardm_connection(
    conn: &serde_json::Value,
    group_names: &std::collections::HashMap<String, String>,
) -> Result<RedisServer, ImportError> {
    let get_str = |key: &str| conn.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let host = get_str("host");
    if host.trim().is_empty() {
        return Err(ImportError::MissingHost);
    }
    let port = conn
        .get("port")
        .and_then(|p| p.as_u64().or_else(|| p.as_str().and_then(|s| s.parse().ok())))
        .filter(|p| *p > 0 && *p <= u16::MAX as u64)
        .ok_or(ImportError::InvalidPort)? as u16;
    // ARDM names a connection with `name`; fall back to host:port.
    let name = match opt(&get_str("name")) {
        Some(name) => name,
        None => format!("{host}:{port}"),
    };

    let mut server = RedisServer {
        name,
        host,
        port,
        username: opt(&get_str("username")),
        // ARDM calls the Redis password `auth`.
        password: opt(&get_str("auth")),
        key_separator: opt(&get_str("separator")),
        readonly: conn.get("connectionReadOnly").and_then(|v| v.as_bool()).filter(|v| *v),
        group: conn
            .get("groupId")
            .map(json_scalar_to_string)
            .and_then(|id| group_names.get(&id).cloned()),
        ..Default::default()
    };
    if conn.get("cluster").and_then(|v| v.as_bool()).unwrap_or(false) {
        server.server_type = Some(SERVER_TYPE_CLUSTER);
    }
    if let Some(sentinel) = conn.get("sentinelOptions").filter(|v| v.is_object()) {
        server.server_type = Some(SERVER_TYPE_SENTINEL);
        server.master_name = sentinel.get("masterName").and_then(|v| v.as_str()).and_then(opt);
        // ARDM's top-level `auth` is what the *sentinel* takes; the master's
        // own login lives in the sentinel options. Only when it is there do
        // the two sets part ways.
        let node_username = sentinel.get("nodeUsername").and_then(|v| v.as_str()).and_then(opt);
        let node_password = sentinel.get("nodePassword").and_then(|v| v.as_str()).and_then(opt);
        if node_username.is_some() || node_password.is_some() {
            server.sentinel_username = server.username.take();
            server.sentinel_password = server.password.take();
            server.username = node_username;
            server.password = node_password;
        }
    }
    if let Some(ssh) = conn.get("sshOptions").filter(|v| v.is_object()) {
        let ssh_str = |key: &str| ssh.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let port = ssh.get("port").and_then(|v| v.as_i64());
        if let Some(addr) = ssh_addr(&ssh_str("host"), port) {
            server.ssh_tunnel = Some(true);
            server.ssh_addr = Some(addr);
            server.ssh_username = opt(&ssh_str("username"));
            server.ssh_password = opt(&ssh_str("password"));
            server.ssh_key = opt(&ssh_str("privatekey"));
            server.ssh_key_passphrase = opt(&ssh_str("passphrase"));
        }
    }
    if let Some(ssl) = conn.get("sslOptions").filter(|v| v.is_object()) {
        let ssl_str = |key: &str| ssl.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_string();
        server.tls = Some(true);
        server.client_key = opt(&ssl_str("key"));
        server.client_cert = opt(&ssl_str("cert"));
        server.root_cert = opt(&ssl_str("ca"));
    }
    RedisServer::finalize_import(server)
}

// ─── Tiny RDM ────────────────────────────────────────────────────────────────

/// Detect and convert a Tiny RDM `connections.yaml` (the file inside its
/// exported zip, or straight from its config dir). Group folders
/// (`type: group`) are flattened: children keep the folder as their Zedis
/// group. Returns `Ok(None)` when the text isn't a Tiny RDM payload.
pub fn try_tinyrdm_import(input: &str) -> Result<Option<Vec<RedisServer>>, ImportError> {
    let trimmed = input.trim();
    // Cheap pre-check: a YAML sequence whose items look like connections.
    if !trimmed.starts_with('-') {
        return Ok(None);
    }
    let Some(yaml::Yaml::Seq(items)) = yaml::parse(trimmed) else {
        return Ok(None);
    };
    // Require the Tiny RDM shape (a named entry with an addr or a group)
    // so a foreign YAML list doesn't get mangled into servers.
    let recognized = items.iter().any(|item| {
        item.get("name").is_some() && (item.get("addr").is_some() || item.get_str("type") == Some("group"))
    });
    if !recognized {
        return Ok(None);
    }

    let mut servers = Vec::new();
    collect_tinyrdm(&items, None, &mut servers)?;
    if servers.is_empty() {
        return Err(ImportError::EmptyRedisInsight);
    }
    Ok(Some(servers))
}

fn collect_tinyrdm(items: &[yaml::Yaml], group: Option<&str>, out: &mut Vec<RedisServer>) -> Result<(), ImportError> {
    for item in items {
        if item.get_str("type") == Some("group") {
            let name = item.get_str("name").unwrap_or_default().to_string();
            if let Some(yaml::Yaml::Seq(children)) = item.get("connections") {
                collect_tinyrdm(children, Some(&name), out)?;
            }
            continue;
        }
        // Skip malformed entries rather than failing the whole file — a
        // partial import beats none when one row is broken.
        if item.get("addr").is_none() {
            continue;
        }
        out.push(tinyrdm_connection(item, group)?);
    }
    Ok(())
}

fn tinyrdm_connection(item: &yaml::Yaml, group: Option<&str>) -> Result<RedisServer, ImportError> {
    let host = item.get_str("addr").unwrap_or_default().to_string();
    if host.trim().is_empty() {
        return Err(ImportError::MissingHost);
    }
    let port = item
        .get_int("port")
        .filter(|p| *p > 0 && *p <= u16::MAX as i64)
        .ok_or(ImportError::InvalidPort)? as u16;
    let name = item
        .get_str("name")
        .and_then(opt)
        .unwrap_or_else(|| format!("{host}:{port}"));

    let mut server = RedisServer {
        name,
        host,
        port,
        username: item.get_str("username").and_then(opt),
        password: item.get_str("password").and_then(opt),
        key_separator: item.get_str("key_separator").and_then(opt),
        connection_timeout: item.get_int("conn_timeout").filter(|v| *v > 0).map(|v| v as u64),
        response_timeout: item.get_int("exec_timeout").filter(|v| *v > 0).map(|v| v as u64),
        group: group.and_then(opt),
        ..Default::default()
    };
    if let Some(ssl) = item.get("ssl").filter(|s| s.get_bool("enable") == Some(true)) {
        server.tls = Some(true);
        server.client_key = ssl.get_str("keyfile").and_then(opt);
        server.client_cert = ssl.get_str("certfile").and_then(opt);
        server.root_cert = ssl.get_str("cafile").and_then(opt);
        server.insecure = ssl.get_bool("allow_insecure").filter(|v| *v);
    }
    if let Some(ssh) = item.get("ssh").filter(|s| s.get_bool("enable") == Some(true))
        && let Some(addr) = ssh_addr(ssh.get_str("addr").unwrap_or_default(), ssh.get_int("port"))
    {
        server.ssh_tunnel = Some(true);
        server.ssh_addr = Some(addr);
        server.ssh_username = ssh.get_str("username").and_then(opt);
        server.ssh_password = ssh.get_str("password").and_then(opt);
        server.ssh_key = ssh.get_str("pk_file").and_then(opt);
        server.ssh_key_passphrase = ssh.get_str("passphrase").and_then(opt);
    }
    if let Some(sentinel) = item.get("sentinel").filter(|s| s.get_bool("enable") == Some(true)) {
        server.server_type = Some(SERVER_TYPE_SENTINEL);
        server.master_name = sentinel.get_str("master").and_then(opt);
        // Tiny RDM's top-level login is the sentinel's; the master's own is
        // nested under `sentinel`. Same split as for ARDM above.
        let node_username = sentinel.get_str("username").and_then(opt);
        let node_password = sentinel.get_str("password").and_then(opt);
        if node_username.is_some() || node_password.is_some() {
            server.sentinel_username = server.username.take();
            server.sentinel_password = server.password.take();
            server.username = node_username;
            server.password = node_password;
        }
    }
    if item.get("cluster").is_some_and(|c| c.get_bool("enable") == Some(true)) {
        server.server_type = Some(SERVER_TYPE_CLUSTER);
    }
    RedisServer::finalize_import(server)
}

// ─── Minimal YAML reader ─────────────────────────────────────────────────────

/// A deliberately small YAML subset reader — just enough for the
/// machine-generated `connections.yaml` Tiny RDM writes: block mappings
/// and sequences by indentation, scalars (plain / single- / double-quoted),
/// booleans, integers, comments, and empty flow collections. Anchors,
/// multi-line scalars, and flow collections with content are out of scope
/// (Tiny RDM never emits them); anything unrecognized degrades to a string,
/// so a surprising file yields a failed match rather than bad data.
mod yaml {
    #[derive(Debug, Clone, PartialEq)]
    pub enum Yaml {
        Map(Vec<(String, Yaml)>),
        Seq(Vec<Yaml>),
        Str(String),
        Bool(bool),
        Int(i64),
        Null,
    }

    impl Yaml {
        pub fn get(&self, key: &str) -> Option<&Yaml> {
            match self {
                Yaml::Map(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
                _ => None,
            }
        }
        pub fn get_str(&self, key: &str) -> Option<&str> {
            match self.get(key)? {
                Yaml::Str(s) => Some(s.as_str()),
                _ => None,
            }
        }
        pub fn get_int(&self, key: &str) -> Option<i64> {
            match self.get(key)? {
                Yaml::Int(v) => Some(*v),
                _ => None,
            }
        }
        pub fn get_bool(&self, key: &str) -> Option<bool> {
            match self.get(key)? {
                Yaml::Bool(v) => Some(*v),
                _ => None,
            }
        }
    }

    /// One significant line: its indentation and content (comments and
    /// trailing whitespace already stripped).
    struct Line {
        indent: usize,
        content: String,
    }

    fn strip_comment(line: &str) -> &str {
        // A `#` only starts a comment at line start or after whitespace, so
        // values like `pass#word` survive. Quoted strings are handled by the
        // scalar parser, which sees the text before any such split.
        let bytes = line.as_bytes();
        let mut in_single = false;
        let mut in_double = false;
        for (i, &b) in bytes.iter().enumerate() {
            match b {
                b'\'' if !in_double => in_single = !in_single,
                b'"' if !in_single => in_double = !in_double,
                b'#' if !in_single && !in_double && (i == 0 || bytes[i - 1] == b' ' || bytes[i - 1] == b'\t') => {
                    return &line[..i];
                }
                _ => {}
            }
        }
        line
    }

    fn lines_of(input: &str) -> Vec<Line> {
        input
            .lines()
            .filter_map(|raw| {
                let no_comment = strip_comment(raw);
                let trimmed = no_comment.trim_end();
                if trimmed.trim().is_empty() || trimmed.trim() == "---" {
                    return None;
                }
                let indent = trimmed.len() - trimmed.trim_start().len();
                Some(Line {
                    indent,
                    content: trimmed.trim_start().to_string(),
                })
            })
            .collect()
    }

    /// Parse a whole document. `None` when the text isn't the expected shape.
    pub fn parse(input: &str) -> Option<Yaml> {
        let mut lines = lines_of(input);
        if lines.is_empty() {
            return None;
        }
        let base = lines[0].indent;
        let mut pos = 0;
        Some(parse_node(&mut lines, &mut pos, base))
    }

    fn parse_node(lines: &mut Vec<Line>, pos: &mut usize, indent: usize) -> Yaml {
        if *pos >= lines.len() {
            return Yaml::Null;
        }
        if lines[*pos].content.starts_with("- ") || lines[*pos].content == "-" {
            parse_seq(lines, pos, indent)
        } else {
            parse_map(lines, pos, indent)
        }
    }

    fn parse_seq(lines: &mut Vec<Line>, pos: &mut usize, indent: usize) -> Yaml {
        let mut items = Vec::new();
        while *pos < lines.len() && lines[*pos].indent == indent {
            let content = lines[*pos].content.clone();
            let Some(rest) = content.strip_prefix('-').map(str::trim_start) else {
                break;
            };
            if rest.is_empty() {
                // `-` alone: the item body is the following indented block.
                *pos += 1;
                let child_indent = lines.get(*pos).map(|l| l.indent).unwrap_or(indent);
                if child_indent > indent {
                    items.push(parse_node(lines, pos, child_indent));
                } else {
                    items.push(Yaml::Null);
                }
                continue;
            }
            // `- key: value` — rewrite this line as the first entry of a map
            // one level in, then let the map parser consume the siblings.
            if is_map_entry(rest) {
                let child_indent = indent + 2;
                lines[*pos] = Line {
                    indent: child_indent,
                    content: rest.to_string(),
                };
                items.push(parse_map(lines, pos, child_indent));
            } else {
                items.push(scalar(rest));
                *pos += 1;
            }
        }
        Yaml::Seq(items)
    }

    /// Whether a line is a `key: …` mapping entry (rather than a scalar).
    fn is_map_entry(content: &str) -> bool {
        split_key(content).is_some()
    }

    /// Split `key: value` respecting quotes; `None` when there is no key.
    fn split_key(content: &str) -> Option<(String, String)> {
        let bytes = content.as_bytes();
        let mut in_single = false;
        let mut in_double = false;
        for (i, &b) in bytes.iter().enumerate() {
            match b {
                b'\'' if !in_double => in_single = !in_single,
                b'"' if !in_single => in_double = !in_double,
                b':' if !in_single && !in_double => {
                    let is_last = i + 1 == bytes.len();
                    if is_last || bytes[i + 1] == b' ' {
                        let key = content[..i].trim().trim_matches('"').trim_matches('\'').to_string();
                        if key.is_empty() {
                            return None;
                        }
                        let value = if is_last {
                            String::new()
                        } else {
                            content[i + 1..].trim().to_string()
                        };
                        return Some((key, value));
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn parse_map(lines: &mut Vec<Line>, pos: &mut usize, indent: usize) -> Yaml {
        let mut pairs = Vec::new();
        while *pos < lines.len() && lines[*pos].indent == indent {
            let content = lines[*pos].content.clone();
            let Some((key, value)) = split_key(&content) else {
                break;
            };
            *pos += 1;
            if value.is_empty() {
                // Block child (map or sequence) — or an empty value.
                let child_indent = lines.get(*pos).map(|l| l.indent);
                match child_indent {
                    // A nested sequence may sit at the parent's indent.
                    Some(child) if child > indent => {
                        pairs.push((key, parse_node(lines, pos, child)));
                    }
                    Some(child)
                        if child == indent
                            && lines
                                .get(*pos)
                                .is_some_and(|l| l.content.starts_with("- ") || l.content == "-") =>
                    {
                        pairs.push((key, parse_seq(lines, pos, child)));
                    }
                    _ => pairs.push((key, Yaml::Null)),
                }
            } else {
                pairs.push((key, scalar(&value)));
            }
        }
        Yaml::Map(pairs)
    }

    /// Parse one scalar token: quoted string, bool, integer, null, empty
    /// flow collection, or plain string.
    fn scalar(raw: &str) -> Yaml {
        let raw = raw.trim();
        if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
            return Yaml::Str(unescape_double(&raw[1..raw.len() - 1]));
        }
        if raw.len() >= 2 && raw.starts_with('\'') && raw.ends_with('\'') {
            // Single quotes escape only `''`.
            return Yaml::Str(raw[1..raw.len() - 1].replace("''", "'"));
        }
        match raw {
            "true" | "True" | "TRUE" | "yes" | "on" => return Yaml::Bool(true),
            "false" | "False" | "FALSE" | "no" | "off" => return Yaml::Bool(false),
            "" | "~" | "null" | "Null" | "NULL" => return Yaml::Null,
            "{}" | "[]" => return Yaml::Map(Vec::new()),
            _ => {}
        }
        if let Ok(v) = raw.parse::<i64>() {
            return Yaml::Int(v);
        }
        Yaml::Str(raw.to_string())
    }

    fn unescape_double(raw: &str) -> String {
        let mut out = String::with_capacity(raw.len());
        let mut chars = raw.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('0') => out.push('\0'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('/') => out.push('/'),
                // \xNN / \uNNNN — decode when well-formed, else keep literal.
                Some(esc @ ('x' | 'u')) => {
                    let width = if esc == 'x' { 2 } else { 4 };
                    let hex: String = chars.by_ref().take(width).collect();
                    match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        Some(decoded) => out.push(decoded),
                        None => {
                            out.push('\\');
                            out.push(esc);
                            out.push_str(&hex);
                        }
                    }
                }
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ardm_base64_export_maps_every_section() {
        let json = r#"{
            "groups": [{"id": 7, "name": "Prod"}],
            "connections": [
                {
                    "name": "cache",
                    "host": "10.0.0.5",
                    "port": 6380,
                    "auth": "s3cret",
                    "username": "app",
                    "separator": "::",
                    "groupId": 7,
                    "connectionReadOnly": true,
                    "sshOptions": {
                        "host": "bastion.internal", "port": 2222, "username": "ops",
                        "privatekey": "/home/ops/.ssh/id_ed25519", "passphrase": "pp"
                    },
                    "sslOptions": {"key": "KEY", "cert": "CERT", "ca": "CA"}
                },
                {"name": "shards", "host": "cluster.internal", "port": 7000, "cluster": true}
            ]
        }"#;
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(json);

        let servers = try_ardm_import(&encoded).expect("import ok").expect("recognized");
        assert_eq!(servers.len(), 2);

        let first = &servers[0];
        assert_eq!(first.name, "cache");
        assert_eq!(first.host, "10.0.0.5");
        assert_eq!(first.port, 6380);
        assert_eq!(first.password.as_deref(), Some("s3cret"));
        assert_eq!(first.username.as_deref(), Some("app"));
        assert_eq!(first.key_separator.as_deref(), Some("::"));
        assert_eq!(first.group.as_deref(), Some("Prod"));
        assert_eq!(first.readonly, Some(true));
        assert_eq!(first.ssh_tunnel, Some(true));
        assert_eq!(first.ssh_addr.as_deref(), Some("bastion.internal:2222"));
        assert_eq!(first.ssh_username.as_deref(), Some("ops"));
        assert_eq!(first.ssh_key.as_deref(), Some("/home/ops/.ssh/id_ed25519"));
        assert_eq!(first.ssh_key_passphrase.as_deref(), Some("pp"));
        assert_eq!(first.tls, Some(true));
        assert_eq!(first.root_cert.as_deref(), Some("CA"));
        // Every import gets a fresh id.
        assert!(!first.id.is_empty());

        assert_eq!(servers[1].server_type, Some(SERVER_TYPE_CLUSTER));
        assert!(servers[1].group.is_none());

        // The decoded JSON is accepted directly too.
        assert_eq!(try_ardm_import(json).expect("ok").expect("recognized").len(), 2);
    }

    #[test]
    fn ardm_sentinel_node_password_splits_the_logins() {
        let json = r#"{"connections": [{
            "name": "s", "host": "sentinel.internal", "port": 26379, "auth": "sentinel-pw",
            "sentinelOptions": {"masterName": "mymaster", "nodePassword": "master-pw"}
        }]}"#;
        let servers = try_ardm_import(json).expect("ok").expect("recognized");
        let s = &servers[0];
        assert_eq!(s.server_type, Some(SERVER_TYPE_SENTINEL));
        assert_eq!(s.master_name.as_deref(), Some("mymaster"));
        assert_eq!(s.password.as_deref(), Some("master-pw"));
        assert_eq!(s.sentinel_password.as_deref(), Some("sentinel-pw"));
        assert!(s.username.is_none() && s.sentinel_username.is_none());

        // Without a node password the single login stays with the data nodes.
        let json = r#"{"connections": [{
            "name": "s", "host": "sentinel.internal", "port": 26379, "auth": "pw",
            "sentinelOptions": {"masterName": "mymaster"}
        }]}"#;
        let s = &try_ardm_import(json).expect("ok").expect("recognized")[0];
        assert_eq!(s.password.as_deref(), Some("pw"));
        assert!(s.sentinel_password.is_none());
    }

    #[test]
    fn ardm_rejects_foreign_payloads() {
        assert!(try_ardm_import("not base64 at all !!!").expect("ok").is_none());
        // Valid JSON but not ARDM-shaped.
        assert!(
            try_ardm_import(r#"{"connections": [{"nope": 1}]}"#)
                .expect("ok")
                .is_none()
        );
        assert!(try_ardm_import(r#"{"name":"x","host":"h"}"#).expect("ok").is_none());
    }

    #[test]
    fn tinyrdm_yaml_flattens_groups_and_maps_options() {
        let yaml = r#"
- name: local
  last_db: 0
  addr: 127.0.0.1
  port: 6379
  key_separator: ":"
  conn_timeout: 60
  exec_timeout: 30
  ssl:
    enable: false
  ssh:
    enable: false
    login_type: pwd
  sentinel:
    enable: false
  cluster:
    enable: false
- name: Team
  type: group
  connections:
    - name: prod-tls   # trailing comment
      addr: redis.example.com
      port: 6380
      username: app
      password: "p@ss#word"
      ssl:
        enable: true
        keyfile: /certs/client.key
        certfile: /certs/client.crt
        cafile: /certs/ca.crt
        allow_insecure: true
      ssh:
        enable: true
        addr: bastion.example.com
        port: 2200
        username: ops
        pk_file: /home/ops/id_rsa
        passphrase: secret
      sentinel:
        enable: true
        master: mymaster
      cluster:
        enable: false
"#;
        let servers = try_tinyrdm_import(yaml).expect("import ok").expect("recognized");
        assert_eq!(servers.len(), 2);

        let local = &servers[0];
        assert_eq!(local.name, "local");
        assert_eq!(local.host, "127.0.0.1");
        assert_eq!(local.port, 6379);
        assert_eq!(local.key_separator.as_deref(), Some(":"));
        assert_eq!(local.connection_timeout, Some(60));
        assert_eq!(local.response_timeout, Some(30));
        // Disabled sections must not switch anything on.
        assert_eq!(local.tls, None);
        assert_eq!(local.ssh_tunnel, None);
        assert_eq!(local.server_type, None);
        assert!(local.group.is_none());

        let prod = &servers[1];
        assert_eq!(prod.name, "prod-tls");
        assert_eq!(prod.group.as_deref(), Some("Team"));
        assert_eq!(prod.host, "redis.example.com");
        assert_eq!(prod.port, 6380);
        // `#` inside a quoted scalar is not a comment.
        assert_eq!(prod.password.as_deref(), Some("p@ss#word"));
        assert_eq!(prod.tls, Some(true));
        assert_eq!(prod.client_key.as_deref(), Some("/certs/client.key"));
        assert_eq!(prod.client_cert.as_deref(), Some("/certs/client.crt"));
        assert_eq!(prod.root_cert.as_deref(), Some("/certs/ca.crt"));
        assert_eq!(prod.insecure, Some(true));
        assert_eq!(prod.ssh_tunnel, Some(true));
        assert_eq!(prod.ssh_addr.as_deref(), Some("bastion.example.com:2200"));
        assert_eq!(prod.ssh_key.as_deref(), Some("/home/ops/id_rsa"));
        assert_eq!(prod.server_type, Some(SERVER_TYPE_SENTINEL));
        assert_eq!(prod.master_name.as_deref(), Some("mymaster"));
    }

    #[test]
    fn tinyrdm_sentinel_login_splits_the_logins() {
        let yaml = r#"- name: s
  addr: sentinel.internal
  port: 26379
  username: sentinel-user
  password: sentinel-pw
  sentinel:
    enable: true
    master: mymaster
    username: app
    password: master-pw
"#;
        let servers = try_tinyrdm_import(yaml).expect("import ok").expect("recognized");
        let s = &servers[0];
        assert_eq!(s.server_type, Some(SERVER_TYPE_SENTINEL));
        assert_eq!(s.master_name.as_deref(), Some("mymaster"));
        assert_eq!(s.username.as_deref(), Some("app"));
        assert_eq!(s.password.as_deref(), Some("master-pw"));
        assert_eq!(s.sentinel_username.as_deref(), Some("sentinel-user"));
        assert_eq!(s.sentinel_password.as_deref(), Some("sentinel-pw"));
    }

    #[test]
    fn tinyrdm_rejects_foreign_yaml_and_json() {
        // A YAML list that isn't Tiny RDM.
        assert!(try_tinyrdm_import("- foo: 1\n- bar: 2\n").expect("ok").is_none());
        // JSON goes to the other importers.
        assert!(try_tinyrdm_import(r#"{"connections":[]}"#).expect("ok").is_none());
    }

    #[test]
    fn yaml_reader_handles_quotes_escapes_and_nesting() {
        use yaml::Yaml;
        let doc = yaml::parse(
            r#"
- name: "line\nbreak"
  quoted: 'it''s here'
  nested:
    deep:
      flag: true
      count: -12
  empty: {}
  blank:
"#,
        )
        .expect("parsed");
        let Yaml::Seq(items) = doc else {
            panic!("expected a sequence")
        };
        let item = &items[0];
        assert_eq!(item.get_str("name"), Some("line\nbreak"));
        assert_eq!(item.get_str("quoted"), Some("it's here"));
        let deep = item.get("nested").and_then(|n| n.get("deep")).expect("nested map");
        assert_eq!(deep.get_bool("flag"), Some(true));
        assert_eq!(deep.get_int("count"), Some(-12));
        assert_eq!(item.get("empty"), Some(&Yaml::Map(Vec::new())));
        assert_eq!(item.get("blank"), Some(&Yaml::Null));
    }
}
