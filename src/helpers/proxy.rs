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

//! Outbound-proxy resolution for the app's HTTP clients (update check, AI).
//!
//! ureq's default config already honors `ALL_PROXY` / `HTTPS_PROXY` /
//! `HTTP_PROXY` (+ `NO_PROXY`), which covers terminal launches and most
//! Linux desktops. A GUI app started from the Dock / Finder / Explorer
//! inherits no shell environment though, so [`system_proxy`] falls back to
//! the OS-level *system* proxy: `scutil --proxy` on macOS, the WinINET
//! registry values on Windows — both read via a short-lived command so no
//! registry / SystemConfiguration dependency is pulled in.
//!
//! SOCKS is supported (`socks-proxy` feature): configured URIs may use
//! `socks4://` / `socks4a://` / `socks5://` / `socks5h://`, and the OS
//! fallbacks honor a SOCKS-only system proxy (after HTTPS/HTTP, which
//! tunnel via CONNECT and are preferred when both are on).
//!
//! Deliberately out of scope: PAC files (need a full PAC engine).

#[cfg(any(target_os = "macos", test))]
use std::collections::HashMap;
#[cfg(not(target_os = "linux"))]
use std::process::Command;
use std::sync::RwLock;
use tracing::debug;
use ureq::Proxy;

/// User-configured proxy override, mirrored from the persisted app state
/// (`ZedisAppState::http_proxy`) at startup and on every save. A process
/// global because the HTTP callers (updater, AI) run on background threads
/// with no `cx` to read the store through. Values:
/// - `""` — follow the environment / OS system proxy (the default);
/// - `"none"` — always connect directly, skipping every proxy source;
/// - anything else — a proxy URI to use as-is.
static CONFIGURED_PROXY: RwLock<String> = RwLock::new(String::new());

/// Mirror the persisted proxy setting into this module. Called at startup
/// and from the app-state setter, so a settings change applies to the next
/// request without a restart.
pub fn set_configured_proxy(value: &str) {
    *CONFIGURED_PROXY.write().expect("proxy setting lock poisoned") = value.trim().to_string();
}

/// Accepts what [`app_proxy`] can act on: empty (system), `none` (direct),
/// or a URI ureq's `Proxy` can parse. Drives the settings-input validator.
pub fn is_valid_proxy_setting(value: &str) -> bool {
    let value = value.trim();
    value.is_empty() || value.eq_ignore_ascii_case("none") || Proxy::new(value).is_ok()
}

/// How the configured value resolves — split from [`app_proxy`] so the
/// precedence is testable without touching env vars or the OS.
enum Resolution {
    /// `"none"` — connect directly, skip every proxy source.
    Direct,
    /// A usable configured URI.
    Explicit(Proxy),
    /// Nothing configured (or an unusable URI) — fall through to
    /// [`system_proxy`].
    System,
}

fn resolve_configured(configured: &str) -> Resolution {
    let configured = configured.trim();
    if configured.eq_ignore_ascii_case("none") {
        debug!("proxy: configured as direct — skipping env/OS proxies");
        return Resolution::Direct;
    }
    if !configured.is_empty() {
        match Proxy::new(configured) {
            Ok(proxy) => return Resolution::Explicit(proxy),
            Err(e) => {
                // The settings input validates, but a hand-edited
                // zedis.toml can still hold junk — degrade to the system
                // behavior instead of silently going direct.
                debug!(%configured, error = %e, "proxy: unusable configured URI, falling back to system");
            }
        }
    }
    Resolution::System
}

/// The proxy the app's HTTP clients should use, if any: the user-configured
/// setting first, then explicit proxy environment variables (ureq's own
/// lookup, incl. `NO_PROXY`), then the OS's system proxy settings.
/// Re-resolved per call — toggling the setting or the system proxy
/// (Clash & co.) must not require an app restart.
pub fn app_proxy() -> Option<Proxy> {
    let configured = CONFIGURED_PROXY.read().expect("proxy setting lock poisoned").clone();
    match resolve_configured(&configured) {
        Resolution::Direct => None,
        Resolution::Explicit(proxy) => Some(proxy),
        Resolution::System => system_proxy(),
    }
}

