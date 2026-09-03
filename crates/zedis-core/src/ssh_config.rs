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

//! The slice of `~/.ssh/config` a tunnel needs — pure parsing, no I/O.
//!
//! OpenSSH semantics that matter here: options before the first `Host`
//! line apply to every host; a `Host` line starts a block that applies
//! when one of its patterns matches the alias the user typed (`*` and
//! `?` wildcards, `!` negation); the **first** value obtained for an
//! option wins, which is why `Host *` defaults sit at the bottom of a
//! file. `Match` blocks are skipped (their criteria need a live
//! environment) and `Include` is not followed.

use std::path::Path;

/// What a lookup found for one alias. Every field is `None` when the file
/// says nothing about it, so a caller only fills what it left blank.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SshHostConfig {
    pub host_name: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    /// The first `IdentityFile` — ssh offers every listed file, this
    /// client authenticates with one. Unexpanded: see
    /// [`expand_identity_file`].
    pub identity_file: Option<String>,
    pub proxy_jump: Option<String>,
}

impl SshHostConfig {
    pub fn is_empty(&self) -> bool {
        self.host_name.is_none()
            && self.user.is_none()
            && self.port.is_none()
            && self.identity_file.is_none()
            && self.proxy_jump.is_none()
    }
}

/// Resolve `alias` against the text of an ssh config file.
pub fn lookup(text: &str, alias: &str) -> SshHostConfig {
    let mut found = SshHostConfig::default();
    // Options before any `Host` / `Match` line are global.
    let mut applies = true;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((keyword, value)) = split_keyword(line) else {
            continue;
        };
        match keyword.as_str() {
            "host" => {
                applies = host_patterns_match(&value, alias);
                continue;
            }
            // `Match exec …` / `Match host …` need a live environment to
            // evaluate; the block is skipped rather than guessed.
            "match" => {
                applies = false;
                continue;
            }
            _ => {}
        }
        if !applies {
            continue;
        }
        match keyword.as_str() {
            "hostname" => set_first(&mut found.host_name, value),
            "user" => set_first(&mut found.user, value),
            "port" => {
                if found.port.is_none() {
                    found.port = value.parse().ok();
                }
            }
            "identityfile" => set_first(&mut found.identity_file, value),
            "proxyjump" => set_first(&mut found.proxy_jump, value),
            _ => {}
        }
    }
    found
}

