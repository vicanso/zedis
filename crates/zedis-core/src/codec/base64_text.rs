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

//! Base64 text → bytes, standard or URL-safe alphabet, padded or not.
//!
//! The alphabet alone proves nothing — a hex digest, a session token or a
//! plain word can all be "valid Base64" and decode to noise — so this only
//! answers with the bytes, and the caller decides whether they mean
//! anything (readable text, JSON, a format the pipeline recognises). Short
//! inputs are refused outright: below [`MIN_LEN`] a coincidental match is
//! far likelier than an encoded value.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};

/// Shortest text considered (the encoding of 12 bytes).
pub const MIN_LEN: usize = 16;

/// The decoded bytes, if `text` is well-formed Base64 of either alphabet.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if text.len() < MIN_LEN {
        return None;
    }
    let body = text.trim_end_matches('=');
    let padding = text.len() - body.len();
    if padding > 2 || body.is_empty() {
        return None;
    }
    let standard = body
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/');
    let url_safe = body
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    // An alphanumeric-only string fits both alphabets and decodes the same
    // either way; `+` / `/` versus `-` / `_` decides the rest.
    if standard {
        decode_with(text, body, padding, &STANDARD, &STANDARD_NO_PAD)
    } else if url_safe {
        decode_with(text, body, padding, &URL_SAFE, &URL_SAFE_NO_PAD)
    } else {
        None
    }
}

fn decode_with(
    text: &str,
    body: &str,
    padding: usize,
    padded: &impl Engine,
    unpadded: &impl Engine,
) -> Option<Vec<u8>> {
    if padding > 0 {
        // Padding present: it has to be the canonical amount.
        text.len().is_multiple_of(4).then(|| padded.decode(text).ok()).flatten()
    } else if body.len().is_multiple_of(4) {
        padded.decode(body).ok()
    } else {
        unpadded.decode(body).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_standard_and_url_safe_with_or_without_padding() {
        assert_eq!(decode("aGVsbG8sIHdvcmxkISEh").as_deref(), Some(&b"hello, world!!!"[..]));
        assert_eq!(decode("aGVsbG8sIHdvcmxkIQ==").as_deref(), Some(&b"hello, world!"[..]));
        assert_eq!(decode("aGVsbG8sIHdvcmxkIQ").as_deref(), Some(&b"hello, world!"[..]));
        // URL-safe alphabet: `-` and `_` in place of `+` and `/`.
        let bytes = [0xfbu8, 0xff, 0xbf, 0xfe, 0xff, 0xef, 0xfb, 0xff, 0xbf, 0xfe, 0xff, 0xef];
        let text = URL_SAFE_NO_PAD.encode(bytes);
        assert!(text.contains('-') && text.contains('_'), "{text}");
        assert_eq!(decode(&text).as_deref(), Some(&bytes[..]));
        assert_eq!(decode(&text[..15]), None, "15 chars is below the floor");
    }

    #[test]
    fn refuses_what_is_not_base64() {
        assert_eq!(decode("hello world, not b64"), None, "space");
        assert_eq!(decode("short"), None);
        assert_eq!(decode("aGVsbG8sIHdvcmxkIQ==="), None, "three pads");
        assert_eq!(decode("aGVsbG8sIHdvcmxkIQ=x"), None, "pad inside");
    }
}
