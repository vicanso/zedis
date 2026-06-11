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

//! Minimal RFC 4180 CSV writer for exporting key / search-result tables.

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

#[cfg(test)]
mod tests {
    use super::build_csv;

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
}
