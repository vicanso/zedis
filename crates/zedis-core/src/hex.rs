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
        out.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

/// Lowercase hex digits by nibble — a byte is two table lookups, where a
/// `format!` per byte was a heap allocation per byte of the value.
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
const HEX_UPPER_DIGITS: &[u8; 16] = b"0123456789ABCDEF";

fn push_hex_byte(out: &mut String, digits: &[u8; 16], byte: u8) {
    out.push(char::from(digits[usize::from(byte >> 4)]));
    out.push(char::from(digits[usize::from(byte & 0x0f)]));
}

fn push_escaped_byte(out: &mut String, byte: u8) {
    out.push_str("\\x");
    push_hex_byte(out, HEX_DIGITS, byte);
}

fn push_offset_hex(out: &mut String, offset: usize, width: usize) {
    for shift in (0..width).rev() {
        let nibble = (offset >> (shift * 4)) & 0x0f;
        out.push(char::from(HEX_UPPER_DIGITS[nibble]));
    }
}

/// Render `bytes` as a classic hex + ASCII dump (`0000: 00 0f ...  |...|`).
///
/// Row offsets use 4 uppercase hex digits up to 64 KiB and 8 digits above.
/// When `bytes_per_row` is at least 16 and even, an extra midpoint space is
/// inserted between the two halves for readability.
pub fn bytes_to_hex_dump(bytes: &[u8], bytes_per_row: usize) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    let row = bytes_per_row.max(1);
    let offset_width = if bytes.len() <= 0x1_0000 { 4 } else { 8 };
    let split_after = (row >= 16 && row.is_multiple_of(2)).then_some(row / 2 - 1);
    let row_count = bytes.len().div_ceil(row);
    let per_row_cap = offset_width + 2 + row * 3 + 4 + row + 1;
    let mut out = String::with_capacity(row_count * per_row_cap);

    for (row_ix, chunk) in bytes.chunks(row).enumerate() {
        if row_ix > 0 {
            out.push('\n');
        }
        push_offset_hex(&mut out, row_ix * row, offset_width);
        out.push_str(": ");

        for i in 0..row {
            if i > 0 {
                out.push(' ');
                if split_after == Some(i - 1) {
                    out.push(' ');
                }
            }
            if let Some(&b) = chunk.get(i) {
                push_hex_byte(&mut out, HEX_DIGITS, b);
            } else {
                out.push_str("  ");
            }
        }

        out.push_str("  |");
        for &b in chunk {
            if b.is_ascii_graphic() || b == b' ' {
                out.push(char::from(b));
            } else {
                out.push('.');
            }
        }
        out.push('|');
    }

    out
}

fn push_valid_str_escaped(out: &mut String, s: &str) {
    let mut buf = [0u8; 4];
    for ch in s.chars() {
        if ch.is_control() {
            for &b in ch.encode_utf8(&mut buf).as_bytes() {
                push_escaped_byte(out, b);
            }
        } else {
            out.push(ch);
        }
    }
}

/// Render arbitrary bytes as mixed readable UTF-8 text: printable ASCII and
/// valid non-control UTF-8 characters (such as CJK) appear verbatim, while
/// control characters and invalid UTF-8 bytes are shown as lowercase `\xNN`.
pub fn bytes_to_readable_text(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut rest = bytes;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(valid) => {
                push_valid_str_escaped(&mut out, valid);
                break;
            }
            Err(err) => {
                let valid_up_to = err.valid_up_to();
                if valid_up_to > 0
                    && let Ok(valid) = std::str::from_utf8(&rest[..valid_up_to])
                {
                    push_valid_str_escaped(&mut out, valid);
                }
                let bad_len = err.error_len().unwrap_or(rest.len() - valid_up_to);
                for &b in &rest[valid_up_to..valid_up_to + bad_len] {
                    push_escaped_byte(&mut out, b);
                }
                rest = &rest[valid_up_to + bad_len..];
            }
        }
    }
    out
}

/// Like [`bytes_to_readable_text`], bounded to `max_bytes` of input without
/// splitting a trailing multi-byte UTF-8 character.
pub fn bytes_to_readable_preview(bytes: &[u8], max_bytes: usize) -> String {
    if bytes.len() <= max_bytes {
        return bytes_to_readable_text(bytes);
    }
    let mut cut = max_bytes;
    if cut > 0 && (bytes[cut] & 0xC0) == 0x80 {
        for back in 1..=3.min(cut) {
            match std::str::from_utf8(&bytes[cut - back..cut]) {
                Ok(_) => break,
                Err(err) if err.error_len().is_none() => {
                    cut -= back;
                    break;
                }
                Err(_) => {}
            }
        }
    }
    let mut out = bytes_to_readable_text(&bytes[..cut]);
    out.push_str(" …");
    out
}

