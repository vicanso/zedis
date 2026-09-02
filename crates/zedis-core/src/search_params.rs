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

//! `FT.SEARCH … PARAMS` values: the `$name` placeholders a query references
//! and how a typed value becomes the bytes that go on the wire.
//!
//! A KNN / VECTOR_RANGE blob is the raw little-endian array RediSearch
//! reads as memory, so it has to match the field's `TYPE` exactly — the
//! encoding is chosen per parameter instead of guessed from the text.

/// How a parameter's text is encoded before it is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParamKind {
    /// Sent verbatim — numbers, tag values, terms.
    #[default]
    Text,
    /// Little-endian `f32` vector (`VECTOR … TYPE FLOAT32`).
    Float32,
    /// Little-endian `f64` vector (`TYPE FLOAT64`).
    Float64,
    /// IEEE 754 half precision (`TYPE FLOAT16`).
    Float16,
    /// bfloat16 — the top half of an `f32` (`TYPE BFLOAT16`).
    BFloat16,
}

impl ParamKind {
    pub const ALL: [ParamKind; 5] = [
        ParamKind::Text,
        ParamKind::Float32,
        ParamKind::Float64,
        ParamKind::Float16,
        ParamKind::BFloat16,
    ];

    /// The label on the kind toggle — the schema's own `TYPE` word for the
    /// vector kinds, so it can be matched against `FT.INFO` by eye.
    pub fn label(self) -> &'static str {
        match self {
            ParamKind::Text => "TEXT",
            ParamKind::Float32 => "FLOAT32",
            ParamKind::Float64 => "FLOAT64",
            ParamKind::Float16 => "FLOAT16",
            ParamKind::BFloat16 => "BFLOAT16",
        }
    }

    pub fn is_vector(self) -> bool {
        !matches!(self, ParamKind::Text)
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Every `$name` placeholder in `query`, unique, in first-seen order. A
/// `\$` is an escaped dollar and is skipped.
pub fn param_names(query: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let chars: Vec<char> = query.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' && (i == 0 || chars[i - 1] != '\\') && i + 1 < chars.len() && is_ident_start(chars[i + 1]) {
            let start = i + 1;
            let mut end = start;
            while end < chars.len() && is_ident(chars[end]) {
                end += 1;
            }
            let name: String = chars[start..end].iter().collect();
            if !names.contains(&name) {
                names.push(name);
            }
            i = end;
        } else {
            i += 1;
        }
    }
    names
}

/// Whether `$name` sits in a vector slot — `KNN k @field $name` or
/// `VECTOR_RANGE radius $name` — which is what decides the default kind.
pub fn is_vector_param(query: &str, name: &str) -> bool {
    let tokens: Vec<&str> = query
        .split(|c: char| c.is_whitespace() || matches!(c, '[' | ']' | '(' | ')' | '{' | '}' | '=' | '>'))
        .filter(|t| !t.is_empty())
        .collect();
    let wanted = format!("${name}");
    let is_count = |t: &str| t.starts_with('$') || t.parse::<f64>().is_ok();
    tokens.iter().enumerate().any(|(i, t)| {
        if *t != wanted {
            return false;
        }
        let knn = i >= 3
            && tokens[i - 1].starts_with('@')
            && is_count(tokens[i - 2])
            && tokens[i - 3].eq_ignore_ascii_case("KNN");
        let range = i >= 2 && is_count(tokens[i - 1]) && tokens[i - 2].eq_ignore_ascii_case("VECTOR_RANGE");
        knn || range
    })
}

/// Parse `0.1, -2 3e-1` / `[0.1, 0.2]` into floats. Commas, whitespace and
/// enclosing brackets all separate; anything else is an error naming the
/// offending item.
pub fn parse_vector(text: &str) -> Result<Vec<f64>, String> {
    let mut values = Vec::new();
    for (index, token) in text
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | '[' | ']' | '(' | ')'))
        .filter(|t| !t.is_empty())
        .enumerate()
    {
        let value: f64 = token
            .parse()
            .map_err(|_| format!("not a number: '{token}' (item {})", index + 1))?;
        values.push(value);
    }
    if values.is_empty() {
        return Err("empty vector".to_string());
    }
    Ok(values)
}

/// `f32` → IEEE 754 half bits, round-to-nearest-even; overflow saturates
/// to ±inf, NaN stays NaN.
fn f32_to_f16_bits(value: f32) -> u16 {
    let x = value.to_bits();
    let sign = ((x >> 16) & 0x8000) as u16;
    let exp = ((x >> 23) & 0xff) as i32;
    let mant = x & 0x7f_ffff;
    if exp == 0xff {
        let nan = if mant != 0 { 0x200 } else { 0 };
        return sign | 0x7c00 | nan;
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = mant | 0x80_0000;
        let shift = (14 - e) as u32;
        let mut half = (m >> shift) as u16;
        let rem = m & ((1u32 << shift) - 1);
        let halfway = 1u32 << (shift - 1);
        if rem > halfway || (rem == halfway && half & 1 == 1) {
            half += 1;
        }
        return sign | half;
    }
    let mut half = ((e as u16) << 10) | (mant >> 13) as u16;
    let rem = mant & 0x1fff;
    if rem > 0x1000 || (rem == 0x1000 && half & 1 == 1) {
        // A carry out of the mantissa bumps the exponent — correct, and
        // 0x7bff + 1 lands exactly on +inf.
        half = half.wrapping_add(1);
    }
    sign | half
}

