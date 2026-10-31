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

//! Keyspace schema inference and Data Dictionary export (Markdown / JSON).
//!
//! Collapses concrete key names (`user:10042:profile` → `user:{id}:profile`)
//! and aggregates per-pattern type/encoding mix, TTL policy, memory size
//! percentiles (p50 / p95 / max), example keys, and optional field structure
//! (Hash fields, JSON object keys, Stream fields) without retaining any key
//! values.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Maximum distinct collapsed patterns tracked in one [`SchemaAccumulator`].
pub const MAX_SCHEMA_PATTERNS: usize = 2_000;

/// Maximum segments preserved before collapsing deeper segments into `{*}`.
const MAX_PATTERN_SEGMENTS: usize = 6;

/// Maximum size samples retained per pattern for percentile estimation.
const MAX_SIZE_SAMPLES_PER_PATTERN: usize = 128;

/// Maximum concrete example keys kept per pattern.
const MAX_EXAMPLES_PER_PATTERN: usize = 3;

/// Maximum distinct fields tracked per pattern (Hash / JSON / Stream).
pub const MAX_FIELDS_PER_PATTERN: usize = 32;

/// Inferred TTL policy for a collapsed key pattern.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TtlPolicy {
    /// Every sampled key under this pattern has `TTL == -1` (never expires).
    Permanent,
    /// Every sampled key expires, with TTLs clustered around `approx_secs`.
    Fixed { approx_secs: u64 },
    /// Every sampled key expires, spread between `min_secs` and `max_secs`.
    Spread {
        min_secs: u64,
        max_secs: u64,
        avg_secs: u64,
    },
    /// Mix of permanent (`TTL == -1`) and expiring keys (`perm_pct`% permanent).
    Mixed { perm_pct: u8, avg_ttl_secs: u64 },
}

impl TtlPolicy {
    /// Compact English summary for Markdown/JSON exports and table cells.
    pub fn summary(&self) -> String {
        match self {
            Self::Permanent => "Permanent (no TTL)".to_string(),
            Self::Fixed { approx_secs } => format!("Fixed (~{})", format_compact_secs(*approx_secs)),
            Self::Spread {
                min_secs,
                max_secs,
                avg_secs,
            } => format!(
                "TTL {}..{} (avg {})",
                format_compact_secs(*min_secs),
                format_compact_secs(*max_secs),
                format_compact_secs(*avg_secs)
            ),
            Self::Mixed { perm_pct, avg_ttl_secs } => {
                format!("Mixed ({perm_pct}% no TTL, avg {})", format_compact_secs(*avg_ttl_secs))
            }
        }
    }
}

fn format_compact_secs(secs: u64) -> String {
    if secs >= 86_400 {
        let d = secs / 86_400;
        let h = (secs % 86_400) / 3_600;
        if h == 0 { format!("{d}d") } else { format!("{d}d{h}h") }
    } else if secs >= 3_600 {
        let h = secs / 3_600;
        let m = (secs % 3_600) / 60;
        if m == 0 { format!("{h}h") } else { format!("{h}h{m}m") }
    } else if secs >= 60 {
        let m = secs / 60;
        let s = secs % 60;
        if s == 0 { format!("{m}m") } else { format!("{m}m{s}s") }
    } else {
        format!("{secs}s")
    }
}

/// One inferred field inside a Hash, JSON document, or Stream pattern.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InferredField {
    pub name: String,
    pub value_types: Vec<String>,
    /// Percentage of inspected keys in this pattern that contained this field (1..=100).
    pub presence_pct: u8,
}

/// Inferred schema for a single collapsed key pattern.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatternSchema {
    pub pattern: String,
    pub sampled_keys: u64,
    pub estimated_keys: u64,
    pub estimated_memory_bytes: u64,
    pub memory_share_pct: f64,
    pub p50_bytes: u64,
    pub p95_bytes: u64,
    pub max_bytes: u64,
    pub types: Vec<(String, u64)>,
    pub encodings: Vec<(String, u64)>,
    pub ttl_policy: TtlPolicy,
    pub examples: Vec<String>,
    pub sampled_structures: u64,
    pub fields: Vec<InferredField>,
}

