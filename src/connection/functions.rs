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

//! Redis 7+ Functions (`FUNCTION LIST / LOAD / DELETE`) wrappers.
//!
//! Functions are the successor to EVAL scripts: persistent, named,
//! grouped into libraries. The wire format is RESP-2/3 with mixed
//! types; we project the parts the UI cares about (library name,
//! engine, per-function metadata, optional source code) and ignore
//! the rest so module upgrades don't break the GUI.

use super::async_connection::RedisAsyncConn;
use crate::error::Error;
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
}
