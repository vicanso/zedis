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

//! A JSON Web Token, shown rather than verified: header and payload as
//! JSON, the signature as it came, and the `exp` / `iat` / `nbf` claims
//! spelled out as dates with their distance from now. No key is checked
//! — the viewer says what the token claims, not whether to trust it.

use super::describe_instant;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};
use serde_json::{Map, Value};

/// `{ "header", "payload", "signature", … }` for a compact JWS, else `None`.
pub fn decode(text: &str) -> Option<Value> {
    let text = text.trim();
    let mut parts = text.split('.');
    let (header, payload, signature) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || header.is_empty() || payload.is_empty() {
        return None;
    }
    let header = json_object(header)?;
    if !header.contains_key("alg") {
        return None;
    }
    let payload = json_object(payload)?;

    let mut out = Map::new();
    out.insert("header".into(), Value::Object(header));
    for (claim, label) in [("exp", "expires"), ("nbf", "not_before"), ("iat", "issued")] {
        if let Some(seconds) = payload.get(claim).and_then(Value::as_i64)
            && let Some(described) = describe_instant(seconds)
        {
            out.insert(label.into(), Value::String(described));
        }
    }
    out.insert("payload".into(), Value::Object(payload));
    out.insert(
        "signature".into(),
        Value::String(if signature.is_empty() {
            "(none)".to_string()
        } else {
            signature.to_string()
        }),
    );
    Some(Value::Object(out))
}

/// A base64url segment (padding tolerated, the standard alphabet too)
/// holding a JSON object.
fn json_object(segment: &str) -> Option<Map<String, Value>> {
    let segment = segment.trim_end_matches('=');
    let bytes = URL_SAFE_NO_PAD
        .decode(segment)
        .or_else(|_| STANDARD_NO_PAD.decode(segment))
        .ok()?;
    match serde_json::from_slice(&bytes).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A compact token built from its JSON, so the test reads as the claims
    /// rather than as Base64.
    fn token(header: &str, payload: &str, signature: &str) -> String {
        format!(
            "{}.{}.{signature}",
            URL_SAFE_NO_PAD.encode(header),
            URL_SAFE_NO_PAD.encode(payload)
        )
    }

    #[test]
    fn splits_header_payload_and_signature_and_dates_the_claims() {
        let token = token(
            r#"{"alg":"HS256","typ":"JWT"}"#,
            r#"{"sub":"42","name":"zedis","exp":4102444800}"#,
            "sig-goes-here",
        );
        let decoded = decode(&token).expect("jwt");
        assert_eq!(decoded["header"], json!({ "alg": "HS256", "typ": "JWT" }));
        assert_eq!(decoded["payload"]["name"], json!("zedis"));
        assert_eq!(decoded["signature"], json!("sig-goes-here"));
        // 4102444800 = 2100-01-01T00:00:00Z, still ahead of any clock this runs on.
        let expires = decoded["expires"].as_str().expect("expires");
        assert!(expires.starts_with("2100-01-01T00:00:00Z (in "), "{expires}");
    }

    #[test]
    fn an_unsigned_token_and_padded_segments_still_decode() {
        let unsigned = "eyJhbGciOiJub25lIn0=.eyJzdWIiOiIxIn0=.";
        let decoded = decode(unsigned).expect("jwt");
        assert_eq!(decoded["header"]["alg"], json!("none"));
        assert_eq!(decoded["signature"], json!("(none)"));
    }

    #[test]
    fn refuses_dotted_text_that_is_not_a_token() {
        assert_eq!(decode("a.b.c"), None);
        assert_eq!(decode("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0"), None, "two parts");
        // Header without `alg` is not a JOSE header.
        assert_eq!(decode("eyJ0eXAiOiJKV1QifQ.eyJzdWIiOiIxIn0.x"), None);
        assert_eq!(decode("1.2.3.4"), None);
    }
}
