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

//! RediSearch (`FT.*`) command wrappers and response parsers.
//!
//! The module abstracts four flows surfaced by the search-manager view:
//! * `FT._LIST` — enumerate index names
//! * `FT.INFO`  — fetch schema + stats for one index
//! * `FT.SEARCH` — full-text query with optional HIGHLIGHT / RETURN / LIMIT
//! * `FT.AGGREGATE` — single-layer `GROUPBY` + `REDUCE` pipeline
//!
//! RediSearch responses vary across module versions and RESP2/RESP3
//! transports — the parsers below intentionally accept multiple shapes
//! and silently ignore unknown keys rather than fail the whole call,
//! since module upgrades shouldn't break the GUI.

use super::async_connection::RedisAsyncConn;
use crate::error::Error;
use redis::{Value, cmd};

type Result<T, E = Error> = std::result::Result<T, E>;

/// Field categories RediSearch can index. The set tracks the official
/// schema vocabulary; unrecognised types from future versions land in
/// [`FieldKind::Unknown`] so the schema panel still renders.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    Text,
    Numeric,
    Tag,
    Geo,
    Vector,
    GeoShape,
    Unknown(String),
}

impl FieldKind {
    fn from_str(s: &str) -> Self {
        match s.to_ascii_uppercase().as_str() {
            "TEXT" => FieldKind::Text,
            "NUMERIC" => FieldKind::Numeric,
            "TAG" => FieldKind::Tag,
            "GEO" => FieldKind::Geo,
            "VECTOR" => FieldKind::Vector,
            "GEOSHAPE" => FieldKind::GeoShape,
            other => FieldKind::Unknown(other.to_string()),
        }
    }
}

/// One attribute / field as declared in the index's schema.
#[derive(Debug, Clone, Default)]
pub struct FieldSchema {
    pub name: String,
    pub kind_str: String,
    pub sortable: bool,
    pub no_index: bool,
    pub no_stem: bool,
    pub weight: Option<f64>,
    pub separator: Option<String>,
}

impl FieldSchema {
    pub fn kind(&self) -> FieldKind {
        FieldKind::from_str(&self.kind_str)
    }
}

/// Decoded view of `FT.INFO`. Only the fields the UI actually renders
/// are kept; everything else is dropped during parse.
#[derive(Debug, Clone, Default)]
pub struct IndexInfo {
    pub num_docs: u64,
    pub max_doc_id: u64,
    pub num_terms: u64,
    pub num_records: u64,
    /// `true` while RediSearch is still backfilling the index from
    /// pre-existing keys (immediately after `FT.CREATE`). Surfaced
    /// in the schema header so users don't mistake "still indexing"
    /// for "0 documents found".
    pub indexing: bool,
    /// Number of keys that matched the index's prefix but failed to be
    /// indexed — typically because they were stored as `HASH` while the
    /// index was created `ON JSON` (or vice versa). A non-zero value is
    /// the most actionable diagnostic for "I have data but the index
    /// shows 0 docs".
    pub indexing_failures: u64,
    pub fields: Vec<FieldSchema>,
    /// HASH / JSON — the underlying storage the index reads from.
    pub key_type: String,
    /// Key prefixes the index watches (empty means all keys).
    pub prefixes: Vec<String>,
    pub language: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    /// `(offset, count)`. `(0, 10)` is the RediSearch default.
    pub limit: (u32, u32),
    /// Empty ⇒ return all stored fields.
    pub return_fields: Vec<String>,
    /// Empty ⇒ no `HIGHLIGHT` clause at all.
    pub highlight_fields: Vec<String>,
    /// Default tags used if `HIGHLIGHT` is enabled and the user picked the
    /// "wrap with these markers" preset.
    pub highlight_open: Option<String>,
    pub highlight_close: Option<String>,
    /// `SORTBY <field> [ASC|DESC]`. Field must be SORTABLE in the schema.
    pub sort_by: Option<String>,
    /// When `sort_by` is set, `false` = ASC (default), `true` = DESC.
    pub sort_desc: bool,
    /// Query dialect version (`DIALECT N`). `None` omits the clause so the
    /// server default applies. Dialects ≥ 2 unlock modern syntax.
    pub dialect: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct SearchHit {
    pub doc_id: String,
    pub fields: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default)]
pub struct SearchResult {
    /// Total matching documents (not the page size).
    pub total: u64,
    pub hits: Vec<SearchHit>,
}

