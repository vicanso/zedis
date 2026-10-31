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

//! Pure parser and formatter for `zedis://` deep links and web query links.
//!
//! Format: `zedis://open?server=<name_or_id>&db=<n>&key=<key>&view=<view>`
//!
//! Security: Links never carry passwords, secret tokens, or values. Any link
//! containing embedded userinfo (`user:pass@`) or secret query parameters is
//! rejected.

use std::fmt;

/// Parsed `zedis://` deep link targeting a server, database, key, or view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepLink {
    /// Server name or ID (never credentials!).
    pub server: String,
    /// Optional logical DB index (`0..`).
    pub db: Option<usize>,
    /// Optional key name to select/reveal in the editor.
    pub key: Option<String>,
    /// Optional server view name (e.g. `editor`, `metrics`, `slowlog`, `monitor`, `benchmark`).
    pub view: Option<String>,
}

/// Errors returned when parsing a `zedis://` deep link or web query string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeepLinkError {
    /// Input does not start with `zedis://` or has an unsupported path.
    InvalidScheme,
    /// Link embeds userinfo (`user:pass@`) or secret parameters.
    ContainsCredentials,
    /// Required `server` parameter is missing or empty.
    MissingServer,
    /// `db` parameter is not a valid non-negative integer.
    InvalidDb(String),
    /// Percent-encoded sequence is malformed or not valid UTF-8.
    InvalidEncoding,
}

impl fmt::Display for DeepLinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidScheme => f.write_str("invalid zedis:// URI scheme or path"),
            Self::ContainsCredentials => f.write_str("deep links must not contain credentials"),
            Self::MissingServer => f.write_str("missing required 'server' parameter"),
            Self::InvalidDb(val) => write!(f, "invalid database index '{val}'"),
            Self::InvalidEncoding => f.write_str("invalid percent-encoding in deep link"),
        }
    }
}

impl std::error::Error for DeepLinkError {}

/// Returns `true` if `raw` starts with `zedis://` (case-insensitive, after trimming whitespace).
pub fn is_zedis_deep_link(raw: &str) -> bool {
    let trimmed = raw.trim();
    trimmed.len() >= 8 && trimmed.as_bytes()[..8].eq_ignore_ascii_case(b"zedis://")
}

impl DeepLink {
    /// Parse a `zedis://` URI such as `zedis://open?server=prod&db=0&key=user%3A42`.
    ///
    /// Also accepts `zedis://open/?...`, `zedis://?...`, and `zedis:///?...`.
    pub fn parse(raw: &str) -> Result<Self, DeepLinkError> {
        let trimmed = raw.trim();
        if !is_zedis_deep_link(trimmed) {
            return Err(DeepLinkError::InvalidScheme);
        }
        let rest = &trimmed[8..];
        // Strip fragment (#...) if present on the raw URI (note: `#` inside a key must be `%23`).
        let without_fragment = rest.split('#').next().unwrap_or(rest);
        let (target, query) = without_fragment.split_once('?').ok_or(DeepLinkError::MissingServer)?;

        // Security rule: reject embedded userinfo (`user:pass@`) in the authority/path part.
        if target.contains('@') {
            return Err(DeepLinkError::ContainsCredentials);
        }

        let normalized_target = target.trim_matches('/');
        if !normalized_target.is_empty() && !normalized_target.eq_ignore_ascii_case("open") {
            return Err(DeepLinkError::InvalidScheme);
        }

        Self::parse_query_pairs(query, false)
    }

    /// Parse a browser URL query string (`window.location.search`, with or without leading `?`).
    ///
    /// Recognizes:
    /// - `?zedis=<url-encoded zedis:// URI>`
    /// - `?open=1&server=<name>&...`
    /// - `?server=<name>&db=<n>&key=<key>&view=<view>`
    ///
    /// Returns `Ok(None)` when the query string does not carry deep-link parameters.
    pub fn from_query_str(query: &str) -> Result<Option<Self>, DeepLinkError> {
        let trimmed = query.trim();
        let trimmed = trimmed.strip_prefix('?').unwrap_or(trimmed);
        let trimmed = trimmed.split('#').next().unwrap_or(trimmed).trim();
        if trimmed.is_empty() {
            return Ok(None);
        }

        let mut has_open = false;
        let mut has_server = false;

        for pair in trimmed.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (raw_k, raw_v) = pair.split_once('=').unwrap_or((pair, ""));
            let k = percent_decode(raw_k, true)?;
            if k.eq_ignore_ascii_case("zedis") {
                let v = percent_decode(raw_v, true)?;
                if v.trim().is_empty() {
                    return Ok(None);
                }
                return Self::parse(&v).map(Some);
            }
            if k.eq_ignore_ascii_case("open") {
                has_open = true;
            } else if k.eq_ignore_ascii_case("server") {
                has_server = true;
            }
        }