/// `f32` → bfloat16 bits, round-to-nearest-even; NaN keeps a payload bit.
fn f32_to_bf16_bits(value: f32) -> u16 {
    let x = value.to_bits();
    if value.is_nan() {
        return ((x >> 16) | 0x40) as u16;
    }
    let lsb = (x >> 16) & 1;
    ((x.wrapping_add(0x7fff + lsb)) >> 16) as u16
}

/// Encode a parameter's text for the wire: verbatim bytes for `Text`, a
/// little-endian array for the vector kinds.
pub fn encode_param(kind: ParamKind, text: &str) -> Result<Vec<u8>, String> {
    if kind == ParamKind::Text {
        return Ok(text.as_bytes().to_vec());
    }
    let values = parse_vector(text)?;
    let mut out = Vec::with_capacity(values.len() * 8);
    for v in values {
        match kind {
            ParamKind::Text => unreachable!("handled above"),
            ParamKind::Float32 => out.extend_from_slice(&(v as f32).to_le_bytes()),
            ParamKind::Float64 => out.extend_from_slice(&v.to_le_bytes()),
            ParamKind::Float16 => out.extend_from_slice(&f32_to_f16_bits(v as f32).to_le_bytes()),
            ParamKind::BFloat16 => out.extend_from_slice(&f32_to_bf16_bits(v as f32).to_le_bytes()),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_placeholders_once_in_order() {
        assert_eq!(
            param_names("@t:$term @n:[$lo $hi] =>[KNN 3 @v $BLOB] $term"),
            ["term", "lo", "hi", "BLOB"]
        );
        assert!(param_names("plain query with 5$ and \\$escaped").is_empty());
        assert!(param_names("").is_empty());
    }

    #[test]
    fn vector_slots_are_recognised() {
        assert!(is_vector_param("*=>[KNN 10 @v $BLOB]", "BLOB"));
        assert!(is_vector_param("(@t:x)=>[KNN $K @v $q EF_RUNTIME 100]", "q"));
        assert!(is_vector_param("@v:[VECTOR_RANGE 0.5 $vec]", "vec"));
        assert!(!is_vector_param("*=>[KNN $K @v $q]", "K"));
        assert!(!is_vector_param("@t:$term", "term"));
    }

    #[test]
    fn parses_loose_vector_text() {
        assert_eq!(parse_vector("[0.5, -1, 2e1]").expect("ok"), [0.5, -1.0, 20.0]);
        assert_eq!(parse_vector(" 1 2\t3 ").expect("ok"), [1.0, 2.0, 3.0]);
        assert_eq!(parse_vector("").expect_err("empty"), "empty vector");
        assert_eq!(
            parse_vector("1, two, 3").expect_err("bad item"),
            "not a number: 'two' (item 2)"
        );
    }

    #[test]
    fn encodes_little_endian_per_kind() {
        assert_eq!(encode_param(ParamKind::Text, " a b ").expect("text"), b" a b ");
        assert_eq!(
            encode_param(ParamKind::Float32, "1, 0").expect("f32"),
            [0x00, 0x00, 0x80, 0x3f, 0x00, 0x00, 0x00, 0x00]
        );
        assert_eq!(
            encode_param(ParamKind::Float64, "1").expect("f64"),
            [0, 0, 0, 0, 0, 0, 0xf0, 0x3f]
        );
        assert_eq!(
            encode_param(ParamKind::Float16, "1 -2 0.5").expect("f16"),
            [0x00, 0x3c, 0x00, 0xc0, 0x00, 0x38]
        );
        assert_eq!(
            encode_param(ParamKind::BFloat16, "1 -2").expect("bf16"),
            [0x80, 0x3f, 0x00, 0xc0]
        );
        assert!(encode_param(ParamKind::Float32, "x").is_err());
    }

    #[test]
    fn half_precision_edges() {
        assert_eq!(f32_to_f16_bits(65504.0), 0x7bff, "largest finite half");
        assert_eq!(f32_to_f16_bits(65520.0), 0x7c00, "ties-to-even rounds up to inf");
        assert_eq!(f32_to_f16_bits(f32::INFINITY), 0x7c00);
        assert_eq!(f32_to_f16_bits(-0.0), 0x8000);
        assert_eq!(f32_to_f16_bits(5.960_464_5e-8), 0x0001, "smallest subnormal");
        assert_eq!(f32_to_f16_bits(2.980_232_2e-8), 0x0000, "half of it ties to even zero");
        assert_eq!(f32_to_f16_bits(1e-10), 0x0000, "underflow");
        assert_ne!(f32_to_f16_bits(f32::NAN) & 0x03ff, 0, "NaN keeps a payload");
        assert_eq!(f32_to_bf16_bits(1.0), 0x3f80);
        assert_eq!(f32_to_bf16_bits(1.004), 0x3f81, "above halfway rounds up");
        // 1 + 2⁻⁸ = 0x3f80_8000: the dropped half is exactly 0x8000.
        assert_eq!(
            f32_to_bf16_bits(f32::from_bits(0x3f80_8000)),
            0x3f80,
            "exact halfway ties to even"
        );
    }
}