/// Single-stage aggregation: one `GROUPBY` + one reducer. The view
/// builder constrains the user to this shape; more complex pipelines
/// would need an additional `stages` field here.
#[derive(Debug, Clone, Default)]
pub struct AggregateOptions {
    pub group_by: Vec<String>,
    pub reducer: Option<ReducerSpec>,
    pub limit: Option<(u32, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReducerFn {
    Count,
    CountDistinct,
    Sum,
    Avg,
    Min,
    Max,
    StdDev,
    Quantile,
    ToList,
    FirstValue,
}

impl ReducerFn {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReducerFn::Count => "COUNT",
            ReducerFn::CountDistinct => "COUNT_DISTINCT",
            ReducerFn::Sum => "SUM",
            ReducerFn::Avg => "AVG",
            ReducerFn::Min => "MIN",
            ReducerFn::Max => "MAX",
            ReducerFn::StdDev => "STDDEV",
            ReducerFn::Quantile => "QUANTILE",
            ReducerFn::ToList => "TOLIST",
            ReducerFn::FirstValue => "FIRST_VALUE",
        }
    }
    /// How many positional arguments the reducer expects (excluding the
    /// field name itself for COUNT, which takes 0 args).
    pub fn arity(&self) -> usize {
        match self {
            // COUNT takes no field argument
            ReducerFn::Count => 0,
            // QUANTILE needs the field + a percentile (e.g. `0.5`)
            ReducerFn::Quantile => 2,
            // Everything else takes a single field name
            _ => 1,
        }
    }
    pub fn all() -> [ReducerFn; 10] {
        [
            ReducerFn::Count,
            ReducerFn::CountDistinct,
            ReducerFn::Sum,
            ReducerFn::Avg,
            ReducerFn::Min,
            ReducerFn::Max,
            ReducerFn::StdDev,
            ReducerFn::Quantile,
            ReducerFn::ToList,
            ReducerFn::FirstValue,
        ]
    }
}

/// Inputs for `FT.CREATE`. Mirrors a subset of the official option
/// surface — index identity, key-type binding, prefix filter, and the
/// schema's per-field declarations. WEIGHT / SEPARATOR / VECTOR-specific
/// knobs are intentionally omitted from this MVP form; users who need
/// them can still drop into a CLI/Terminal.
#[derive(Debug, Clone, Default)]
pub struct CreateIndexOptions {
    pub index: String,
    /// `false` ⇒ `ON HASH` (the default), `true` ⇒ `ON JSON` (RediSearch
    /// 2.6+ with the JSON module loaded).
    pub on_json: bool,
    pub prefixes: Vec<String>,
    pub fields: Vec<CreateFieldSpec>,
}

#[derive(Debug, Clone, Default)]
pub struct CreateFieldSpec {
    pub name: String,
    /// One of `TEXT` / `NUMERIC` / `TAG` / `GEO`. Free-form so we can
    /// add future types without code churn — the form constrains the
    /// user to the supported subset.
    pub field_type: String,
    pub sortable: bool,
    pub no_stem: bool,
    pub no_index: bool,
}

/// Drop an existing index. `delete_documents` controls the destructive
/// `DD` suffix — `true` removes the indexed documents from Redis along
/// with the index definition, `false` (the safer default) only removes
/// the index leaving raw key data intact.
pub async fn ft_dropindex(conn: &mut RedisAsyncConn, index: &str, delete_documents: bool) -> Result<()> {
    let mut c = cmd("FT.DROPINDEX");
    c.arg(index);
    if delete_documents {
        c.arg("DD");
    }
    let _: () = c.query_async(conn).await?;
    Ok(())
}

/// Add a new attribute to an existing index via `FT.ALTER`. RediSearch
/// only supports *adding* fields — type changes and removals require a
/// drop + recreate cycle. Existing documents are re-scanned and
/// back-indexed against the new field, which is typically fast but
/// blocks proportional to dataset size; the indexer runs in the
/// background after the command returns.
pub async fn ft_alter_add(conn: &mut RedisAsyncConn, index: &str, field: &CreateFieldSpec) -> Result<()> {
    let mut c = cmd("FT.ALTER");
    c.arg(index).arg("SCHEMA").arg("ADD");
    c.arg(field.name.as_str()).arg(field.field_type.as_str());
    if field.sortable {
        c.arg("SORTABLE");
    }
    if field.no_stem {
        c.arg("NOSTEM");
    }
    if field.no_index {
        c.arg("NOINDEX");
    }
    let _: () = c.query_async(conn).await?;
    Ok(())
}

