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

//! Redis 7+ Functions wrappers.
//!
//! Covers the library lifecycle (`LIST` / `LOAD` / `DELETE` / `FLUSH` /
//! `DUMP` / `RESTORE` / `STATS`) plus trial invocation (`FCALL` /
//! `FCALL_RO`). Wire formats mix RESP-2/3 map-or-array shapes; we
//! project only the fields the UI needs so module upgrades don't
//! break the GUI.

use super::async_connection::RedisAsyncConn;
use crate::error::Error;
use crate::string::redis_value_to_string;
use redis::{Value, cmd};

type Result<T, E = Error> = std::result::Result<T, E>;

/// One function registered inside a library (one library can register
/// many). Only the public-API surface is captured here — internal
/// fields like the function's compiled bytecode are dropped.
#[derive(Debug, Clone, Default)]
pub struct FunctionMeta {
    pub name: String,
    pub description: Option<String>,
    /// `no-writes`, `allow-oom`, `allow-stale`, etc. Server's
    /// `register_function` flags.
    pub flags: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct FunctionLibrary {
    pub name: String,
    /// Currently always `"LUA"`; reserved for future engines.
    pub engine: String,
    pub functions: Vec<FunctionMeta>,
    /// Source code. Populated only when `function_list` is called with
    /// `with_code=true` (sends `WITHCODE`). `None` means the listing
    /// was the lightweight summary form.
    pub code: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct FunctionListing {
    pub libraries: Vec<FunctionLibrary>,
    /// `true` when `FUNCTION LIST` came back as "unknown command" —
    /// Redis < 7.0. UI uses this to show "Functions require Redis 7+"
    /// instead of "no libraries".
    pub unsupported: bool,
}

/// Snapshot from `FUNCTION STATS` — engine counters + optional
/// currently-running script.
#[derive(Debug, Clone, Default)]
pub struct FunctionStats {
    pub libraries_count: u64,
    pub functions_count: u64,
    /// Name of the function currently executing on the server, if any.
    pub running_name: Option<String>,
    /// Wall-clock duration of the running function in milliseconds.
    pub running_duration_ms: Option<u64>,
}

/// Policy for `FUNCTION RESTORE` after the payload.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FunctionRestorePolicy {
    /// Keep existing libraries; fail if a name collides.
    #[default]
    Append,
    /// Overwrite libraries with matching names.
    Replace,
    /// Drop all existing libraries before restore.
    Flush,
}

/// Client-side check before `FUNCTION LOAD`. Catches the common
/// "forgot the shebang" / "bad name" mistakes that Redis only rejects
/// with a terse ERR after round-trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryValidateError {
    Empty,
    MissingShebang,
    MissingName,
    InvalidName(String),
}

impl LibraryValidateError {
    /// Stable i18n key under the `[functions]` section.
    pub fn i18n_key(&self) -> &'static str {
        match self {
            Self::Empty => "code_required",
            Self::MissingShebang => "validate_missing_shebang",
            Self::MissingName => "validate_missing_name",
            Self::InvalidName(_) => "validate_invalid_name",
        }
    }
}

/// Successful local validation of a library source body.
#[derive(Debug, Clone)]
pub struct LibraryValidation {
    pub library_name: String,
    /// Soft issues that still allow LOAD (e.g. no `register_function`
    /// yet while the user is mid-edit). UI surfaces these as hints.
    pub warnings: Vec<&'static str>,
}

pub async fn function_list(conn: &mut RedisAsyncConn, with_code: bool) -> Result<FunctionListing> {
    let mut c = cmd("FUNCTION");
    c.arg("LIST");
    if with_code {
        c.arg("WITHCODE");
    }
    let res: redis::RedisResult<Value> = c.query_async(conn).await;
    match res {
        Ok(v) => Ok(FunctionListing {
            libraries: parse_list(&v).unwrap_or_default(),
            unsupported: false,
        }),
        Err(e) if is_unsupported(&e) => Ok(FunctionListing {
            unsupported: true,
            ..Default::default()
        }),
        Err(e) => Err(e.into()),
    }
}

/// Load a library. The library name is embedded in the source via the
/// `#!lua name=<libname>` shebang line — Redis returns that name on
/// success. `replace=true` overwrites an existing library with the
/// same name (the default RESP error otherwise).
pub async fn function_load(conn: &mut RedisAsyncConn, code: &str, replace: bool) -> Result<String> {
    let mut c = cmd("FUNCTION");
    c.arg("LOAD");
    if replace {
        c.arg("REPLACE");
    }
    c.arg(code);
    let name: String = c.query_async(conn).await?;
    Ok(name)
}

