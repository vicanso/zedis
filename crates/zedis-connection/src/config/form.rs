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

//! The server form and the entry it edits: the form's values read into a
//! `RedisServer`, and the indexes its radios and selects are drawn at.

use super::*;

/// Preset tag color keys, ordered to match the RadioGroup option list.
/// Index 0 = "none" (no chip rendered).
/// Tag color preset keys, ordered Local → Dev → UAT → Prod → Archive.
/// Environment presets offered in the server form, paired 1:1 with
/// [`TAG_ENV_LABELS`] by index. `magenta` is the production / high-risk
/// color (see [`RedisServer::is_high_risk_tag`]). Retired keys (`sky`,
/// `slate`, `green`, `red`, ...) are no longer offered but still render via
/// `canonical_tag_key`, so older servers keep their colored chip.
pub const TAG_COLOR_PRESETS: &[&str] = &["none", "teal", "purple", "magenta"];

fn tag_color_from_form_value(form_value: Option<&str>) -> Option<String> {
    let raw = form_value?.trim();
    if raw.is_empty() {
        return None;
    }
    // Form may pass either the index (RadioGroup) or the preset key (existing config).
    if let Ok(idx) = raw.parse::<usize>() {
        let key = *TAG_COLOR_PRESETS.get(idx)?;
        if key == "none" {
            return None;
        }
        return Some(key.to_string());
    }
    if raw == "none" {
        return None;
    }
    Some(raw.to_string())
}

/// The server form's *Write lock* choice, by the index of the option the
/// form shows — follow the tag (`None`), locked, unlocked — or by the word;
/// also `true` / `false`, which is what an older config or an import says.
pub(super) fn write_lock_from_form_value(form_value: Option<&str>) -> Option<bool> {
    match form_value.map(str::trim) {
        Some("1") | Some("locked") | Some("true") => Some(true),
        Some("2") | Some("unlocked") | Some("false") => Some(false),
        _ => None,
    }
}

/// The server form's one *Writes* choice, by option index — follow the tag,
/// allowed, locked, read-only — as `(readonly, write_lock)`. Read-only wins
/// whatever the lock says, and every other choice leaves it unset.
pub(super) fn writes_from_form_value(choice: &str) -> (Option<bool>, Option<bool>) {
    match choice.trim() {
        "1" | "allowed" => (None, Some(false)),
        "2" | "locked" => (None, Some(true)),
        "3" | "readonly" => (Some(true), None),
        _ => (None, None),
    }
}

/// The form index for the stored pair, in the order [`writes_from_form_value`]
/// reads: read-only first, since it is the stronger answer.
pub fn writes_index(readonly: Option<bool>, write_lock: Option<bool>) -> usize {
    if readonly.unwrap_or(false) {
        return 3;
    }
    match write_lock {
        None => 0,
        Some(false) => 1,
        Some(true) => 2,
    }
}

pub fn tag_color_index(value: Option<&str>) -> usize {
    let Some(v) = value else { return 0 };
    TAG_COLOR_PRESETS.iter().position(|k| *k == v).unwrap_or(0)
}

/// User-facing environment names for the server form's single "Environment"
/// select, index-aligned with [`TAG_COLOR_PRESETS`] (None→none, Dev→teal,
/// UAT→purple, Prod→magenta). These double as the persisted display tag, so
/// they are intentionally kept as stable English identifiers (not localized)
/// — like the color preset keys they pair with.
pub const TAG_ENV_LABELS: &[&str] = &["None", "Dev", "UAT", "Prod"];

/// Display label for a canonical tag-color preset key (e.g. `magenta` → `Prod`).
/// Returns `None` for `none`/unknown so a server with no environment stays
/// unlabeled.
fn env_label_for_key(key: &str) -> Option<&'static str> {
    let idx = TAG_COLOR_PRESETS.iter().position(|k| *k == key)?;
    let label = *TAG_ENV_LABELS.get(idx)?;
    (label != "None").then_some(label)
}