        if !has_open && !has_server {
            return Ok(None);
        }

        Self::parse_query_pairs(trimmed, true).map(Some)
    }

    fn parse_query_pairs(query: &str, plus_as_space: bool) -> Result<Self, DeepLinkError> {
        let mut server: Option<String> = None;
        let mut db: Option<usize> = None;
        let mut key: Option<String> = None;
        let mut view: Option<String> = None;

        for pair in query.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (raw_k, raw_v) = pair.split_once('=').unwrap_or((pair, ""));
            let k = percent_decode(raw_k, plus_as_space)?;
            let v = percent_decode(raw_v, plus_as_space)?;

            match k.to_ascii_lowercase().as_str() {
                "server" => {
                    // Reject credentials disguised in the server field (e.g. `redis://:secret@host` or `user:pass@host`).
                    if v.contains('@') || v.contains("://") {
                        return Err(DeepLinkError::ContainsCredentials);
                    }
                    if !v.trim().is_empty() {
                        server = Some(v);
                    }
                }
                "db" => {
                    let trimmed_v = v.trim();
                    if !trimmed_v.is_empty() {
                        let parsed = trimmed_v
                            .parse::<usize>()
                            .map_err(|_| DeepLinkError::InvalidDb(trimmed_v.to_string()))?;
                        db = Some(parsed);
                    }
                }
                "key" => {
                    if !v.is_empty() {
                        key = Some(v);
                    }
                }
                "view" => {
                    let trimmed_v = v.trim();
                    if !trimmed_v.is_empty() {
                        view = Some(trimmed_v.to_string());
                    }
                }
                "password" | "pass" | "secret" | "token" | "auth" | "username" | "user" => {
                    return Err(DeepLinkError::ContainsCredentials);
                }
                _ => {}
            }
        }

        let server = server.ok_or(DeepLinkError::MissingServer)?;
        Ok(Self { server, db, key, view })
    }

    /// Format as a `zedis://open?...` URI with RFC 3986 percent-encoding.
    pub fn to_zedis_uri(&self) -> String {
        let mut out = String::from("zedis://open");
        self.append_query_params(&mut out);
        out
    }

    /// Format as a relative web query string (`?server=...&db=...&key=...`).
    pub fn to_web_query(&self) -> String {
        let mut out = String::new();
        self.append_query_params(&mut out);
        out
    }

    fn append_query_params(&self, out: &mut String) {
        out.push_str("?server=");
        percent_encode(&self.server, out);
        if let Some(db) = self.db {
            out.push_str("&db=");
            out.push_str(&db.to_string());
        }
        if let Some(key) = &self.key
            && !key.is_empty()
        {
            out.push_str("&key=");
            percent_encode(key, out);
        }
        if let Some(view) = &self.view
            && !view.is_empty()
        {
            out.push_str("&view=");
            percent_encode(view, out);
        }
    }
}

/// RFC 3986 unreserved characters: ALPHA / DIGIT / "-" / "." / "_" / "~"
fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
}

fn percent_encode(input: &str, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &b in input.as_bytes() {
        if is_unreserved(b) {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0F) as usize] as char);
        }
    }
}