pub async fn function_delete(conn: &mut RedisAsyncConn, library: &str) -> Result<()> {
    let _: () = cmd("FUNCTION").arg("DELETE").arg(library).query_async(conn).await?;
    Ok(())
}

/// Invoke a loaded function. `readonly=true` uses `FCALL_RO` so Redis
/// rejects writes inside the script.
pub async fn function_fcall(
    conn: &mut RedisAsyncConn,
    name: &str,
    keys: &[String],
    args: &[String],
    readonly: bool,
) -> Result<String> {
    let mut c = cmd(if readonly { "FCALL_RO" } else { "FCALL" });
    c.arg(name).arg(keys.len());
    for k in keys {
        c.arg(k.as_str());
    }
    for a in args {
        c.arg(a.as_str());
    }
    let v: Value = c.query_async(conn).await?;
    Ok(redis_value_to_string(&v))
}

/// Serialize every loaded library into a binary payload (portable via
/// base64 for clipboard / file transfer).
pub async fn function_dump(conn: &mut RedisAsyncConn) -> Result<Vec<u8>> {
    let bytes: Vec<u8> = cmd("FUNCTION").arg("DUMP").query_async(conn).await?;
    Ok(bytes)
}

/// Restore libraries from a `FUNCTION DUMP` payload.
pub async fn function_restore(conn: &mut RedisAsyncConn, payload: &[u8], policy: FunctionRestorePolicy) -> Result<()> {
    let mut c = cmd("FUNCTION");
    c.arg("RESTORE").arg(payload);
    match policy {
        FunctionRestorePolicy::Append => {
            c.arg("APPEND");
        }
        FunctionRestorePolicy::Replace => {
            c.arg("REPLACE");
        }
        FunctionRestorePolicy::Flush => {
            c.arg("FLUSH");
        }
    }
    let _: () = c.query_async(conn).await?;
    Ok(())
}

/// Drop every library. `async_mode` maps to `ASYNC` vs `SYNC`.
pub async fn function_flush(conn: &mut RedisAsyncConn, async_mode: bool) -> Result<()> {
    let mut c = cmd("FUNCTION");
    c.arg("FLUSH");
    c.arg(if async_mode { "ASYNC" } else { "SYNC" });
    let _: () = c.query_async(conn).await?;
    Ok(())
}

pub async fn function_stats(conn: &mut RedisAsyncConn) -> Result<FunctionStats> {
    let v: Value = cmd("FUNCTION").arg("STATS").query_async(conn).await?;
    Ok(parse_stats(&v).unwrap_or_default())
}

// -------- validation --------

/// Parse and validate library source before `FUNCTION LOAD`.
///
/// Rules:
/// - non-empty body
/// - first non-empty line is a Lua shebang `#!lua …`
/// - shebang carries `name=<lib>` matching Redis's identifier rules
/// - soft warning when no `redis.register_function` call is present
pub fn validate_library_source(code: &str) -> std::result::Result<LibraryValidation, LibraryValidateError> {
    if code.trim().is_empty() {
        return Err(LibraryValidateError::Empty);
    }
    let first = code.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    if !first.starts_with("#!") {
        return Err(LibraryValidateError::MissingShebang);
    }
    // Engine must be lua (case-insensitive). Accept `#!lua` or `#! lua`.
    let after_bang = first.trim_start_matches('#').trim_start_matches('!');
    let after_bang = after_bang.trim_start();
    if !after_bang.to_ascii_lowercase().starts_with("lua") {
        return Err(LibraryValidateError::MissingShebang);
    }
    let name = extract_shebang_name(first).ok_or(LibraryValidateError::MissingName)?;
    if !is_valid_function_identifier(&name) {
        return Err(LibraryValidateError::InvalidName(name));
    }
    let mut warnings = Vec::new();
    if !code.contains("register_function") {
        warnings.push("validate_no_register");
    }
    Ok(LibraryValidation {
        library_name: name,
        warnings,
    })
}