pub async fn ft_create(conn: &mut RedisAsyncConn, opts: &CreateIndexOptions) -> Result<()> {
    let mut c = cmd("FT.CREATE");
    c.arg(opts.index.as_str());
    c.arg("ON").arg(if opts.on_json { "JSON" } else { "HASH" });
    if !opts.prefixes.is_empty() {
        c.arg("PREFIX").arg(opts.prefixes.len());
        for p in &opts.prefixes {
            c.arg(p.as_str());
        }
    }
    c.arg("SCHEMA");
    for f in &opts.fields {
        // For JSON-backed indexes the identifier uses JSONPath form, but
        // the form lets users type either `$.title` or `title`; pass it
        // through verbatim. AS-aliases aren't surfaced here yet.
        c.arg(f.name.as_str()).arg(f.field_type.as_str());
        if f.sortable {
            c.arg("SORTABLE");
        }
        if f.no_stem {
            // NOSTEM only meaningful for TEXT; harmless on other types
            // would be rejected by RediSearch, so the form gates it.
            c.arg("NOSTEM");
        }
        if f.no_index {
            c.arg("NOINDEX");
        }
    }
    let _: () = c.query_async(conn).await?;
    Ok(())
}

#[derive(Debug, Clone, Default)]
pub struct ReducerSpec {
    pub func: Option<ReducerFn>,
    pub args: Vec<String>,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AggregateResult {
    /// First element of the reply. Not trustworthy across module
    /// versions: RediSearch <2.6 always reports 1, and some pipelines
    /// report the page size rather than the match total — treat it as a
    /// total only when it exceeds `rows.len()` (the view's header does
    /// exactly that; paging uses the full-page heuristic instead).
    pub total: u64,
    /// Each row is a list of `(field, value)` pairs.
    pub rows: Vec<Vec<(String, String)>>,
}

/// Returned-shape indicator for `FT._LIST`: an empty `Vec` is ambiguous
/// (server doesn't have RediSearch, vs. has it but no indexes yet).
#[derive(Debug, Clone, Default)]
pub struct IndexListing {
    pub names: Vec<String>,
    /// True ⇒ the server returned `ERR unknown command` for `FT._LIST`.
    /// UI uses this to show "RediSearch module not loaded" instead of "no
    /// indexes".
    pub unsupported: bool,
}

pub async fn ft_list(conn: &mut RedisAsyncConn) -> Result<IndexListing> {
    let res: redis::RedisResult<Vec<String>> = cmd("FT._LIST").query_async(conn).await;
    match res {
        Ok(names) => Ok(IndexListing {
            names: names.into_iter().collect(),
            unsupported: false,
        }),
        Err(e) if is_unsupported(&e) => Ok(IndexListing {
            unsupported: true,
            ..Default::default()
        }),
        Err(e) => Err(e.into()),
    }
}

pub async fn ft_info(conn: &mut RedisAsyncConn, index: &str) -> Result<IndexInfo> {
    let value: Value = cmd("FT.INFO").arg(index).query_async(conn).await?;
    parse_info(&value).ok_or_else(|| Error::Invalid {
        message: format!("FT.INFO {index} returned unexpected shape"),
    })
}

/// `FT.EXPLAIN index query [DIALECT n]` — the query's execution plan as
/// the server's own indented tree, one bulk string with embedded
/// newlines. Also valid for the query half of an aggregation (the plan
/// covers the filter expression, not the pipeline).
pub async fn ft_explain(conn: &mut RedisAsyncConn, index: &str, query: &str, dialect: Option<u32>) -> Result<String> {
    let mut c = cmd("FT.EXPLAIN");
    c.arg(index).arg(query);
    if let Some(d) = dialect {
        c.arg("DIALECT").arg(d);
    }
    let value: Value = c.query_async(conn).await?;
    parse_simple_string(&value).ok_or_else(|| Error::Invalid {
        message: format!("FT.EXPLAIN {index} returned unexpected shape"),
    })
}

/// `FT.PROFILE index SEARCH|AGGREGATE QUERY query` — run the query and
/// return the profile section (parsing/iterator timings) rendered as
/// indented text. The reply nests differently per module version and
/// RESP transport, so instead of committing to a schema the raw tree is
/// pretty-printed — robust the same way the other parsers here are.
pub async fn ft_profile(conn: &mut RedisAsyncConn, index: &str, aggregate: bool, query: &str) -> Result<String> {
    let mut c = cmd("FT.PROFILE");
    c.arg(index)
        .arg(if aggregate { "AGGREGATE" } else { "SEARCH" })
        .arg("QUERY")
        .arg(query);
    let value: Value = c.query_async(conn).await?;
    let profile = profile_section(&value);
    let mut out = String::new();
    pretty_value(profile, 0, &mut out);
    if out.trim().is_empty() {
        return Err(Error::Invalid {
            message: format!("FT.PROFILE {index} returned unexpected shape"),
        });
    }
    Ok(out)
}

/// Build and dispatch an `FT.SEARCH` invocation. Options that are
/// "empty" simply omit their clause.
pub async fn ft_search(
    conn: &mut RedisAsyncConn,
    index: &str,
    query: &str,
    opts: &SearchOptions,
) -> Result<SearchResult> {
    let mut c = cmd("FT.SEARCH");
    c.arg(index).arg(query);
    if !opts.return_fields.is_empty() {
        c.arg("RETURN").arg(opts.return_fields.len());
        for f in &opts.return_fields {
            c.arg(f.as_str());
        }
    }
    if !opts.highlight_fields.is_empty() {
        c.arg("HIGHLIGHT").arg("FIELDS").arg(opts.highlight_fields.len());
        for f in &opts.highlight_fields {
            c.arg(f.as_str());
        }
        if let (Some(open), Some(close)) = (&opts.highlight_open, &opts.highlight_close) {
            c.arg("TAGS").arg(open.as_str()).arg(close.as_str());
        }
    }
    if let Some(field) = &opts.sort_by
        && !field.is_empty()
    {
        c.arg("SORTBY").arg(field.as_str());
        if opts.sort_desc {
            c.arg("DESC");
        } else {
            c.arg("ASC");
        }
    }
    let (offset, count) = opts.limit;
    c.arg("LIMIT").arg(offset).arg(count);
    if let Some(d) = opts.dialect {
        c.arg("DIALECT").arg(d);
    }
    let value: Value = c.query_async(conn).await?;
    parse_search(&value).ok_or_else(|| Error::Invalid {
        message: format!("FT.SEARCH {index} returned unexpected shape"),
    })
}

pub async fn ft_aggregate(
    conn: &mut RedisAsyncConn,
    index: &str,
    query: &str,
    opts: &AggregateOptions,
) -> Result<AggregateResult> {
    let mut c = cmd("FT.AGGREGATE");
    c.arg(index).arg(query);
    if !opts.group_by.is_empty() {
        c.arg("GROUPBY").arg(opts.group_by.len());
        for f in &opts.group_by {
            // GROUPBY fields use `@field` form on the wire.
            c.arg(format!("@{}", f.as_str()));
        }
        if let Some(reducer) = &opts.reducer
            && let Some(func) = &reducer.func
        {
            c.arg("REDUCE").arg(func.as_str()).arg(reducer.args.len());
            for arg in &reducer.args {
                // Field-name args also need the `@` sigil — except for
                // QUANTILE's numeric percentile, which is just a literal.
                let is_field_arg = !arg.starts_with('@') && !is_numeric_literal(arg);
                if is_field_arg {
                    c.arg(format!("@{}", arg.as_str()));
                } else {
                    c.arg(arg.as_str());
                }
            }
            if let Some(alias) = &reducer.alias {
                c.arg("AS").arg(alias.as_str());
            }
        }
    }
    if let Some((offset, count)) = opts.limit {
        c.arg("LIMIT").arg(offset).arg(count);
    }
    let value: Value = c.query_async(conn).await?;
    parse_aggregate(&value).ok_or_else(|| Error::Invalid {
        message: format!("FT.AGGREGATE {index} returned unexpected shape"),
    })
}

// -------- parsers --------

fn is_numeric_literal(s: &str) -> bool {
    s.trim().parse::<f64>().is_ok()
}

fn is_unsupported(err: &redis::RedisError) -> bool {
    let msg = err.to_string();
    msg.contains("unknown command")
        || msg.contains("ERR unknown")
        || msg.contains("not available")
        || msg.contains("ERR Unknown")
}

fn parse_simple_string(v: &Value) -> Option<String> {
    match v {
        Value::SimpleString(s) | Value::VerbatimString { text: s, .. } => Some(s.clone()),
        Value::BulkString(bytes) => String::from_utf8(bytes.clone()).ok(),
        Value::Int(n) => Some(n.to_string()),
        Value::Double(n) => Some(format!("{n}")),
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
                .filter_map(|(k, val)| Some((parse_simple_string(k)?, val.clone())))
                .collect(),
        ),
        _ => None,
    }
}

