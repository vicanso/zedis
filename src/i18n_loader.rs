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

//! Runtime i18n backend.
//!
//! The `i18n!` macro is pointed at the empty `locales_stub/` directory so it
//! embeds no translations at compile time — that codegen would otherwise be
//! ~600KiB of `_RUST_I18N_BACKEND` map-insertion instructions (the single
//! largest function in the binary). Instead the real `locales/*.toml` are
//! embedded via rust-embed (compressed in release builds) and parsed into a
//! [`SimpleBackend`] on first access. This trades a one-time startup parse for
//! a smaller binary, with zero loss of translations.
//!
//! The TOML -> flat-key transformation mirrors rust-i18n's own `flatten_keys`
//! / v1 parsing (`rust-i18n-support`), so existing `t!("section.key")` lookups
//! resolve identically. The project's locale files are all v1 (filename is the
//! locale, no `_version` field) with string leaves nested one level, but the
//! scalar arms below are kept for parity with upstream.

use rust_embed::RustEmbed;
use rust_i18n::SimpleBackend;
use serde_json::Value;
use std::borrow::Cow;
use std::collections::HashMap;

#[derive(RustEmbed)]
#[folder = "locales"]
#[include = "*.toml"]
struct LocaleAssets;

/// Build the runtime translation backend from the embedded `locales/*.toml`.
///
/// Each file's stem is the locale (`en.toml` -> `en`); the file is parsed as a
/// v1 rust-i18n document (the whole tree belongs to that locale) and flattened
/// to dotted keys. Malformed or non-UTF8 files are skipped rather than panicking
/// — a missing translation simply falls through to the `fallback` locale.
pub fn runtime_backend() -> SimpleBackend {
    let mut backend = SimpleBackend::new();
    for path in LocaleAssets::iter() {
        let Some(locale) = path.strip_suffix(".toml") else {
            continue;
        };
        let Some(file) = LocaleAssets::get(path.as_ref()) else {
            continue;
        };
        let Ok(content) = std::str::from_utf8(&file.data) else {
            continue;
        };
        let Ok(value) = toml::from_str::<Value>(content) else {
            continue;
        };
        let mut flat: HashMap<Cow<'static, str>, Cow<'static, str>> = HashMap::new();
        flatten_keys(String::new(), &value, &mut flat);
        backend.add_translations(Cow::Owned(locale.to_string()), flat);
    }
    backend
}

/// Flatten a parsed locale tree into dotted keys (`section.key`), mirroring
/// rust-i18n's `flatten_keys`: objects recurse with a `prefix.key` path and
/// scalars stringify. Arrays don't occur in the locale files and are ignored.
fn flatten_keys(prefix: String, value: &Value, out: &mut HashMap<Cow<'static, str>, Cow<'static, str>>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let next = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten_keys(next, child, out);
            }
        }
        Value::String(s) => {
            out.insert(Cow::Owned(prefix), Cow::Owned(s.clone()));
        }
        Value::Bool(b) => {
            out.insert(Cow::Owned(prefix), Cow::Owned(b.to_string()));
        }
        Value::Number(n) => {
            out.insert(Cow::Owned(prefix), Cow::Owned(n.to_string()));
        }
        Value::Null => {
            out.insert(Cow::Owned(prefix), Cow::Borrowed(""));
        }
        Value::Array(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::runtime_backend;
    use rust_i18n::Backend;

    #[test]
    fn loads_flattens_and_resolves_locales() {
        let backend = runtime_backend();

        // All eight shipped locales are present.
        let locales = backend.available_locales();
        assert_eq!(locales.len(), 8, "expected 8 locales, got {locales:?}");
        for lang in ["en", "zh", "de", "es", "fr", "ja", "pt", "ru"] {
            assert!(locales.iter().any(|l| l.as_ref() == lang), "missing locale {lang}");
        }

        // A `[section]` table flattens to the dotted `section.key` form the
        // `t!("section.key")` call sites expect.
        assert_eq!(
            backend.translate("en", "status_bar.module_not_loaded").as_deref(),
            Some("module not loaded")
        );
        // Native (non-English) values resolve, not just the fallback.
        assert_eq!(
            backend.translate("zh", "status_bar.module_not_loaded").as_deref(),
            Some("模块未加载")
        );
        // Unknown keys return None so the `fallback = "en"` chain can engage.
        assert!(backend.translate("en", "status_bar.__does_not_exist__").is_none());
    }
}