fn extract_shebang_name(shebang: &str) -> Option<String> {
    // Prefer `name=foo` token (Redis docs form). Allow optional quotes.
    for token in shebang.split_whitespace() {
        let lower = token.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("name=") {
            // Re-slice original to keep case of the name value.
            let orig = &token[token.len() - rest.len()..];
            let name = orig.trim_matches(|c| c == '"' || c == '\'');
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Redis library / function identifiers: letter or `_` first, then
/// alphanumerics / `_`. Mirrors the server's practical acceptance set.
pub fn is_valid_function_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// -------- parsers --------

fn is_unsupported(err: &redis::RedisError) -> bool {
    let msg = err.to_string();
    msg.contains("unknown command") || msg.contains("ERR unknown") || msg.contains("ERR Unknown")
}

fn parse_simple_string(v: &Value) -> Option<String> {
    match v {
        Value::SimpleString(s) | Value::VerbatimString { text: s, .. } => Some(s.clone()),
        Value::BulkString(bytes) => String::from_utf8(bytes.clone()).ok(),
        Value::Int(n) => Some(n.to_string()),
        _ => None,
    }
}

fn parse_int(v: &Value) -> Option<u64> {
    match v {
        Value::Int(n) if *n >= 0 => Some(*n as u64),
        Value::BulkString(bytes) => String::from_utf8_lossy(bytes).parse().ok(),
        Value::SimpleString(s) => s.parse().ok(),
        _ => None,
    }
}

fn extract_pairs(v: &Value) -> Option<Vec<(String, Value)>> {
    match v {
        Value::Array(items) => {
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

fn parse_string_array(v: &Value) -> Vec<String> {
    match v {
        Value::Array(items) => items.iter().filter_map(parse_simple_string).collect(),
        Value::Set(items) => items.iter().filter_map(parse_simple_string).collect(),
        _ => Vec::new(),
    }
}

fn parse_function_meta(v: &Value) -> Option<FunctionMeta> {
    let entries = extract_pairs(v)?;
    let mut meta = FunctionMeta::default();
    for (k, val) in entries {
        match k.to_ascii_lowercase().as_str() {
            "name" => {
                meta.name = parse_simple_string(&val).unwrap_or_default();
            }
            "description" => {
                let s = parse_simple_string(&val).unwrap_or_default();
                if !s.is_empty() {
                    meta.description = Some(s);
                }
            }
            "flags" => {
                meta.flags = parse_string_array(&val);
            }
            _ => {}
        }
    }
    if meta.name.is_empty() {
        return None;
    }
    Some(meta)
}

fn parse_library(v: &Value) -> Option<FunctionLibrary> {
    let entries = extract_pairs(v)?;
    let mut lib = FunctionLibrary::default();
    for (k, val) in entries {
        match k.to_ascii_lowercase().as_str() {
            "library_name" => {
                lib.name = parse_simple_string(&val).unwrap_or_default();
            }
            "engine" => {
                lib.engine = parse_simple_string(&val).unwrap_or_default();
            }
            "functions" => {
                if let Value::Array(items) = val {
                    lib.functions = items.iter().filter_map(parse_function_meta).collect();
                }
            }
            "library_code" => {
                lib.code = parse_simple_string(&val);
            }
            _ => {}
        }
    }
    if lib.name.is_empty() {
        return None;
    }
    Some(lib)
}

fn parse_list(v: &Value) -> Option<Vec<FunctionLibrary>> {
    let items = match v {
        Value::Array(items) => items,
        _ => return None,
    };
    Some(items.iter().filter_map(parse_library).collect())
}

fn parse_stats(v: &Value) -> Option<FunctionStats> {
    let entries = extract_pairs(v)?;
    let mut stats = FunctionStats::default();
    for (k, val) in entries {
        match k.to_ascii_lowercase().as_str() {
            "running_script" => {
                if let Some(pairs) = extract_pairs(&val) {
                    for (rk, rv) in pairs {
                        match rk.to_ascii_lowercase().as_str() {
                            "name" => stats.running_name = parse_simple_string(&rv),
                            "duration_ms" => stats.running_duration_ms = parse_int(&rv),
                            _ => {}
                        }
                    }
                }
            }
            "engines" => {
                // engines → map/array of engine name → counters
                if let Some(engine_pairs) = extract_pairs(&val) {
                    for (engine, counters) in engine_pairs {
                        let _ = engine;
                        if let Some(c_pairs) = extract_pairs(&counters) {
                            for (ck, cv) in c_pairs {
                                match ck.to_ascii_lowercase().as_str() {
                                    "libraries_count" => {
                                        if let Some(n) = parse_int(&cv) {
                                            stats.libraries_count = stats.libraries_count.saturating_add(n);
                                        }
                                    }
                                    "functions_count" => {
                                        if let Some(n) = parse_int(&cv) {
                                            stats.functions_count = stats.functions_count.saturating_add(n);
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Some(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use redis::Value;

    fn bs(s: &str) -> Value {
        Value::BulkString(s.as_bytes().to_vec())
    }

    #[test]
    fn parses_listing_without_code() {
        let raw = Value::Array(vec![Value::Array(vec![
            bs("library_name"),
            bs("mylib"),
            bs("engine"),
            bs("LUA"),
            bs("functions"),
            Value::Array(vec![
                Value::Array(vec![
                    bs("name"),
                    bs("hello"),
                    bs("description"),
                    Value::Nil,
                    bs("flags"),
                    Value::Array(vec![]),
                ]),
                Value::Array(vec![
                    bs("name"),
                    bs("count"),
                    bs("description"),
                    bs("counts something"),
                    bs("flags"),
                    Value::Array(vec![bs("no-writes")]),
                ]),
            ]),
        ])]);
        let libs = parse_list(&raw).expect("parse failed");
        assert_eq!(libs.len(), 1);
        let lib = &libs[0];
        assert_eq!(lib.name.as_str(), "mylib");
        assert_eq!(lib.engine.as_str(), "LUA");
        assert_eq!(lib.functions.len(), 2);
        assert_eq!(lib.functions[0].name.as_str(), "hello");
        assert!(lib.functions[0].description.is_none());
        assert_eq!(lib.functions[1].name.as_str(), "count");
        assert_eq!(
            lib.functions[1].description.as_ref().map(|s| s.as_ref()),
            Some("counts something"),
        );
        assert_eq!(lib.functions[1].flags.len(), 1);
        assert!(lib.code.is_none());
    }

    #[test]
    fn parses_listing_with_code() {
        let raw = Value::Array(vec![Value::Array(vec![
            bs("library_name"),
            bs("mylib"),
            bs("engine"),
            bs("LUA"),
            bs("functions"),
            Value::Array(vec![]),
            bs("library_code"),
            bs("#!lua name=mylib\nredis.register_function('hello', function() return 'hi' end)"),
        ])]);
        let libs = parse_list(&raw).expect("parse failed");
        let code = libs[0].code.as_deref().expect("code should be present");
        assert!(code.starts_with("#!lua name=mylib"));
        assert!(code.contains("register_function"));
    }

    #[test]
    fn skips_malformed_libraries() {
        // Two libraries: one valid, one missing `library_name`. Parser
        // should silently drop the bad one rather than failing the
        // whole listing.
        let raw = Value::Array(vec![
            Value::Array(vec![bs("library_name"), bs("ok"), bs("engine"), bs("LUA")]),
            // Missing library_name → drop.
            Value::Array(vec![bs("engine"), bs("LUA")]),
        ]);
        let libs = parse_list(&raw).expect("parse failed");
        assert_eq!(libs.len(), 1);
        assert_eq!(libs[0].name.as_str(), "ok");
    }

    #[test]
    fn validates_happy_path() {
        let code = "#!lua name=mylib\nredis.register_function('hello', function() return 1 end)\n";
        let v = validate_library_source(code).expect("should pass");
        assert_eq!(v.library_name, "mylib");
        assert!(v.warnings.is_empty());
    }

    #[test]
    fn validates_missing_shebang() {
        let err = validate_library_source("redis.register_function('x', function() end)").expect_err("need shebang");
        assert_eq!(err, LibraryValidateError::MissingShebang);
    }

    #[test]
    fn validates_invalid_name() {
        let err = validate_library_source("#!lua name=bad-name!\n").expect_err("bad name");
        match err {
            LibraryValidateError::InvalidName(n) => assert_eq!(n, "bad-name!"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn validates_warns_without_register() {
        let v = validate_library_source("#!lua name=empty_lib\n-- TODO\n").expect("ok");
        assert_eq!(v.warnings, vec!["validate_no_register"]);
    }

    #[test]
    fn parses_stats_engines() {
        let raw = Value::Array(vec![
            bs("running_script"),
            Value::Nil,
            bs("engines"),
            Value::Array(vec![
                bs("LUA"),
                Value::Array(vec![
                    bs("libraries_count"),
                    Value::Int(2),
                    bs("functions_count"),
                    Value::Int(5),
                ]),
            ]),
        ]);
        let stats = parse_stats(&raw).expect("parse");
        assert_eq!(stats.libraries_count, 2);
        assert_eq!(stats.functions_count, 5);
        assert!(stats.running_name.is_none());
    }

    #[test]
    fn identifier_rules() {
        assert!(is_valid_function_identifier("mylib"));
        assert!(is_valid_function_identifier("_x"));
        assert!(is_valid_function_identifier("a1_b"));
        assert!(!is_valid_function_identifier("1bad"));
        assert!(!is_valid_function_identifier("bad-name"));
        assert!(!is_valid_function_identifier(""));
    }
}
