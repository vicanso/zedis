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

//! Hex encode / decode helpers for the bytes editor's hex write mode.
//!
//! Format choice: lowercase, space-separated bytes, newline every
//! `bytes_per_row` bytes. The parser ignores all whitespace, commas, and
//! `0x` prefixes so the row width is purely a presentation choice.

/// Render a byte slice as a hex string with line wrapping.
///
/// `bytes_per_row` controls where the renderer inserts a newline.
/// `0` or `1` collapse to a single per-row byte (degenerate but safe);
/// callers should pick something like 16 / 32 based on viewport width.
/// Empty input returns an empty string.
pub fn bytes_to_hex_text(bytes: &[u8], bytes_per_row: usize) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    let row = bytes_per_row.max(1);
    let mut out = String::with_capacity(bytes.len() * 3);
    for (i, byte) in bytes.iter().enumerate() {
        if i > 0 {
            if i % row == 0 {
                out.push('\n');
            } else {
                out.push(' ');
            }
        }
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Parse a user-edited hex string back into raw bytes.
///
/// Lenient input: ignores ASCII whitespace, commas, and `0x` / `0X`
/// prefixes (so users can paste output from `printf "%#x"` or comma-
/// separated lists). Returns `Err` with a short human-readable message
/// when input contains non-hex characters or has an odd nibble count.
pub fn parse_hex_text(input: &str) -> Result<Vec<u8>, String> {
    // Strip whitespace + common decorations in one pass.
    let mut cleaned = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_whitespace() || c == ',' {
            continue;
        }
        if c == '0' && matches!(chars.peek(), Some('x' | 'X')) {
            chars.next();
            continue;
        }
        cleaned.push(c);
    }
    if cleaned.is_empty() {
        return Ok(Vec::new());
    }
    if !cleaned.len().is_multiple_of(2) {
        return Err(format!("odd number of hex digits ({})", cleaned.len()));
    }
    let mut bytes = Vec::with_capacity(cleaned.len() / 2);
    for pair in cleaned.as_bytes().chunks(2) {
        // pair is guaranteed length 2 by the even-length check above.
        let s = std::str::from_utf8(pair).map_err(|e| e.to_string())?;
        let b = u8::from_str_radix(s, 16).map_err(|_| format!("invalid hex pair `{s}`"))?;
        bytes.push(b);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_round_trip() {
        assert_eq!(bytes_to_hex_text(&[], 16), "");
        assert_eq!(parse_hex_text(""), Ok(Vec::new()));
    }

    #[test]
    fn round_trip_basic() {
        let original: Vec<u8> = (0u8..=255).collect();
        for row in [8usize, 16, 24, 32] {
            let text = bytes_to_hex_text(&original, row);
            let parsed = parse_hex_text(&text).expect("parse must succeed");
            assert_eq!(parsed, original, "round-trip failed at row width {row}");
        }
    }

    #[test]
    fn row_width_controls_line_breaks() {
        let bytes: Vec<u8> = (0..32).collect();

        let narrow = bytes_to_hex_text(&bytes, 16);
        assert_eq!(narrow.matches('\n').count(), 1);
        for line in narrow.split('\n') {
            assert_eq!(line.split_whitespace().count(), 16);
        }

        let wide = bytes_to_hex_text(&bytes, 32);
        // 32 bytes at 32/row ⇒ a single line, no newline.
        assert_eq!(wide.matches('\n').count(), 0);
        assert_eq!(wide.split_whitespace().count(), 32);
    }

    #[test]
    fn zero_row_width_falls_back_to_one() {
        // Defensive: callers passing 0 shouldn't crash.
        let text = bytes_to_hex_text(&[0x00, 0x01, 0x02], 0);
        assert_eq!(text.matches('\n').count(), 2);
    }

    #[test]
    fn parses_lenient_input() {
        // Spaces, newlines, commas, 0x prefixes — all should be accepted.
        let parsed = parse_hex_text("0x00, 0x01\n  02 03  ff").unwrap_or_default();
        assert_eq!(parsed, vec![0x00, 0x01, 0x02, 0x03, 0xff]);
    }

    #[test]
    fn parses_uppercase() {
        assert_eq!(parse_hex_text("AB CD ef").unwrap_or_default(), vec![0xAB, 0xCD, 0xEF]);
    }

    #[test]
    fn rejects_odd_length() {
        let err = parse_hex_text("abc").expect_err("odd");
        assert!(err.contains("odd"));
    }

    #[test]
    fn rejects_non_hex_chars() {
        let err = parse_hex_text("zz").expect_err("invalid hex");
        assert!(err.contains("invalid hex"));
    }
}