/// Env-var / OS fallback — see [`app_proxy`], the entry point callers use.
fn system_proxy() -> Option<Proxy> {
    if let Some(proxy) = Proxy::try_from_env() {
        return Some(proxy);
    }
    let uri = os_proxy_uri()?;
    match Proxy::new(&uri) {
        Ok(proxy) => {
            debug!(%uri, "system proxy: using OS proxy settings");
            Some(proxy)
        }
        Err(e) => {
            debug!(%uri, error = %e, "system proxy: unusable proxy URI");
            None
        }
    }
}

#[cfg(target_os = "macos")]
fn os_proxy_uri() -> Option<String> {
    let out = Command::new("scutil").arg("--proxy").output().ok()?;
    if !out.status.success() {
        return None;
    }
    proxy_from_scutil(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(target_os = "windows")]
fn os_proxy_uri() -> Option<String> {
    let out = Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    proxy_from_wininet_reg(&String::from_utf8_lossy(&out.stdout))
}

/// Desktop-specific stores (gsettings / KDE) vary too much to chase; the
/// env-var route in [`system_proxy`] is the lingua franca on Linux.
#[cfg(target_os = "linux")]
fn os_proxy_uri() -> Option<String> {
    None
}

/// Parse `scutil --proxy` output (`Key : value` lines inside a
/// `<dictionary>`). Prefers the HTTPS proxy — both the manifest fetch and
/// the installer download are HTTPS, tunneled via CONNECT — then HTTP,
/// then a SOCKS-only setup (macOS's SOCKS proxy is SOCKS5).
#[cfg(any(target_os = "macos", test))]
fn proxy_from_scutil(output: &str) -> Option<String> {
    let mut map = HashMap::new();
    for line in output.lines() {
        if let Some((k, v)) = line.split_once(" : ") {
            map.insert(k.trim(), v.trim());
        }
    }
    for (scheme, uri_scheme) in [("HTTPS", "http"), ("HTTP", "http"), ("SOCKS", "socks5")] {
        if map.get(format!("{scheme}Enable").as_str()) == Some(&"1")
            && let (Some(host), Some(port)) = (
                map.get(format!("{scheme}Proxy").as_str()),
                map.get(format!("{scheme}Port").as_str()),
            )
            && !host.is_empty()
        {
            return Some(format!("{uri_scheme}://{host}:{port}"));
        }
    }
    None
}

/// Parse `reg query …\Internet Settings` output. `ProxyServer` is either a
/// bare `host:port` applying to every protocol, or a
/// `scheme=host:port;…` list — prefer the `https` entry, then `http`,
/// then a SOCKS-only `socks=` entry (WinINET's SOCKS is SOCKS5).
#[cfg(any(target_os = "windows", test))]
fn proxy_from_wininet_reg(output: &str) -> Option<String> {
    let mut enabled = false;
    let mut server = "";
    for line in output.lines() {
        let mut parts = line.split_whitespace();
        match parts.next() {
            Some("ProxyEnable") => {
                enabled = parts.next_back().is_some_and(|v| v.trim_start_matches("0x") == "1");
            }
            Some("ProxyServer") => {
                server = parts.next_back().unwrap_or_default();
            }
            _ => {}
        }
    }
    if !enabled || server.is_empty() {
        return None;
    }
    if !server.contains('=') {
        return Some(format!("http://{server}"));
    }
    for (want, uri_scheme) in [("https", "http"), ("http", "http"), ("socks", "socks5")] {
        for entry in server.split(';') {
            if let Some((scheme, host_port)) = entry.split_once('=')
                && scheme.trim() == want
                && !host_port.trim().is_empty()
            {
                return Some(format!("{uri_scheme}://{}", host_port.trim()));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_value_resolution_precedence() {
        // "" → fall through to env/OS.
        assert!(matches!(resolve_configured(""), Resolution::System));
        assert!(matches!(resolve_configured("   "), Resolution::System));
        // "none" (any case) → direct, no proxies at all.
        assert!(matches!(resolve_configured("none"), Resolution::Direct));
        assert!(matches!(resolve_configured(" NONE "), Resolution::Direct));
        // A usable URI wins outright — HTTP and SOCKS alike.
        assert!(matches!(
            resolve_configured("http://127.0.0.1:7890"),
            Resolution::Explicit(_)
        ));
        assert!(matches!(
            resolve_configured("socks5://127.0.0.1:1080"),
            Resolution::Explicit(_)
        ));
        // Junk (hand-edited config) degrades to the system behavior.
        assert!(matches!(resolve_configured("::garbage::"), Resolution::System));
    }

    #[test]
    fn proxy_setting_validation() {
        assert!(is_valid_proxy_setting(""));
        assert!(is_valid_proxy_setting("none"));
        assert!(is_valid_proxy_setting("http://127.0.0.1:7890"));
        assert!(!is_valid_proxy_setting("::garbage::"));
    }

    #[test]
    fn scutil_prefers_https_then_http() {
        let both = "<dictionary> {\n  HTTPEnable : 1\n  HTTPPort : 7890\n  HTTPProxy : 127.0.0.1\n  HTTPSEnable : 1\n  HTTPSPort : 7891\n  HTTPSProxy : 10.0.0.2\n}\n";
        assert_eq!(proxy_from_scutil(both).as_deref(), Some("http://10.0.0.2:7891"));

        let http_only =
            "<dictionary> {\n  HTTPEnable : 1\n  HTTPPort : 7890\n  HTTPProxy : 127.0.0.1\n  HTTPSEnable : 0\n}\n";
        assert_eq!(proxy_from_scutil(http_only).as_deref(), Some("http://127.0.0.1:7890"));
    }

    #[test]
    fn scutil_disabled_is_none_and_socks_only_resolves() {
        let disabled = "<dictionary> {\n  HTTPEnable : 0\n  HTTPSEnable : 0\n}\n";
        assert_eq!(proxy_from_scutil(disabled), None);
        // SOCKS-only setups resolve now that ureq carries socks-proxy.
        let socks = "<dictionary> {\n  SOCKSEnable : 1\n  SOCKSPort : 1080\n  SOCKSProxy : 127.0.0.1\n}\n";
        assert_eq!(proxy_from_scutil(socks).as_deref(), Some("socks5://127.0.0.1:1080"));
        // ...but HTTP/HTTPS still win when they are enabled alongside.
        let mixed = "<dictionary> {\n  HTTPEnable : 1\n  HTTPPort : 7890\n  HTTPProxy : 127.0.0.1\n  SOCKSEnable : 1\n  SOCKSPort : 1080\n  SOCKSProxy : 127.0.0.1\n}\n";
        assert_eq!(proxy_from_scutil(mixed).as_deref(), Some("http://127.0.0.1:7890"));
    }

    #[test]
    fn wininet_bare_and_per_scheme_servers() {
        let bare = "    ProxyEnable    REG_DWORD    0x1\n    ProxyServer    REG_SZ    127.0.0.1:7890\n";
        assert_eq!(proxy_from_wininet_reg(bare).as_deref(), Some("http://127.0.0.1:7890"));

        let per_scheme = "    ProxyEnable    REG_DWORD    0x1\n    ProxyServer    REG_SZ    http=127.0.0.1:8888;https=127.0.0.1:8889;socks=127.0.0.1:1080\n";
        assert_eq!(
            proxy_from_wininet_reg(per_scheme).as_deref(),
            Some("http://127.0.0.1:8889")
        );

        // SOCKS-only per-scheme list resolves as SOCKS5.
        let socks_only = "    ProxyEnable    REG_DWORD    0x1\n    ProxyServer    REG_SZ    socks=127.0.0.1:1080\n";
        assert_eq!(
            proxy_from_wininet_reg(socks_only).as_deref(),
            Some("socks5://127.0.0.1:1080")
        );
    }

    #[test]
    fn wininet_disabled_is_none() {
        let disabled = "    ProxyEnable    REG_DWORD    0x0\n    ProxyServer    REG_SZ    127.0.0.1:7890\n";
        assert_eq!(proxy_from_wininet_reg(disabled), None);
        assert_eq!(proxy_from_wininet_reg(""), None);
    }
}