impl PatternSchema {
    pub fn primary_type(&self) -> &str {
        self.types.first().map(|(t, _)| t.as_str()).unwrap_or("unknown")
    }

    pub fn type_summary(&self) -> String {
        if self.types.is_empty() {
            return "unknown".to_string();
        }
        self.types
            .iter()
            .map(|(t, _)| t.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn encoding_summary(&self) -> String {
        if self.encodings.is_empty() {
            return "-".to_string();
        }
        self.encodings
            .iter()
            .map(|(e, _)| e.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn fields_summary(&self) -> String {
        if self.fields.is_empty() {
            return "-".to_string();
        }
        self.fields
            .iter()
            .map(|f| {
                let kind = f.value_types.join("|");
                if f.presence_pct < 100 {
                    format!("{}?: {}", f.name, kind)
                } else {
                    format!("{}: {}", f.name, kind)
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Inferred keyspace data dictionary across all sampled patterns.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DataDictionary {
    pub total_sampled_keys: u64,
    pub estimated_total_keys: u64,
    pub estimated_total_memory_bytes: u64,
    pub sample_ratio: f32,
    pub patterns: Vec<PatternSchema>,
}

impl DataDictionary {
    /// Export the inferred Data Dictionary as a Markdown document.
    pub fn to_markdown(&self, context_label: &str) -> String {
        let mut out = String::with_capacity(4096);
        out.push_str("# Redis Keyspace Data Dictionary\n\n");
        if !context_label.is_empty() {
            out.push_str(&format!("- **Source**: `{context_label}`\n"));
        }
        out.push_str(&format!(
            "- **Sampled Keys**: {} (sample ratio {:.0}%)\n",
            self.total_sampled_keys,
            self.sample_ratio * 100.0
        ));
        out.push_str(&format!("- **Estimated Total Keys**: {}\n", self.estimated_total_keys));
        out.push_str(&format!(
            "- **Estimated Memory**: {} bytes\n",
            self.estimated_total_memory_bytes
        ));
        out.push_str(&format!("- **Inferred Patterns**: {}\n\n", self.patterns.len()));

        out.push_str("## Pattern Summary\n\n");
        out.push_str(
            "| Pattern | Type (Encoding) | Est. Keys | Est. Memory (Share) | Size p50 / p95 / max | TTL Policy | Fields / Structure |\n",
        );
        out.push_str("| --- | --- | ---: | ---: | ---: | --- | --- |\n");

        for entry in &self.patterns {
            let type_enc = if entry.encodings.is_empty() {
                entry.type_summary()
            } else {
                format!("{} ({})", entry.type_summary(), entry.encoding_summary())
            };
            let mem_cell = format!("{} B ({:.1}%)", entry.estimated_memory_bytes, entry.memory_share_pct);
            let size_cell = format!("{} / {} / {} B", entry.p50_bytes, entry.p95_bytes, entry.max_bytes);
            let fields_cell = escape_md_cell(&entry.fields_summary());
            out.push_str(&format!(
                "| `{}` | {} | {} | {} | {} | {} | {} |\n",
                escape_md_cell(&entry.pattern),
                escape_md_cell(&type_enc),
                entry.estimated_keys,
                mem_cell,
                size_cell,
                escape_md_cell(&entry.ttl_policy.summary()),
                fields_cell,
            ));
        }

        let detailed: Vec<&PatternSchema> = self
            .patterns
            .iter()
            .filter(|p| !p.fields.is_empty() || !p.examples.is_empty())
            .take(50)
            .collect();
        if !detailed.is_empty() {
            out.push_str("\n## Pattern Details\n");
            for entry in detailed {
                out.push_str(&format!("\n### `{}`\n\n", entry.pattern));
                out.push_str(&format!(
                    "- **Type**: `{}` · **Encoding**: `{}`\n",
                    entry.type_summary(),
                    entry.encoding_summary()
                ));
                out.push_str(&format!(
                    "- **Estimated Keys**: {} (sampled {}) · **Estimated Memory**: {} B ({:.1}%)\n",
                    entry.estimated_keys, entry.sampled_keys, entry.estimated_memory_bytes, entry.memory_share_pct
                ));
                out.push_str(&format!(
                    "- **Size Distribution**: p50 = {} B, p95 = {} B, max = {} B\n",
                    entry.p50_bytes, entry.p95_bytes, entry.max_bytes
                ));
                out.push_str(&format!("- **TTL Policy**: {}\n", entry.ttl_policy.summary()));
                if !entry.examples.is_empty() {
                    let ex = entry
                        .examples
                        .iter()
                        .map(|k| format!("`{k}`"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    out.push_str(&format!("- **Example Keys**: {ex}\n"));
                }
                if !entry.fields.is_empty() {
                    out.push_str("- **Inferred Fields**:\n");
                    for field in &entry.fields {
                        out.push_str(&format!(
                            "  - `{}`: `{}` (present in {}% of sampled keys)\n",
                            field.name,
                            field.value_types.join(" | "),
                            field.presence_pct
                        ));
                    }
                }
            }
        }

        out
    }

    /// Export the inferred Data Dictionary as pretty-printed JSON.
    pub fn to_json_pretty(&self, context_label: &str) -> String {
        #[derive(Serialize)]
        struct Envelope<'a> {
            source: &'a str,
            #[serde(flatten)]
            dictionary: &'a DataDictionary,
        }
        serde_json::to_string_pretty(&Envelope {
            source: context_label,
            dictionary: self,
        })
        .unwrap_or_else(|_| "{}".to_string())
    }
}

fn escape_md_cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

/// Collapse dynamic identifier segments (`12345`, UUIDs, ISO dates, hex hashes,
/// long random tokens) in `key` into placeholders (`{id}`, `{uuid}`, `{date}`,
/// `{hash}`, `{token}`).
pub fn collapse_key_pattern(key: &str, separator: &str) -> String {
    if separator.is_empty() || !key.contains(separator) {
        return key.to_string();
    }
    let mut parts = Vec::new();
    for (idx, seg) in key.split(separator).enumerate() {
        if idx >= MAX_PATTERN_SEGMENTS {
            parts.push("{*}");
            break;
        }
        parts.push(classify_segment(seg));
    }
    parts.join(separator)
}

fn classify_segment(seg: &str) -> &str {
    if seg.is_empty() {
        return seg;
    }
    if is_uuid_segment(seg) {
        return "{uuid}";
    }
    if is_date_segment(seg) {
        return "{date}";
    }
    if is_integer_segment(seg) {
        return "{id}";
    }
    if is_hex_hash_segment(seg) {
        return "{hash}";
    }
    if is_token_segment(seg) {
        return "{token}";
    }
    seg
}

fn is_uuid_segment(seg: &str) -> bool {
    let bytes = seg.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, &b) in bytes.iter().enumerate() {
        if matches!(i, 8 | 13 | 18 | 23) {
            if b != b'-' {
                return false;
            }
        } else if !b.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

fn is_date_segment(seg: &str) -> bool {
    let bytes = seg.as_bytes();
    // YYYY-MM-DD
    if bytes.len() == 10 && bytes[4] == b'-' && bytes[7] == b'-' {
        let year_ok = bytes[0..4].iter().all(u8::is_ascii_digit) && (bytes[0] == b'1' || bytes[0] == b'2');
        let month = parse_two_digits(bytes[5], bytes[6]);
        let day = parse_two_digits(bytes[8], bytes[9]);
        return year_ok && matches!(month, Some(1..=12)) && matches!(day, Some(1..=31));
    }
    // YYYYMMDD (only when year starts with 19 or 20 and month/day are valid)
    if bytes.len() == 8 && bytes.iter().all(u8::is_ascii_digit) {
        let prefix_ok = (bytes[0] == b'1' && bytes[1] == b'9') || (bytes[0] == b'2' && bytes[1] == b'0');
        let month = parse_two_digits(bytes[4], bytes[5]);
        let day = parse_two_digits(bytes[6], bytes[7]);
        return prefix_ok && matches!(month, Some(1..=12)) && matches!(day, Some(1..=31));
    }
    false
}

fn parse_two_digits(hi: u8, lo: u8) -> Option<u8> {
    if hi.is_ascii_digit() && lo.is_ascii_digit() {
        Some((hi - b'0') * 10 + (lo - b'0'))
    } else {
        None
    }
}

fn is_integer_segment(seg: &str) -> bool {
    let bytes = seg.as_bytes();
    let digits = if bytes.first() == Some(&b'-') {
        &bytes[1..]
    } else {
        bytes
    };
    !digits.is_empty() && digits.iter().all(u8::is_ascii_digit)
}

fn is_hex_hash_segment(seg: &str) -> bool {
    let bytes = seg.as_bytes();
    if bytes.len() < 16 {
        return false;
    }
    bytes.iter().all(u8::is_ascii_hexdigit) && bytes.iter().any(u8::is_ascii_digit)
}

fn is_token_segment(seg: &str) -> bool {
    let bytes = seg.as_bytes();
    if bytes.len() < 20 {
        return false;
    }
    let mut digits = 0usize;
    let mut letters = 0usize;
    for &b in bytes {
        if b.is_ascii_digit() {
            digits += 1;
        } else if b.is_ascii_alphabetic() {
            letters += 1;
        } else if b != b'_' && b != b'-' {
            return false;
        }
    }
    digits >= 3 && letters >= 3
}

/// Infer the scalar value type of a raw Redis field/element payload without
/// retaining its value.
pub fn infer_scalar_type(bytes: &[u8]) -> &'static str {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return "binary";
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "string";
    }
    if trimmed.eq_ignore_ascii_case("true") || trimmed.eq_ignore_ascii_case("false") {
        return "bool";
    }
    if is_integer_segment(trimmed) && trimmed.parse::<i64>().is_ok() {
        return "int";
    }
    if trimmed.parse::<f64>().is_ok_and(f64::is_finite) {
        return "float";
    }
    if trimmed.starts_with('{') && serde_json::from_str::<Value>(trimmed).is_ok_and(|v| v.is_object()) {
        return "json";
    }
    if trimmed.starts_with('[') && serde_json::from_str::<Value>(trimmed).is_ok_and(|v| v.is_array()) {
        return "array";
    }
    "string"
}

/// Extract top-level `(field_name, value_type)` pairs if `bytes` is a JSON object.
pub fn infer_json_object_fields(bytes: &[u8]) -> Vec<(String, String)> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Vec::new();
    };
    let trimmed = text.trim();
    if !trimmed.starts_with('{') {
        return Vec::new();
    }
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(trimmed) else {
        return Vec::new();
    };
    map.into_iter()
        .take(MAX_FIELDS_PER_PATTERN)
        .map(|(k, v)| (k, json_value_type_name(&v).to_string()))
        .collect()
}

fn json_value_type_name(val: &Value) -> &'static str {
    match val {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "int"
            } else {
                "float"
            }
        }
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[derive(Default, Clone)]
struct FieldAcc {
    count: u64,
    value_types: BTreeSet<String>,
}

#[derive(Default, Clone)]
struct PatternAcc {
    sampled_keys: u64,
    total_memory_bytes: u64,
    max_memory_bytes: u64,
    size_samples: Vec<u64>,
    types: BTreeMap<String, u64>,
    encodings: BTreeMap<String, u64>,
    no_ttl_count: u64,
    ttl_count: u64,
    min_ttl_secs: i64,
    max_ttl_secs: i64,
    ttl_sum_secs: i64,
    examples: Vec<String>,
    sampled_structures: u64,
    fields: BTreeMap<String, FieldAcc>,
}

/// Streaming accumulator that builds a [`DataDictionary`] from sampled keys.
#[derive(Default, Clone)]
pub struct SchemaAccumulator {
    patterns: HashMap<String, PatternAcc>,
    total_sampled_keys: u64,
    total_sampled_memory: u64,
}

impl SchemaAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.total_sampled_keys == 0
    }

    /// Record one sampled key into its collapsed pattern bucket.
    pub fn record_key(
        &mut self,
        key: &str,
        key_type: &str,
        encoding: &str,
        memory_bytes: u64,
        ttl_secs: i64,
        separator: &str,
    ) {
        self.total_sampled_keys += 1;
        self.total_sampled_memory += memory_bytes;

        let collapsed = collapse_key_pattern(key, separator);
        if !self.patterns.contains_key(&collapsed) && self.patterns.len() >= MAX_SCHEMA_PATTERNS {
            return;
        }
        let acc = self.patterns.entry(collapsed).or_default();
        acc.sampled_keys += 1;
        acc.total_memory_bytes += memory_bytes;
        acc.max_memory_bytes = acc.max_memory_bytes.max(memory_bytes);
        if acc.size_samples.len() < MAX_SIZE_SAMPLES_PER_PATTERN {
            acc.size_samples.push(memory_bytes);
        } else {
            let slot = (acc.sampled_keys as usize) % MAX_SIZE_SAMPLES_PER_PATTERN;
            acc.size_samples[slot] = memory_bytes;
        }

        if !key_type.is_empty() && key_type != "none" {
            *acc.types.entry(key_type.to_string()).or_insert(0) += 1;
        }
        if !encoding.is_empty() {
            *acc.encodings.entry(encoding.to_string()).or_insert(0) += 1;
        }

        if ttl_secs > 0 {
            if acc.ttl_count == 0 {
                acc.min_ttl_secs = ttl_secs;
                acc.max_ttl_secs = ttl_secs;
            } else {
                acc.min_ttl_secs = acc.min_ttl_secs.min(ttl_secs);
                acc.max_ttl_secs = acc.max_ttl_secs.max(ttl_secs);
            }
            acc.ttl_count += 1;
            acc.ttl_sum_secs += ttl_secs;
        } else if ttl_secs == -1 {
            acc.no_ttl_count += 1;
        }

        if acc.examples.len() < MAX_EXAMPLES_PER_PATTERN && !acc.examples.iter().any(|e| e == key) {
            acc.examples.push(key.to_string());
        }
    }

    /// Attach observed `(field_name, field_type)` pairs from one inspected key
    /// belonging to `pattern`.
    pub fn record_structure_fields(&mut self, pattern: &str, fields: &[(String, String)]) {
        if fields.is_empty() {
            return;
        }
        let Some(acc) = self.patterns.get_mut(pattern) else {
            return;
        };
        acc.sampled_structures += 1;
        for (name, kind) in fields {
            if !acc.fields.contains_key(name) && acc.fields.len() >= MAX_FIELDS_PER_PATTERN {
                continue;
            }
            let entry = acc.fields.entry(name.clone()).or_default();
            entry.count += 1;
            if !kind.is_empty() {
                entry.value_types.insert(kind.clone());
            }
        }
    }

    /// Return up to `limit` `(pattern, primary_type, example_key)` tuples for
    /// the top patterns by memory so the caller can optionally probe their
    /// internal field structure (Hash / JSON / String / Stream).
    pub fn top_probe_targets(&self, limit: usize) -> Vec<(String, String, String)> {
        let mut list: Vec<(&String, &PatternAcc)> = self
            .patterns
            .iter()
            .filter(|(_, acc)| acc.sampled_structures == 0 && !acc.examples.is_empty())
            .collect();
        list.sort_by(|a, b| {
            b.1.total_memory_bytes
                .cmp(&a.1.total_memory_bytes)
                .then_with(|| b.1.sampled_keys.cmp(&a.1.sampled_keys))
                .then_with(|| a.0.cmp(b.0))
        });
        list.into_iter()
            .filter_map(|(pattern, acc)| {
                let primary_type = acc
                    .types
                    .iter()
                    .max_by_key(|(_, count)| *count)
                    .map(|(t, _)| t.clone())?;
                if !matches!(
                    primary_type.as_str(),
                    "hash" | "string" | "ReJSON-RL" | "json" | "stream"
                ) {
                    return None;
                }
                let example = acc.examples.first()?.clone();
                Some((pattern.clone(), primary_type, example))
            })
            .take(limit)
            .collect()
    }

    /// Build the finalized [`DataDictionary`], scaling counts and memory by
    /// `sample_ratio` (`0.0..=1.0`) and keeping up to `max_patterns` rows.
    pub fn finish(&self, sample_ratio: f32, max_patterns: usize) -> DataDictionary {
        let ratio = if sample_ratio > 0.0 && sample_ratio <= 1.0 {
            sample_ratio
        } else {
            1.0
        };
        let scale = 1.0 / ratio;
        let estimated_total_keys = (self.total_sampled_keys as f32 * scale).round() as u64;
        let estimated_total_memory_bytes = (self.total_sampled_memory as f32 * scale).round() as u64;

        let mut patterns: Vec<PatternSchema> = self
            .patterns
            .iter()
            .map(|(pattern, acc)| {
                let estimated_keys = ((acc.sampled_keys as f32 * scale).round() as u64).max(acc.sampled_keys);
                let estimated_memory_bytes = (acc.total_memory_bytes as f32 * scale).round() as u64;
                let memory_share_pct = if self.total_sampled_memory == 0 {
                    0.0
                } else {
                    acc.total_memory_bytes as f64 * 100.0 / self.total_sampled_memory as f64
                };

                let (p50_bytes, p95_bytes) = compute_percentiles(&acc.size_samples);

                let mut types: Vec<(String, u64)> = acc.types.iter().map(|(k, v)| (k.clone(), *v)).collect();
                types.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

                let mut encodings: Vec<(String, u64)> = acc.encodings.iter().map(|(k, v)| (k.clone(), *v)).collect();
                encodings.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

                let ttl_policy = infer_ttl_policy(
                    acc.no_ttl_count,
                    acc.ttl_count,
                    acc.min_ttl_secs,
                    acc.max_ttl_secs,
                    acc.ttl_sum_secs,
                );

                let denom = acc.sampled_structures.max(1);
                let fields: Vec<InferredField> = acc
                    .fields
                    .iter()
                    .map(|(name, f)| {
                        let pct = ((f.count * 100) / denom).clamp(1, 100) as u8;
                        InferredField {
                            name: name.clone(),
                            value_types: f.value_types.iter().cloned().collect(),
                            presence_pct: pct,
                        }
                    })
                    .collect();

                PatternSchema {
                    pattern: pattern.clone(),
                    sampled_keys: acc.sampled_keys,
                    estimated_keys,
                    estimated_memory_bytes,
                    memory_share_pct,
                    p50_bytes,
                    p95_bytes,
                    max_bytes: acc.max_memory_bytes,
                    types,
                    encodings,
                    ttl_policy,
                    examples: acc.examples.clone(),
                    sampled_structures: acc.sampled_structures,
                    fields,
                }
            })
            .collect();

        patterns.sort_by(|a, b| {
            b.estimated_memory_bytes
                .cmp(&a.estimated_memory_bytes)
                .then_with(|| b.estimated_keys.cmp(&a.estimated_keys))
                .then_with(|| a.pattern.cmp(&b.pattern))
        });
        if max_patterns > 0 && patterns.len() > max_patterns {
            patterns.truncate(max_patterns);
        }

        DataDictionary {
            total_sampled_keys: self.total_sampled_keys,
            estimated_total_keys,
            estimated_total_memory_bytes,
            sample_ratio: ratio,
            patterns,
        }
    }
}

fn compute_percentiles(samples: &[u64]) -> (u64, u64) {
    if samples.is_empty() {
        return (0, 0);
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let n = sorted.len();
    let p50 = sorted[(n - 1) / 2];
    let p95_idx = ((n - 1) * 95) / 100;
    let p95 = sorted[p95_idx];
    (p50, p95)
}

fn infer_ttl_policy(
    no_ttl_count: u64,
    ttl_count: u64,
    min_ttl_secs: i64,
    max_ttl_secs: i64,
    ttl_sum_secs: i64,
) -> TtlPolicy {
    if ttl_count == 0 {
        return TtlPolicy::Permanent;
    }
    let avg_secs = (ttl_sum_secs.max(0) as u64) / ttl_count;
    if no_ttl_count > 0 {
        let total = no_ttl_count + ttl_count;
        let perm_pct = ((no_ttl_count * 100) / total).clamp(1, 99) as u8;
        return TtlPolicy::Mixed {
            perm_pct,
            avg_ttl_secs: avg_secs,
        };
    }
    let min_u = min_ttl_secs.max(0) as u64;
    let max_u = max_ttl_secs.max(0) as u64;
    let spread = max_u.saturating_sub(min_u);
    if spread <= (max_u / 10).max(60) {
        TtlPolicy::Fixed { approx_secs: avg_secs }
    } else {
        TtlPolicy::Spread {
            min_secs: min_u,
            max_secs: max_u,
            avg_secs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_numeric_uuid_date_and_hash_segments() {
        assert_eq!(collapse_key_pattern("user:10042:profile", ":"), "user:{id}:profile");
        assert_eq!(
            collapse_key_pattern("order:550e8400-e29b-41d4-a716-446655440000:items", ":"),
            "order:{uuid}:items"
        );
        assert_eq!(collapse_key_pattern("stats:2026-10-31:dau", ":"), "stats:{date}:dau");
        assert_eq!(collapse_key_pattern("stats:20261031:dau", ":"), "stats:{date}:dau");
        assert_eq!(
            collapse_key_pattern("session:e3b0c44298fc1c149afbf4c8996fb924", ":"),
            "session:{hash}"
        );
        assert_eq!(
            collapse_key_pattern("config:feature_flags", ":"),
            "config:feature_flags"
        );
    }

    #[test]
    fn infers_ttl_policy_percentiles_and_fields_into_dictionary() {
        let mut acc = SchemaAccumulator::new();
        acc.record_key("user:1:profile", "hash", "listpack", 200, 3600, ":");
        acc.record_key("user:2:profile", "hash", "listpack", 240, 3580, ":");
        acc.record_key("config:global", "string", "embstr", 64, -1, ":");

        acc.record_structure_fields(
            "user:{id}:profile",
            &[
                ("name".to_string(), "string".to_string()),
                ("age".to_string(), "int".to_string()),
            ],
        );
        acc.record_structure_fields("user:{id}:profile", &[("name".to_string(), "string".to_string())]);

        let dict = acc.finish(0.5, 100);
        assert_eq!(dict.total_sampled_keys, 3);
        assert_eq!(dict.estimated_total_keys, 6);
        assert_eq!(dict.patterns.len(), 2);

        let user_pat = &dict.patterns[0];
        assert_eq!(user_pat.pattern, "user:{id}:profile");
        assert_eq!(user_pat.estimated_keys, 4);
        assert!(matches!(user_pat.ttl_policy, TtlPolicy::Fixed { .. }));
        assert_eq!(user_pat.fields.len(), 2);
        assert_eq!(user_pat.fields[0].name, "age");
        assert_eq!(user_pat.fields[0].presence_pct, 50);
        assert_eq!(user_pat.fields[1].name, "name");
        assert_eq!(user_pat.fields[1].presence_pct, 100);

        let md = dict.to_markdown("local:6379/db0");
        assert!(md.contains("user:{id}:profile"));
        assert!(md.contains("age?: int, name: string"));

        let json = dict.to_json_pretty("local:6379/db0");
        assert!(json.contains("\"pattern\": \"user:{id}:profile\""));
    }

    #[test]
    fn infers_json_object_and_scalar_types_without_keeping_values() {
        let fields = infer_json_object_fields(br#"{"id": 42, "active": true, "tags": ["a"], "name": "alice"}"#);
        assert_eq!(
            fields,
            vec![
                ("id".to_string(), "int".to_string()),
                ("active".to_string(), "bool".to_string()),
                ("tags".to_string(), "array".to_string()),
                ("name".to_string(), "string".to_string()),
            ]
        );
        assert_eq!(infer_scalar_type(b"42"), "int");
        assert_eq!(infer_scalar_type(b"3.14"), "float");
        assert_eq!(infer_scalar_type(b"true"), "bool");
        assert_eq!(infer_scalar_type(b"{\"a\":1}"), "json");
        assert_eq!(infer_scalar_type(b"hello"), "string");
    }
}
