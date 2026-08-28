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

//! Minimal RFC 4180 CSV writer for exporting key / search-result tables,
//! plus the matching reader for re-importing those files.

/// Build a CSV document from a header row and data rows. Fields containing
/// `,`, `"`, or a newline are quoted (with embedded `"` doubled); records are
/// CRLF-terminated per RFC 4180 so the output opens cleanly in spreadsheets.
pub fn build_csv(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut out = String::new();
    write_record(&mut out, headers.iter().copied());
    for row in rows {
        write_record(&mut out, row.iter().map(String::as_str));
    }
    out
}

/// One CRLF-terminated CSV record with the same RFC 4180 quoting as
/// [`build_csv`] — for streaming writers that emit rows chunk by chunk.
pub fn build_csv_record(fields: &[&str]) -> String {
    let mut out = String::new();
    write_record(&mut out, fields.iter().copied());
    out
}

fn write_record<'a>(out: &mut String, fields: impl Iterator<Item = &'a str>) {
    let mut first = true;
    for field in fields {
        if !first {
            out.push(',');
        }
        first = false;
        push_field(out, field);
    }
    out.push_str("\r\n");
}

fn push_field(out: &mut String, field: &str) {
    if field.contains([',', '"', '\n', '\r']) {
        out.push('"');
        for ch in field.chars() {
            if ch == '"' {
                out.push('"');
            }
            out.push(ch);
        }
        out.push('"');
    } else {
        out.push_str(field);
    }
}

/// Parse an RFC 4180 CSV document into records of fields — the inverse of
/// [`build_csv`]. Accepts CRLF or LF record endings; quoted fields may
/// contain commas, doubled quotes (`""` → `"`) and newlines. Lenient where
/// the RFC is strict: a quote inside an unquoted field is taken literally,
/// and an unterminated quote runs to end of input instead of failing —
/// higher layers validate field counts with better error messages than a
/// character offset. Blank lines yield no record.
pub fn parse_csv(input: &str) -> Vec<Vec<String>> {
    let mut records: Vec<Vec<String>> = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    // A record is "pending" once any field data or separator was seen, so
    // `a,,b` keeps its empty fields but a blank line emits nothing.
    let mut pending = false;
    let mut in_quotes = false;
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_quotes {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(ch);
            }
            continue;
        }
        match ch {
            '"' if field.is_empty() => {
                in_quotes = true;
                pending = true;
            }
            ',' => {
                record.push(std::mem::take(&mut field));
                pending = true;
            }
            '\r' if chars.peek() == Some(&'\n') => {} // handled by the '\n'
            '\n' => {
                if pending || !field.is_empty() {
                    record.push(std::mem::take(&mut field));
                    records.push(std::mem::take(&mut record));
                    pending = false;
                }
            }
            _ => {
                field.push(ch);
                pending = true;
            }
        }
    }
    if pending || !field.is_empty() {
        record.push(field);
        records.push(record);
    }
    records
}

#[cfg(test)]
mod tests {
    use super::{build_csv, parse_csv};

    #[test]
    fn header_plain_and_quoted_fields() {
        let csv = build_csv(
            &["key", "type", "ttl"],
            &[
                vec!["a".into(), "string".into(), "60".into()],
                vec!["has,comma".into(), "he\"llo".into(), String::new()],
            ],
        );
        assert_eq!(csv, "key,type,ttl\r\na,string,60\r\n\"has,comma\",\"he\"\"llo\",\r\n");
    }

    #[test]
    fn parse_inverts_build() {
        let rows = vec![
            vec!["a".to_string(), "string".to_string(), "60".to_string()],
            vec!["has,comma".to_string(), "he\"llo".to_string(), String::new()],
            vec!["multi\nline".to_string(), "x\r\ny".to_string(), "z".to_string()],
        ];
        let csv = build_csv(&["key", "type", "ttl"], &rows);
        let parsed = parse_csv(&csv);
        assert_eq!(parsed.len(), 4);
        assert_eq!(parsed[0], ["key", "type", "ttl"]);
        assert_eq!(parsed[1..], rows[..]);
    }

    #[test]
    fn parse_tolerates_lf_blank_lines_and_missing_trailing_newline() {
        let parsed = parse_csv("a,b\n\nc,\nd");
        assert_eq!(
            parsed,
            vec![
                vec!["a".to_string(), "b".to_string()],
                vec!["c".to_string(), String::new()],
                vec!["d".to_string()],
            ]
        );
        // Lenient forms: a stray quote mid-field stays literal, an
        // unterminated quote runs to end of input.
        assert_eq!(parse_csv("ab\"c"), vec![vec!["ab\"c".to_string()]]);
        assert_eq!(parse_csv("\"open,end"), vec![vec!["open,end".to_string()]]);
        assert!(parse_csv("").is_empty());
        assert!(parse_csv("\r\n\n").is_empty());
    }
}
