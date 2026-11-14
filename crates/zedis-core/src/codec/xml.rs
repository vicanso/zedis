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

//! XML detection and pretty-printing.
//!
//! Only accepts complete, well-formed XML documents/fragments with at least
//! one balanced element tag so plain text containing `<` or `>` is not
//! misclassified.

use quick_xml::events::Event;
use quick_xml::{Reader, Writer};
use std::io::Cursor;

/// Validate and pretty-print an XML string with 2-space indentation.
/// Returns `None` if `text` is not well-formed XML with at least one element.
pub fn decode(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if !trimmed.starts_with('<') || !trimmed.ends_with('>') {
        return None;
    }

    let mut reader = Reader::from_str(trimmed);
    reader.config_mut().trim_text(true);
    let mut writer = Writer::new_with_indent(Cursor::new(Vec::new()), b' ', 2);
    let mut depth: usize = 0;
    let mut saw_element = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                saw_element = true;
                depth += 1;
                writer.write_event(Event::Start(e)).ok()?;
            }
            Ok(Event::End(e)) => {
                depth = depth.checked_sub(1)?;
                writer.write_event(Event::End(e)).ok()?;
            }
            Ok(Event::Empty(e)) => {
                saw_element = true;
                writer.write_event(Event::Empty(e)).ok()?;
            }
            Ok(Event::Eof) => break,
            Ok(event) => {
                writer.write_event(event).ok()?;
            }
            Err(_) => return None,
        }
    }

    if !saw_element || depth != 0 {
        return None;
    }

    String::from_utf8(writer.into_inner().into_inner()).ok()
}

#[cfg(test)]
mod tests {
    use super::decode;

    #[test]
    fn pretty_prints_minified_xml() {
        let input =
            r#"<?xml version="1.0" encoding="UTF-8"?><root id="1"><user><name>Alice</name><active/></user></root>"#;
        let out = decode(input).expect("valid xml");
        assert!(out.contains("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(out.contains("<root id=\"1\">\n  <user>\n    <name>Alice</name>\n    <active/>\n  </user>\n</root>"));
    }

    #[test]
    fn rejects_unclosed_or_non_xml_text() {
        assert!(decode("hello world").is_none());
        assert!(decode("<root><child></root>").is_none());
        assert!(decode("<root>unclosed").is_none());
        assert!(decode("<!-- just a comment -->").is_none());
    }
}