impl RedisServer {
    pub fn from_form_data(id: &str, data: &IndexMap<String, String>) -> Self {
        let get_str = |k: &str| data.get(k).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

        let get_parsed = |k: &str| get_str(k).and_then(|s| s.parse().ok());

        let get_bool = |k: &str| get_str(k).map(|s| s == "true" || s == "1");
        let redis_url = parse_url(get_str("host").unwrap_or_default());
        // The form's one *Writes* choice, or — from an older form, an import
        // or a script — the two fields it stands for.
        let (readonly, write_lock) = match get_str("writes") {
            Some(choice) => writes_from_form_value(&choice),
            None => (
                get_bool("readonly"),
                write_lock_from_form_value(get_str("write_lock").as_deref()),
            ),
        };
        let mut username = get_str("username");
        if username.is_none() && !redis_url.username.is_empty() {
            username = Some(redis_url.username.clone());
        }
        let mut password = get_str("password");
        if password.is_none() && redis_url.password.is_some() {
            password = redis_url.password.clone();
        }
        let mut tls = get_bool("tls");
        if redis_url.tls {
            tls = Some(true);
        }

        Self {
            id: id.to_string(),

            name: get_str("name").unwrap_or_default(),
            host: redis_url.host,
            port: get_parsed("port").unwrap_or_else(|| redis_url.port.unwrap_or(6379)),

            username,
            password,
            master_name: get_str("master_name"),
            sentinel_username: get_str("sentinel_username"),
            sentinel_password: get_str("sentinel_password"),
            description: get_str("description"),
            updated_at: None,
            // Only the web form has a `shared` box, and it always says one
            // thing or the other: ticked is everyone's, unticked is "private
            // to me", which the bridge turns into the name of whoever is
            // signed in. No box at all (the desktop form) says nothing — the
            // desktop's save then carries over the owner the stored entry had.
            owner: get_bool("shared")
                .map(|shared| if shared { Self::OWNER_SHARED } else { Self::OWNER_SELF }.to_string()),

            tls_server_name: get_str("tls_server_name"),
            client_cert: get_str("client_cert"),
            client_key: get_str("client_key"),
            client_key_passphrase: get_str("client_key_passphrase"),
            root_cert: get_str("root_cert"),

            ssh_addr: get_str("ssh_addr"),
            ssh_username: get_str("ssh_username"),
            ssh_password: get_str("ssh_password"),
            ssh_key: get_str("ssh_key"),
            ssh_key_passphrase: get_str("ssh_key_passphrase"),
            ssh_jump: get_str("ssh_jump"),

            server_type: get_parsed("server_type").map(|s| s as usize),
            cluster_read_replicas: get_bool("cluster_read_replicas"),

            tls,
            insecure: get_bool("insecure"),
            ssh_tunnel: get_bool("ssh_tunnel"),
            readonly,
            databases: get_str("databases")
                .and_then(|s| s.parse::<usize>().ok())
                .filter(|&n| n > 0),
            // No `> 0` filter here, unlike every other numeric override:
            // DB 0 is a real (and the most common) pin, so clearing the
            // field is the only way to unset it.
            default_db: get_str("default_db").and_then(|s| s.parse::<u16>().ok()),
            connection_timeout: get_str("connection_timeout")
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|&n| n > 0),
            response_timeout: get_str("response_timeout")
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|&n| n > 0),
            key_separator: get_str("key_separator"),
            key_scan_count: get_str("key_scan_count")
                .and_then(|s| s.parse::<usize>().ok())
                .filter(|&n| n > 0),
            max_key_tree_depth: get_str("max_key_tree_depth")
                .and_then(|s| s.parse::<usize>().ok())
                .filter(|&n| n > 0),
            auto_expand_threshold: get_str("auto_expand_threshold")
                .and_then(|s| s.parse::<usize>().ok())
                .filter(|&n| n > 0),
            // Radio index: 0 = inherit global, 1 = show, 2 = hide.
            show_key_tree_ttl: get_str("show_key_tree_ttl").and_then(|s| match s.as_str() {
                "1" | "true" | "show" => Some(true),
                "2" | "false" | "hide" => Some(false),
                _ => None,
            }),
            // The environment select is the single source of truth: derive the
            // display tag from the chosen preset rather than a separate
            // free-text field.
            tag: tag_color_from_form_value(get_str("tag_color").as_deref())
                .as_deref()
                .and_then(env_label_for_key)
                .map(String::from),
            tag_color: tag_color_from_form_value(get_str("tag_color").as_deref()),
            require_confirm_writes: get_bool("require_confirm_writes"),
            alerts_enabled: get_bool("alerts_enabled"),
            write_lock,
            group: get_str("group"),
            // sort_order is owned by reorder buttons / drag-drop, not
            // the edit form. Preserve the existing value (the caller
            // fills it in via `upsert_server`).
            sort_order: None,
        }
    }

    /// Form radio index for `show_key_tree_ttl`: 0 inherit / 1 show / 2 hide.
    pub fn show_key_tree_ttl_form_index(&self) -> usize {
        match self.show_key_tree_ttl {
            None => 0,
            Some(true) => 1,
            Some(false) => 2,
        }
    }
}