fn parse_int(v: &Value) -> Option<i64> {
    match v {
        Value::Int(n) => Some(*n),
        _ => parse_simple_string(v).and_then(|s| s.parse().ok()),
    }
}

fn parse_field_definition(v: &Value) -> Option<FieldSchema> {
    // Attribute arrays mix `key value` pairs with bare flag tokens
    // (`SORTABLE`, `NOINDEX`, `NOSTEM`), so a plain pair iterator can't
    // describe them — walk linearly instead, peeking ahead for values.
    //
    // RESP3 may deliver this as a Map; in that case there are no bare
    // flags and pair extraction is sufficient.
    if let Value::Map(_) = v {
        return parse_field_definition_from_pairs(extract_pairs(v)?);
    }
    let items = match v {
        Value::Array(items) => items,
        _ => return None,
    };
    let mut field = FieldSchema::default();
    let mut legacy_name_candidate: Option<String> = None;
    let mut i = 0;
    while i < items.len() {
        let token = match parse_simple_string(&items[i]) {
            Some(s) => s,
            None => {
                i += 1;
                continue;
            }
        };
        let lower = token.to_ascii_lowercase();
        match lower.as_str() {
            // Bare flags (no value follows).
            "sortable" => {
                field.sortable = true;
                i += 1;
            }
            "noindex" => {
                field.no_index = true;
                i += 1;
            }
            "nostem" => {
                field.no_stem = true;
                i += 1;
            }
            // Keyed values.
            "identifier" => {
                if field.name.is_empty()
                    && let Some(next) = items.get(i + 1)
                    && let Some(s) = parse_simple_string(next)
                {
                    field.name = s;
                }
                i += 2;
            }
            "attribute" => {
                if let Some(next) = items.get(i + 1)
                    && let Some(s) = parse_simple_string(next)
                {
                    field.name = s;
                }
                i += 2;
            }
            "type" => {
                if let Some(next) = items.get(i + 1)
                    && let Some(s) = parse_simple_string(next)
                {
                    field.kind_str = s;
                }
                i += 2;
            }
            "weight" => {
                if let Some(next) = items.get(i + 1) {
                    field.weight = parse_simple_string(next).and_then(|s| s.parse().ok());
                }
                i += 2;
            }
            "separator" => {
                if let Some(next) = items.get(i + 1) {
                    field.separator = parse_simple_string(next);
                }
                i += 2;
            }
            _ => {
                // Legacy (RediSearch 1.x: `[name, type, TEXT, ...]`):
                // first bare token is the field name. Consume one slot
                // so the loop picks up the real `type` key next.
                if i == 0 && legacy_name_candidate.is_none() {
                    legacy_name_candidate = Some(token);
                    i += 1;
                } else {
                    // Unknown key elsewhere — skip a pair conservatively.
                    i += 2;
                }
            }
        }
    }
    if field.name.is_empty()
        && let Some(name) = legacy_name_candidate
    {
        field.name = name;
    }
    if field.name.is_empty() {
        return None;
    }
    Some(field)
}