/// `~`, `%d` (home), `%h` (the alias as typed) and `%%` in an
/// `IdentityFile` value. Other `%` tokens are left as they are.
pub fn expand_identity_file(value: &str, home: Option<&Path>, alias: &str) -> String {
    let home = home.map(|h| h.to_string_lossy().into_owned()).unwrap_or_default();
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    if let Some('~') = chars.peek()
        && !home.is_empty()
    {
        chars.next();
        out.push_str(&home);
    }
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('d') => out.push_str(&home),
            Some('h') => out.push_str(alias),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// `Keyword value`, `Keyword=value` or `Keyword = value`; the keyword is
/// case-insensitive and the value loses surrounding quotes.
fn split_keyword(line: &str) -> Option<(String, String)> {
    let end = line.find(|c: char| c.is_whitespace() || c == '=')?;
    let keyword = line[..end].to_ascii_lowercase();
    let rest = line[end..].trim_start_matches(|c: char| c.is_whitespace() || c == '=');
    let value = rest.trim().trim_matches('"').to_string();
    if keyword.is_empty() || value.is_empty() {
        return None;
    }
    Some((keyword, value))
}

fn set_first(slot: &mut Option<String>, value: String) {
    if slot.is_none() {
        *slot = Some(value);
    }
}

/// A `Host` line's patterns against the alias: any negated pattern that
/// matches vetoes the block, otherwise any positive match selects it.
fn host_patterns_match(patterns: &str, alias: &str) -> bool {
    let mut selected = false;
    for pattern in patterns.split(|c: char| c.is_whitespace() || c == ',') {
        if pattern.is_empty() {
            continue;
        }
        if let Some(negated) = pattern.strip_prefix('!') {
            if glob_match(negated, alias) {
                return false;
            }
        } else if glob_match(pattern, alias) {
            selected = true;
        }
    }
    selected
}

/// `*` (any run) and `?` (one char), case-insensitive like ssh's host
/// matching in practice.
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_ascii_lowercase().chars().collect();
    let t: Vec<char> = text.to_ascii_lowercase().chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let (mut star_p, mut star_t) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_p = Some(pi);
            star_t = ti;
            pi += 1;
        } else if let Some(sp) = star_p {
            pi = sp + 1;
            star_t += 1;
            ti = star_t;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::{SshHostConfig, expand_identity_file, glob_match, lookup};
    use std::path::Path;

    const CONFIG: &str = r#"
# comment
IdentitiesOnly yes

Host prod
    HostName 10.0.0.5
    User ops
    Port 2222
    IdentityFile ~/.ssh/prod_ed25519
    ProxyJump bastion

Host bastion
  HostName bastion.example.com
  User jump

Host staging-*  !staging-old
    User deploy

Match host something
    User never

Host=quoted
    IdentityFile "~/.ssh/my key"

Host *
    User fallback
    IdentityFile ~/.ssh/id_ed25519
    Port 22
"#;

    #[test]
    fn first_value_wins_and_wildcards_fill_the_rest() {
        let prod = lookup(CONFIG, "prod");
        assert_eq!(
            prod,
            SshHostConfig {
                host_name: Some("10.0.0.5".into()),
                user: Some("ops".into()),
                port: Some(2222),
                identity_file: Some("~/.ssh/prod_ed25519".into()),
                proxy_jump: Some("bastion".into()),
            }
        );
        // Only `Host *` speaks for an unknown alias.
        let other = lookup(CONFIG, "db.internal");
        assert_eq!(other.user.as_deref(), Some("fallback"));
        assert_eq!(other.identity_file.as_deref(), Some("~/.ssh/id_ed25519"));
        assert_eq!(other.port, Some(22));
        assert!(other.host_name.is_none() && other.proxy_jump.is_none());
    }

    #[test]
    fn negation_match_blocks_and_equals_syntax() {
        assert_eq!(lookup(CONFIG, "staging-1").user.as_deref(), Some("deploy"));
        // Vetoed by `!staging-old`, so the wildcard block answers instead.
        assert_eq!(lookup(CONFIG, "staging-old").user.as_deref(), Some("fallback"));
        // `Match` blocks are skipped: never "never".
        assert_eq!(lookup(CONFIG, "something").user.as_deref(), Some("fallback"));
        assert_eq!(lookup(CONFIG, "quoted").identity_file.as_deref(), Some("~/.ssh/my key"));
        assert!(lookup("", "anything").is_empty());
    }

    #[test]
    fn identity_file_tokens_expand() {
        let home = Path::new("/home/me");
        assert_eq!(
            expand_identity_file("~/.ssh/id_ed25519", Some(home), "prod"),
            "/home/me/.ssh/id_ed25519"
        );
        assert_eq!(
            expand_identity_file("%d/.ssh/%h_key", Some(home), "prod"),
            "/home/me/.ssh/prod_key"
        );
        assert_eq!(expand_identity_file("100%%", Some(home), "prod"), "100%");
        // No home known: `~` stays so the caller's own resolver can try.
        assert_eq!(expand_identity_file("~/.ssh/k", None, "prod"), "~/.ssh/k");
        assert_eq!(expand_identity_file("/abs/%x", Some(home), "prod"), "/abs/%x");
    }

    #[test]
    fn glob_semantics() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("prod-?", "prod-1"));
        assert!(!glob_match("prod-?", "prod-12"));
        assert!(glob_match("*.example.com", "db.example.com"));
        assert!(glob_match("Prod", "prod"));
        assert!(!glob_match("prod", "production"));
        assert!(glob_match("a*b*c", "a-b-c"));
        assert!(!glob_match("a*b*c", "a-c-b"));
    }
}
