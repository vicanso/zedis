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

//! One element of a collection — a hash field or value, a list item, a set
//! or sorted-set member — as the editors hold it: the bytes Redis stores,
//! and the text the table shows.
//!
//! The text is decoded the way a string value is (MessagePack, gzip, JWT,
//! …) so a table reads like the value editor; the bytes are what every
//! write and delete sends back, so decoding never touches what is stored.
//! Images are named, not rendered — a cell is one line — and bytes that
//! are neither text nor a known format are shown as hex.

use super::string::detect_and_decode;
use super::value::{DataFormat, detect_format};
use bytes::Bytes;
use gpui::SharedString;
use zedis_core::hex::{bytes_to_hex_text, parse_hex_text};

/// Characters a decoded rendering keeps for the table; the entry panel
/// decodes again, in full, from the bytes.
const ELEMENT_TEXT_CAP: usize = 4096;
/// Bytes shown as hex in a cell before the rest is elided.
const ELEMENT_HEX_CAP: usize = 1024;
/// Bytes per row when a binary element is edited as a hex dump.
const EDIT_HEX_BYTES_PER_ROW: usize = 16;

#[derive(Debug, Clone, PartialEq)]
pub struct KvElement {
    raw: Bytes,
    format: DataFormat,
    text: SharedString,
}

impl KvElement {
    /// The bytes Redis answered with.
    pub fn from_raw(raw: impl Into<Bytes>) -> Self {
        let raw = raw.into();
        let (format, text) = decode_element(&raw);
        Self { raw, format, text }
    }

    /// What the user typed into a form.
    pub fn from_text(text: &str) -> Self {
        Self::from_raw(Bytes::copy_from_slice(text.as_bytes()))
    }

    /// The stored bytes — what every command names the element by.
    pub fn raw(&self) -> &Bytes {
        &self.raw
    }

    /// What the table shows.
    pub fn text(&self) -> &SharedString {
        &self.text
    }

    /// What the bytes were recognised as. `Text` and `Json` are shown as
    /// stored; `Bytes` and the image formats as hex; the rest decoded.
    pub fn format(&self) -> DataFormat {
        self.format
    }

    /// Not UTF-8 text (or holding NUL): shown and edited as hex.
    pub fn is_binary(&self) -> bool {
        is_binary(&self.raw)
    }

    /// The text is a decoding rather than the bytes themselves.
    pub fn is_decoded(&self) -> bool {
        !matches!(self.format, DataFormat::Text | DataFormat::Json | DataFormat::Bytes) && !is_image(self.format)
    }

    /// What the edit form starts from: the text itself, or a hex dump of a
    /// binary element.
    pub fn edit_text(&self) -> SharedString {
        if self.is_binary() {
            bytes_to_hex_text(&self.raw, EDIT_HEX_BYTES_PER_ROW).into()
        } else {
            SharedString::new(String::from_utf8_lossy(&self.raw))
        }
    }

    /// The bytes an edited form value means: parsed hex when this element
    /// is edited as hex, the text's bytes otherwise.
    pub fn bytes_from_edit(&self, edited: &str) -> Result<Bytes, String> {
        if self.is_binary() {
            parse_hex_text(edited).map(Bytes::from)
        } else {
            Ok(Bytes::copy_from_slice(edited.as_bytes()))
        }
    }
}

fn is_binary(raw: &[u8]) -> bool {
    raw.contains(&0) || std::str::from_utf8(raw).is_err()
}

fn is_image(format: DataFormat) -> bool {
    matches!(
        format,
        DataFormat::Svg | DataFormat::Jpeg | DataFormat::Png | DataFormat::Webp | DataFormat::Gif
    )
}