fn from_hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn percent_decode(input: &str, plus_as_space: bool) -> Result<String, DeepLinkError> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                if i + 2 >= bytes.len() {
                    return Err(DeepLinkError::InvalidEncoding);
                }
                let hi = from_hex_digit(bytes[i + 1]).ok_or(DeepLinkError::InvalidEncoding)?;
                let lo = from_hex_digit(bytes[i + 2]).ok_or(DeepLinkError::InvalidEncoding)?;
                out.push((hi << 4) | lo);
                i += 3;
            }
            b'+' if plus_as_space => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| DeepLinkError::InvalidEncoding)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_deep_link() {
        let link = DeepLink::parse("zedis://open?server=prod-cache&db=2&key=user%3A42&view=editor")
            .expect("should parse valid deep link");
        assert_eq!(
            link,
            DeepLink {
                server: "prod-cache".to_string(),
                db: Some(2),
                key: Some("user:42".to_string()),
                view: Some("editor".to_string()),
            }
        );
    }

    #[test]
    fn parses_open_with_slash_and_empty_target() {
        let with_slash = DeepLink::parse("zedis://open/?server=local&db=0").expect("should parse open/");
        assert_eq!(with_slash.server, "local");
        assert_eq!(with_slash.db, Some(0));

        let root_query = DeepLink::parse("zedis://?server=local&key=foo").expect("should parse root query");
        assert_eq!(root_query.server, "local");
        assert_eq!(root_query.key.as_deref(), Some("foo"));
    }

    #[test]
    fn round_trips_special_characters_and_utf8() {
        let original = DeepLink {
            server: "Prod Cluster #1 (EU & US)".to_string(),
            db: Some(15),
            key: Some("tenant:🚀/user?id=42&role=admin# hash+plus%20=val".to_string()),
            view: Some("slowlog".to_string()),
        };
        let uri = original.to_zedis_uri();
        assert!(uri.starts_with("zedis://open?server="));
        let parsed = DeepLink::parse(&uri).expect("round-trip parse should succeed");
        assert_eq!(parsed, original);

        let web_query = original.to_web_query();
        assert!(web_query.starts_with("?server="));
        let from_web = DeepLink::from_query_str(&web_query)
            .expect("web query parse should succeed")
            .expect("should produce Some(DeepLink)");
        assert_eq!(from_web, original);
    }

    #[test]
    fn rejects_embedded_credentials_and_secrets() {
        assert_eq!(
            DeepLink::parse("zedis://user:secret@open?server=prod"),
            Err(DeepLinkError::ContainsCredentials)
        );
        assert_eq!(
            DeepLink::parse("zedis://open?server=user:pass@127.0.0.1:6379"),
            Err(DeepLinkError::ContainsCredentials)
        );
        assert_eq!(
            DeepLink::parse("zedis://open?server=redis://127.0.0.1:6379"),
            Err(DeepLinkError::ContainsCredentials)
        );
        assert_eq!(
            DeepLink::parse("zedis://open?server=prod&password=123"),
            Err(DeepLinkError::ContainsCredentials)
        );
        assert_eq!(
            DeepLink::parse("zedis://open?server=prod&token=abc"),
            Err(DeepLinkError::ContainsCredentials)
        );
    }

    #[test]
    fn rejects_missing_server_and_invalid_db() {
        assert_eq!(
            DeepLink::parse("zedis://open?db=0&key=foo"),
            Err(DeepLinkError::MissingServer)
        );
        assert_eq!(
            DeepLink::parse("zedis://open?server=&key=foo"),
            Err(DeepLinkError::MissingServer)
        );
        assert_eq!(
            DeepLink::parse("zedis://open?server=prod&db=abc"),
            Err(DeepLinkError::InvalidDb("abc".to_string()))
        );
        assert_eq!(
            DeepLink::parse("zedis://unknown-action?server=prod"),
            Err(DeepLinkError::InvalidScheme)
        );
    }

    #[test]
    fn parses_web_query_variants() {
        assert_eq!(DeepLink::from_query_str("").expect("empty query"), None);
        assert_eq!(
            DeepLink::from_query_str("?foo=bar&baz=1").expect("unrelated query"),
            None
        );

        let direct = DeepLink::from_query_str("?server=my+redis&db=1&key=a%3Ab")
            .expect("direct query should parse")
            .expect("should return Some");
        assert_eq!(direct.server, "my redis");
        assert_eq!(direct.db, Some(1));
        assert_eq!(direct.key.as_deref(), Some("a:b"));

        let wrapped = DeepLink::from_query_str("?zedis=zedis%3A%2F%2Fopen%3Fserver%3Dstaging%26db%3D3%26key%3Dk1")
            .expect("wrapped zedis param should parse")
            .expect("should return Some");
        assert_eq!(wrapped.server, "staging");
        assert_eq!(wrapped.db, Some(3));
        assert_eq!(wrapped.key.as_deref(), Some("k1"));
    }

    #[test]
    fn detects_zedis_scheme_case_insensitively() {
        assert!(is_zedis_deep_link("zedis://open?server=a"));
        assert!(is_zedis_deep_link("  ZEDIS://open?server=a "));
        assert!(!is_zedis_deep_link("redis://127.0.0.1:6379"));
        assert!(!is_zedis_deep_link("https://example.com"));
    }
}
