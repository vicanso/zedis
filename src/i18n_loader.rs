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
//! embedded via rust-embed (compressed in release builds) and served by
//! [`LazyLocaleBackend`], which decompresses and parses **one locale at a
//! time, on first lookup**: startup only lists the embedded file names, and a
//! user running in `zh` never pays to inflate the other seven locales (the
//! `en` fallback is parsed only if a key actually misses). This trades a
//! first-lookup parse of the active locale for a smaller binary and a smaller
//! resident set, with zero loss of translations.
//!
//! The TOML -> flat-key transformation mirrors rust-i18n's own `flatten_keys`
//! / v1 parsing (`rust-i18n-support`), so existing `t!("section.key")` lookups
//! resolve identically. The project's locale files are all v1 (filename is the
//! locale, no `_version` field) with string leaves nested one level, but the
//! scalar arms below are kept for parity with upstream.

use rust_embed::RustEmbed;
use rust_i18n::Backend;
use serde_json::Value;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(RustEmbed)]
#[folder = "locales"]
#[include = "*.toml"]
struct LocaleAssets;

/// Per-locale lazy translation store: locale names are known up front (from
/// the embedded file list, which costs no decompression), but each locale's
/// TOML is inflated and flattened only on the first `translate()` that asks
/// for it, then cached in its [`OnceLock`] for the rest of the process.
pub struct LazyLocaleBackend {
    locales: Vec<(String, OnceLock<HashMap<String, String>>)>,
}

impl LazyLocaleBackend {
    fn translations(&self, locale: &str) -> Option<&HashMap<String, String>> {
        let (name, cell) = self.locales.iter().find(|(name, _)| name == locale)?;
        Some(cell.get_or_init(|| parse_locale(name)))
    }
}

impl Backend for LazyLocaleBackend {
    fn available_locales(&self) -> Vec<Cow<'_, str>> {
        self.locales
            .iter()
            .map(|(name, _)| Cow::Borrowed(name.as_str()))
            .collect()
    }

    fn translate(&self, locale: &str, key: &str) -> Option<Cow<'_, str>> {
        self.translations(locale)?
            .get(key)
            .map(|value| Cow::Borrowed(value.as_str()))
    }

    fn messages_for_locale(&self, locale: &str) -> Option<Vec<(Cow<'_, str>, Cow<'_, str>)>> {
        let messages = self
            .translations(locale)?
            .iter()
            .map(|(key, value)| (Cow::Borrowed(key.as_str()), Cow::Borrowed(value.as_str())))
            .collect();
        Some(messages)
    }
}

/// Build the runtime translation backend over the embedded `locales/*.toml`.
///
/// Each file's stem is the locale (`en.toml` -> `en`). Only the file *names*
/// are read here; the contents stay compressed until [`Backend::translate`]
/// first touches that locale.
pub fn runtime_backend() -> LazyLocaleBackend {
    let locales = LocaleAssets::iter()
        .filter_map(|path| {
            path.strip_suffix(".toml")
                .map(|locale| (locale.to_string(), OnceLock::new()))
        })
        .collect();
    LazyLocaleBackend { locales }
}

/// Decompress and parse one locale file as a v1 rust-i18n document (the whole
/// tree belongs to that locale) and flatten it to dotted keys. A malformed or
/// non-UTF8 file yields an empty map rather than panicking — every lookup then
/// misses and falls through to the `fallback` locale.
fn parse_locale(locale: &str) -> HashMap<String, String> {
    let mut flat = HashMap::new();
    let Some(file) = LocaleAssets::get(&format!("{locale}.toml")) else {
        return flat;
    };
    let Ok(content) = std::str::from_utf8(&file.data) else {
        return flat;
    };
    let Ok(value) = toml::from_str::<Value>(content) else {
        return flat;
    };
    flatten_keys(String::new(), &value, &mut flat);
    flat
}

/// Flatten a parsed locale tree into dotted keys (`section.key`), mirroring
/// rust-i18n's `flatten_keys`: objects recurse with a `prefix.key` path and
/// scalars stringify. Arrays don't occur in the locale files and are ignored.
fn flatten_keys(prefix: String, value: &Value, out: &mut HashMap<String, String>) {
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
            out.insert(prefix, s.clone());
        }
        Value::Bool(b) => {
            out.insert(prefix, b.to_string());
        }
        Value::Number(n) => {
            out.insert(prefix, n.to_string());
        }
        Value::Null => {
            out.insert(prefix, String::new());
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

    #[test]
    fn parses_only_the_touched_locale() {
        let backend = runtime_backend();

        // Construction (and listing locales) parses nothing.
        backend.available_locales();
        assert!(
            backend.locales.iter().all(|(_, cell)| cell.get().is_none()),
            "no locale should be parsed before the first translate()"
        );

        // Looking up a `zh` key inflates `zh` and nothing else.
        assert!(backend.translate("zh", "status_bar.module_not_loaded").is_some());
        for (name, cell) in &backend.locales {
            assert_eq!(
                cell.get().is_some(),
                name == "zh",
                "unexpected parse state for locale {name}"
            );
        }
    }
}