#[derive(Default)]
struct ReadableRun {
    len: usize,
    alnum: usize,
    non_ascii_alpha: usize,
}

impl ReadableRun {
    fn reset(&mut self) {
        self.len = 0;
        self.alnum = 0;
        self.non_ascii_alpha = 0;
    }

    fn scan(&mut self, s: &str) -> bool {
        for ch in s.chars() {
            if ch.is_control() {
                self.reset();
                continue;
            }
            self.len += 1;
            if ch.is_ascii_alphanumeric() {
                self.alnum += 1;
            } else if !ch.is_ascii() && ch.is_alphabetic() {
                self.non_ascii_alpha += 1;
            }
            if (self.len >= 4 && self.alnum >= 2) || self.non_ascii_alpha >= 2 {
                return true;
            }
        }
        false
    }
}

/// Returns `true` when `bytes` contains at least one contiguous run of
/// readable text (such as field names or string literals inside FST / Kryo /
/// Protobuf payloads), rather than pure control/binary bytes.
pub fn has_readable_text(bytes: &[u8]) -> bool {
    let mut run = ReadableRun::default();
    let mut rest = bytes;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(valid) => return run.scan(valid),
            Err(err) => {
                let valid_up_to = err.valid_up_to();
                if valid_up_to > 0
                    && let Ok(valid) = std::str::from_utf8(&rest[..valid_up_to])
                    && run.scan(valid)
                {
                    return true;
                }
                run.reset();
                let bad_len = err.error_len().unwrap_or(rest.len() - valid_up_to);
                rest = &rest[valid_up_to + bad_len..];
            }
        }
    }
    false
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

    #[test]
    fn hex_dump_formats_rows_and_ascii_columns() {
        assert_eq!(bytes_to_hex_dump(&[], 16), "");

        let mut payload = vec![0x00, 0x0f, 0xfc, 0x07];
        payload.extend_from_slice(b"EV-H606");
        payload.extend_from_slice(&[0xf7, 0x28, 0x00, 0x04, 0xff, b'Z', b'\n']);
        let dump = bytes_to_hex_dump(&payload, 16);
        let expected = "\
0000: 00 0f fc 07 45 56 2d 48  36 30 36 f7 28 00 04 ff  |....EV-H606.(...|
0010: 5a 0a                                             |Z.|";
        assert_eq!(dump, expected);

        let compact = bytes_to_hex_dump(b"Hi!\x00", 8);
        assert_eq!(compact, "0000: 48 69 21 00              |Hi!.|");

        let large = vec![b'A'; 0x1_0001];
        let large_dump = bytes_to_hex_dump(&large, 16);
        assert!(large_dump.starts_with("00000000: 41 41"));
        assert!(large_dump.contains("\n00010000: 41 "));
    }

    #[test]
    fn readable_text_preserves_utf8_and_escapes_control_or_invalid_bytes() {
        let mut fst_like = vec![0x00, 0x0f, 0xfc, 0x07];
        fst_like.extend_from_slice(b"EV-H606");
        fst_like.extend_from_slice(&[0xf7, 0x28, 0x00, 0x04, 0xff]);
        fst_like.extend_from_slice("温度".as_bytes());
        fst_like.extend_from_slice(b"\n\t\x7f");

        assert_eq!(
            bytes_to_readable_text(&fst_like),
            "\\x00\\x0f\\xfc\\x07EV-H606\\xf7(\\x00\\x04\\xff温度\\x0a\\x09\\x7f"
        );
    }

    #[test]
    fn readable_preview_truncates_without_splitting_utf8() {
        let mut data = b"ab".to_vec();
        data.extend_from_slice("温度计".as_bytes());
        // "温度计" starts at offset 2: "温" is [2..5], "度" is [5..8].
        // Cutting at 6 falls inside "度", so preview backs off to 5 ("ab温 …").
        assert_eq!(bytes_to_readable_preview(&data, 6), "ab温 …");
        assert_eq!(bytes_to_readable_preview(&data, data.len()), "ab温度计");
    }

    #[test]
    fn has_readable_text_filters_noise_and_detects_strings() {
        assert!(!has_readable_text(&[0x00, 0xff, 0x10]));
        assert!(!has_readable_text(b"    ....    "));
        assert!(!has_readable_text(&[0x00, b'a', 0x00, b'b', 0x00]));
        assert!(has_readable_text(&[
            0x00, 0x0f, 0xfc, 0x07, b'E', b'V', b'-', b'H', 0x00
        ]));
        assert!(has_readable_text(&[0xff, 0xfe, 0xe6, 0xb8, 0xa9, 0xe5, 0xba, 0xa6]));
    }
}