/// The label and the one-line text a cell shows for `raw`.
fn decode_element(raw: &[u8]) -> (DataFormat, SharedString) {
    if raw.is_empty() {
        return (DataFormat::Text, SharedString::default());
    }
    let (sniffed, _) = detect_format(raw);
    if is_image(sniffed) {
        return (sniffed, hex_line(raw));
    }
    let binary = is_binary(raw);
    let (format, decoded) = detect_and_decode(raw, ELEMENT_TEXT_CAP);
    match format {
        // Text stays as stored, JSON included: the cell shows what Redis
        // holds and the form edits exactly that. A number that looks like
        // a timestamp stays the number — a column of counters must not
        // turn into dates.
        DataFormat::Text | DataFormat::Json | DataFormat::Timestamp | DataFormat::Bytes if !binary => {
            let label = if format == DataFormat::Json {
                DataFormat::Json
            } else {
                DataFormat::Text
            };
            (label, SharedString::new(String::from_utf8_lossy(raw)))
        }
        // Nothing decoded and not text: hex.
        DataFormat::Text | DataFormat::Json | DataFormat::Timestamp | DataFormat::Bytes => {
            (DataFormat::Bytes, hex_line(raw))
        }
        // A rendering the pipeline calls a preview is named by what the
        // bytes were sniffed as — MessagePack, gzip, … — where it knows.
        DataFormat::Preview => {
            let label = if matches!(
                sniffed,
                DataFormat::MessagePack | DataFormat::Gzip | DataFormat::Zstd | DataFormat::Snappy
            ) {
                sniffed
            } else {
                DataFormat::Preview
            };
            (label, one_line(&decoded))
        }
        other => (other, one_line(&decoded)),
    }
}

/// `text` on one line, whitespace runs collapsed, cut at the cap.
fn one_line(text: &str) -> SharedString {
    let mut out = String::with_capacity(text.len().min(ELEMENT_TEXT_CAP + 4));
    let mut pending_space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(c);
        if out.len() >= ELEMENT_TEXT_CAP {
            out.push('…');
            break;
        }
    }
    out.into()
}

fn hex_line(raw: &[u8]) -> SharedString {
    let shown = &raw[..raw.len().min(ELEMENT_HEX_CAP)];
    let mut text = bytes_to_hex_text(shown, usize::MAX);
    if shown.len() < raw.len() {
        text.push_str(" …");
    }
    text.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    #[test]
    fn text_and_json_are_shown_as_stored() {
        let plain = KvElement::from_raw(Bytes::from_static(b"hello world"));
        assert_eq!(plain.format(), DataFormat::Text);
        assert_eq!(plain.text().as_ref(), "hello world");
        assert!(!plain.is_binary() && !plain.is_decoded());
        assert_eq!(plain.edit_text().as_ref(), "hello world");

        let json = KvElement::from_raw(Bytes::from_static(br#"{"a":1,"b":[1,2]}"#));
        assert_eq!(json.format(), DataFormat::Json);
        assert_eq!(
            json.text().as_ref(),
            r#"{"a":1,"b":[1,2]}"#,
            "compact, not pretty-printed"
        );
        assert!(!json.is_decoded());

        let epoch = KvElement::from_raw(Bytes::from_static(b"1700000000"));
        assert_eq!(epoch.format(), DataFormat::Text);
        assert_eq!(epoch.text().as_ref(), "1700000000", "a number stays a number");
    }

    #[test]
    fn messagepack_and_gzip_decode_to_one_line() {
        let packed = rmp_serde::to_vec(&serde_json::json!({"name": "zedis", "n": 7})).expect("msgpack");
        let element = KvElement::from_raw(packed.clone());
        assert_eq!(element.format(), DataFormat::MessagePack);
        assert!(element.is_decoded());
        assert!(!element.text().contains('\n'), "{}", element.text());
        assert!(element.text().contains("\"name\": \"zedis\""), "{}", element.text());
        assert_eq!(element.raw().as_ref(), packed.as_slice(), "the bytes are untouched");

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"line one\nline two").expect("gzip");
        let gz = encoder.finish().expect("gzip");
        let element = KvElement::from_raw(gz);
        assert_eq!(element.format(), DataFormat::Gzip);
        assert_eq!(element.text().as_ref(), "line one line two");
    }

    #[test]
    fn binary_is_hex_and_edits_as_hex() {
        let element = KvElement::from_raw(Bytes::from_static(&[0x00, 0xff, 0x10]));
        assert_eq!(element.format(), DataFormat::Bytes);
        assert!(element.is_binary());
        assert_eq!(element.text().as_ref(), "00 ff 10");
        assert_eq!(element.edit_text().as_ref(), "00 ff 10");
        assert_eq!(element.bytes_from_edit("00, ff").expect("hex").as_ref(), &[0x00, 0xff]);
        assert!(element.bytes_from_edit("zz").is_err());

        let text = KvElement::from_text("abc");
        assert_eq!(text.bytes_from_edit("xyz").expect("text").as_ref(), b"xyz");
    }

    #[test]
    fn an_image_is_named_but_shown_as_hex() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[0u8; 16]);
        let element = KvElement::from_raw(png);
        assert_eq!(element.format(), DataFormat::Png);
        assert!(element.text().starts_with("89 50 4e 47"), "{}", element.text());
        assert!(!element.is_decoded());
    }
}