fn parse_field_definition_from_pairs(entries: Vec<(String, Value)>) -> Option<FieldSchema> {
    let mut field = FieldSchema::default();
    for (k, val) in entries {
        let key = k.to_ascii_lowercase();
        match key.as_str() {
            "identifier" if field.name.is_empty() => {
                field.name = parse_simple_string(&val).unwrap_or_default();
            }
            "attribute" => {
                field.name = parse_simple_string(&val).unwrap_or_default();
            }
            "type" => {
                field.kind_str = parse_simple_string(&val).unwrap_or_default();
            }
            "weight" => field.weight = parse_simple_string(&val).and_then(|s| s.parse().ok()),
            "separator" => field.separator = parse_simple_string(&val),
            "sortable" => field.sortable = matches!(parse_int(&val), Some(n) if n != 0),
            "noindex" => field.no_index = matches!(parse_int(&val), Some(n) if n != 0),
            "nostem" => field.no_stem = matches!(parse_int(&val), Some(n) if n != 0),
            _ => {}
        }
    }
    if field.name.is_empty() {
        return None;
    }
    Some(field)
}

fn parse_info(value: &Value) -> Option<IndexInfo> {
    let entries = extract_pairs(value)?;
    let mut info = IndexInfo::default();
    for (k, val) in entries {
        let key = k.to_ascii_lowercase();
        match key.as_str() {
            "num_docs" => info.num_docs = parse_int(&val).unwrap_or_default().max(0) as u64,
            "max_doc_id" => info.max_doc_id = parse_int(&val).unwrap_or_default().max(0) as u64,
            "num_terms" => info.num_terms = parse_int(&val).unwrap_or_default().max(0) as u64,
            "num_records" => info.num_records = parse_int(&val).unwrap_or_default().max(0) as u64,
            "indexing" => info.indexing = matches!(parse_int(&val), Some(n) if n != 0),
            // Both legacy (RediSearch 1.x: `hash_indexing_failures`) and
            // modern (2.x: `indexing_failures` inside `gc_stats`/top-level)
            // spellings exist; accept either.
            "hash_indexing_failures" | "indexing_failures" => {
                info.indexing_failures = parse_int(&val).unwrap_or_default().max(0) as u64;
            }
            "attributes" | "fields" => {
                if let Value::Array(items) = &val {
                    info.fields = items.iter().filter_map(parse_field_definition).collect();
                }
            }
            "index_definition" => {
                if let Some(def_entries) = extract_pairs(&val) {
                    for (dk, dv) in def_entries {
                        let dk = dk.to_ascii_lowercase();
                        match dk.as_str() {
                            "key_type" => {
                                info.key_type = parse_simple_string(&dv).unwrap_or_default();
                            }
                            "prefixes" => {
                                if let Value::Array(items) = dv {
                                    info.prefixes = items.iter().filter_map(parse_simple_string).collect();
                                }
                            }
                            "language" => {
                                info.language = parse_simple_string(&dv);
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Some(info)
}

fn parse_search(value: &Value) -> Option<SearchResult> {
    let items = match value {
        Value::Array(items) => items,
        // RESP3 maps for FT.SEARCH appear in newer RediSearch — fall back
        // to looking up by key.
        Value::Map(_) => {
            return parse_search_map(value);
        }
        _ => return None,
    };
    let mut iter = items.iter();
    let total = iter.next().and_then(parse_int).unwrap_or_default().max(0) as u64;
    let mut hits = Vec::new();
    while let Some(id_val) = iter.next() {
        let doc_id: String = parse_simple_string(id_val)?;
        let fields_val = iter.next();
        let fields = fields_val
            .and_then(extract_pairs)
            .map(|pairs| {
                pairs
                    .into_iter()
                    .map(|(k, v)| (k, parse_simple_string(&v).unwrap_or_default()))
                    .collect()
            })
            .unwrap_or_default();
        hits.push(SearchHit { doc_id, fields });
    }
    Some(SearchResult { total, hits })
}

fn parse_search_map(value: &Value) -> Option<SearchResult> {
    let pairs = extract_pairs(value)?;
    let mut result = SearchResult::default();
    for (k, v) in pairs {
        let key = k.to_ascii_lowercase();
        match key.as_str() {
            "total_results" => result.total = parse_int(&v).unwrap_or_default().max(0) as u64,
            "results" => {
                if let Value::Array(items) = v {
                    for hit in items {
                        if let Some(hit_pairs) = extract_pairs(&hit) {
                            let mut sh = SearchHit::default();
                            for (hk, hv) in hit_pairs {
                                match hk.to_ascii_lowercase().as_str() {
                                    "id" => sh.doc_id = parse_simple_string(&hv).unwrap_or_default(),
                                    "extra_attributes" | "values" => {
                                        if let Some(field_pairs) = extract_pairs(&hv) {
                                            sh.fields = field_pairs
                                                .into_iter()
                                                .map(|(fk, fv)| (fk, parse_simple_string(&fv).unwrap_or_default()))
                                                .collect();
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            result.hits.push(sh);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Some(result)
}

/// Locate the profile half of an `FT.PROFILE` reply: RESP3 maps carry a
/// "profile"-named entry, RESP2 replies are `[results, profile]`.
/// Anything else falls back to the whole value so at least the raw tree
/// is shown.
fn profile_section(value: &Value) -> &Value {
    match value {
        Value::Map(items) => items
            .iter()
            .find(|(k, _)| {
                parse_simple_string(k)
                    .map(|s| s.to_ascii_lowercase().contains("profile"))
                    .unwrap_or(false)
            })
            .map(|(_, v)| v)
            .unwrap_or(value),
        Value::Array(items) if items.len() == 2 => &items[1],
        _ => value,
    }
}

/// Render any reply value as an indented tree. Arrays that look like
/// flat key/value pair lists become `key: value` lines; nested arrays
/// indent one level per depth.
fn pretty_value(value: &Value, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    match value {
        Value::Array(items) | Value::Set(items) => {
            // `[label, scalar]` and alternating pair lists read best as lines.
            if let Some(pairs) = extract_scalar_pairs(items) {
                for (k, v) in pairs {
                    out.push_str(&format!("{indent}{k}: {v}\n"));
                }
                return;
            }
            for item in items {
                match parse_simple_string(item) {
                    Some(s) => out.push_str(&format!("{indent}{s}\n")),
                    None => pretty_value(item, depth + 1, out),
                }
            }
        }
        Value::Map(items) => {
            for (k, v) in items {
                let key = parse_simple_string(k).unwrap_or_default();
                match parse_simple_string(v) {
                    Some(s) => out.push_str(&format!("{indent}{key}: {s}\n")),
                    None => {
                        out.push_str(&format!("{indent}{key}:\n"));
                        pretty_value(v, depth + 1, out);
                    }
                }
            }
        }
        other => {
            if let Some(s) = parse_simple_string(other) {
                out.push_str(&format!("{indent}{s}\n"));
            }
        }
    }
}

/// `[k, v, k, v, …]` with every element a scalar — the flat pair shape
/// profile entries use. `None` when anything nests or the arity is odd.
fn extract_scalar_pairs(items: &[Value]) -> Option<Vec<(String, String)>> {
    if items.is_empty() || !items.len().is_multiple_of(2) {
        return None;
    }
    items
        .chunks(2)
        .map(|pair| Some((parse_simple_string(&pair[0])?, parse_simple_string(&pair[1])?)))
        .collect()
}

fn parse_aggregate(value: &Value) -> Option<AggregateResult> {
    let items = match value {
        Value::Array(items) => items,
        _ => return None,
    };
    let mut iter = items.iter();
    let total = iter.next().and_then(parse_int).unwrap_or_default().max(0) as u64;
    let mut rows = Vec::new();
    for row_val in iter {
        if let Some(pairs) = extract_pairs(row_val) {
            rows.push(
                pairs
                    .into_iter()
                    .map(|(k, v)| (k, parse_simple_string(&v).unwrap_or_default()))
                    .collect(),
            );
        }
    }
    Some(AggregateResult { total, rows })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bs(s: &str) -> Value {
        Value::BulkString(s.as_bytes().to_vec())
    }

    #[test]
    fn parses_modern_info_with_attributes() {
        // Minimal but realistic FT.INFO response.
        let raw = Value::Array(vec![
            bs("index_name"),
            bs("idx:posts"),
            bs("num_docs"),
            Value::Int(42),
            bs("attributes"),
            Value::Array(vec![
                Value::Array(vec![
                    bs("identifier"),
                    bs("title"),
                    bs("attribute"),
                    bs("title"),
                    bs("type"),
                    bs("TEXT"),
                    bs("WEIGHT"),
                    bs("2"),
                    bs("SORTABLE"),
                ]),
                Value::Array(vec![
                    bs("identifier"),
                    bs("tags"),
                    bs("attribute"),
                    bs("tags"),
                    bs("type"),
                    bs("TAG"),
                    bs("SEPARATOR"),
                    bs(","),
                ]),
            ]),
            bs("index_definition"),
            Value::Array(vec![
                bs("key_type"),
                bs("HASH"),
                bs("prefixes"),
                Value::Array(vec![bs("post:")]),
            ]),
        ]);
        let info = parse_info(&raw).expect("parse failed");
        assert_eq!(info.num_docs, 42);
        assert_eq!(info.fields.len(), 2);
        let title = &info.fields[0];
        assert_eq!(title.name.as_str(), "title");
        assert_eq!(title.kind(), FieldKind::Text);
        assert!(title.sortable);
        assert_eq!(title.weight, Some(2.0));
        let tags = &info.fields[1];
        assert_eq!(tags.kind(), FieldKind::Tag);
        assert_eq!(tags.separator.as_ref().map(|s| s.as_ref()), Some(","));
        assert_eq!(info.key_type.as_str(), "HASH");
        assert_eq!(info.prefixes, vec![String::from("post:")]);
    }

    #[test]
    fn parses_legacy_fields_alias() {
        // RediSearch 1.x used `fields` instead of `attributes` and a bare
        // string as the first element.
        let raw = Value::Array(vec![
            bs("fields"),
            Value::Array(vec![Value::Array(vec![
                bs("title"),
                bs("type"),
                bs("TEXT"),
                bs("WEIGHT"),
                bs("1"),
            ])]),
        ]);
        let info = parse_info(&raw).expect("parse failed");
        assert_eq!(info.fields.len(), 1);
        assert_eq!(info.fields[0].name.as_str(), "title");
        assert_eq!(info.fields[0].kind(), FieldKind::Text);
    }

    #[test]
    fn parses_search_response() {
        let raw = Value::Array(vec![
            Value::Int(2),
            bs("post:1"),
            Value::Array(vec![bs("title"), bs("hello"), bs("body"), bs("world")]),
            bs("post:7"),
            Value::Array(vec![bs("title"), bs("foo")]),
        ]);
        let r = parse_search(&raw).expect("parse failed");
        assert_eq!(r.total, 2);
        assert_eq!(r.hits.len(), 2);
        assert_eq!(r.hits[0].doc_id.as_str(), "post:1");
        assert_eq!(r.hits[0].fields.len(), 2);
        assert_eq!(r.hits[0].fields[0].0.as_str(), "title");
        assert_eq!(r.hits[0].fields[0].1.as_str(), "hello");
    }

    #[test]
    fn parses_aggregate_response() {
        let raw = Value::Array(vec![
            Value::Int(3),
            Value::Array(vec![bs("tag"), bs("rust"), bs("cnt"), bs("12")]),
            Value::Array(vec![bs("tag"), bs("go"), bs("cnt"), bs("7")]),
            Value::Array(vec![bs("tag"), bs("zig"), bs("cnt"), bs("3")]),
        ]);
        let r = parse_aggregate(&raw).expect("parse failed");
        assert_eq!(r.total, 3);
        assert_eq!(r.rows.len(), 3);
        assert_eq!(r.rows[0][0], (String::from("tag"), String::from("rust")));
        assert_eq!(r.rows[0][1], (String::from("cnt"), String::from("12")));
    }

    #[test]
    fn reducer_arity_is_correct() {
        assert_eq!(ReducerFn::Count.arity(), 0);
        assert_eq!(ReducerFn::Sum.arity(), 1);
        assert_eq!(ReducerFn::Quantile.arity(), 2);
        assert_eq!(ReducerFn::FirstValue.arity(), 1);
    }

    #[test]
    fn profile_section_and_pretty_printer() {
        // RESP2 shape: [results, profile]. The profile mixes flat pair
        // lists with nested iterator arrays.
        let raw = Value::Array(vec![
            Value::Array(vec![Value::Int(0)]),
            Value::Array(vec![
                Value::Array(vec![bs("Total profile time"), bs("0.5")]),
                Value::Array(vec![
                    bs("Iterators profile"),
                    Value::Array(vec![bs("Type"), bs("UNION"), bs("Time"), bs("0.2")]),
                ]),
            ]),
        ]);
        let mut out = String::new();
        pretty_value(profile_section(&raw), 0, &mut out);
        assert!(out.contains("Total profile time: 0.5"), "{out}");
        assert!(out.contains("Type: UNION"), "{out}");

        // RESP3 shape: a map with a Profile entry.
        let raw = Value::Map(vec![
            (bs("Results"), Value::Array(vec![Value::Int(0)])),
            (bs("Profile"), Value::Map(vec![(bs("Total profile time"), bs("1.25"))])),
        ]);
        let mut out = String::new();
        pretty_value(profile_section(&raw), 0, &mut out);
        assert!(out.contains("Total profile time: 1.25"), "{out}");
    }

    #[test]
    fn is_numeric_literal_basic() {
        assert!(is_numeric_literal("0.5"));
        assert!(is_numeric_literal("-12"));
        assert!(is_numeric_literal("3.14e10"));
        assert!(!is_numeric_literal("hello"));
        assert!(!is_numeric_literal("@field"));
    }
}
