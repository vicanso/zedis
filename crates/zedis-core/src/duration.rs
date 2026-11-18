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

//! Human-readable duration parsing and formatting (`90s`, `1h30m`, `500ms`).

use std::error::Error;
use std::fmt::{self, Write as _};
use std::str::Chars;
use std::time::Duration;

const SECS_PER_MINUTE: u64 = 60;
const SECS_PER_HOUR: u64 = 3_600;
const SECS_PER_DAY: u64 = 86_400;
const SECS_PER_WEEK: u64 = 7 * SECS_PER_DAY;
const SECS_PER_MONTH: u64 = 2_630_016; // 30.44 days
const SECS_PER_YEAR: u64 = 31_557_600; // 365.25 days

/// Error returned when parsing a human-readable duration string fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DurationParseError {
    InvalidCharacter(usize),
    NumberExpected(usize),
    UnknownUnit {
        start: usize,
        end: usize,
        unit: String,
        value: u64,
    },
    NumberOverflow,
    Empty,
}

impl Error for DurationParseError {}

impl fmt::Display for DurationParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCharacter(offset) => write!(f, "invalid character at {offset}"),
            Self::NumberExpected(offset) => write!(f, "expected number at {offset}"),
            Self::UnknownUnit { unit, value, .. } if unit.is_empty() => {
                write!(f, "time unit needed, for example {value}sec or {value}ms")
            }
            Self::UnknownUnit { unit, .. } => write!(
                f,
                "unknown time unit {unit:?}, supported units: ns, us/µs, ms, sec, min, hours, days, weeks, months, years (and few variations)"
            ),
            Self::NumberOverflow => {
                write!(
                    f,
                    "number is too large or cannot be represented without a lack of precision (values below 1ns are not supported)"
                )
            }
            Self::Empty => write!(f, "value was empty"),
        }
    }
}

fn checked_mul(a: u64, b: u64) -> Result<u64, DurationParseError> {
    a.checked_mul(b).ok_or(DurationParseError::NumberOverflow)
}

fn checked_add(a: u64, b: u64) -> Result<u64, DurationParseError> {
    a.checked_add(b).ok_or(DurationParseError::NumberOverflow)
}

fn checked_exact_div(a: u64, b: u64) -> Result<u64, DurationParseError> {
    if a.is_multiple_of(b) {
        Ok(a / b)
    } else {
        Err(DurationParseError::NumberOverflow)
    }
}

#[derive(Debug, Clone, Copy)]
struct Fraction {
    numerator: u64,
    denominator: u64,
}

enum Unit {
    Nanosecond,
    Microsecond,
    Millisecond,
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Year,
}

impl Unit {
    fn from_name(s: &str) -> Option<Self> {
        match s {
            "nanos" | "nsec" | "ns" => Some(Self::Nanosecond),
            "usec" | "us" | "µs" => Some(Self::Microsecond),
            "millis" | "msec" | "ms" => Some(Self::Millisecond),
            "seconds" | "second" | "secs" | "sec" | "s" => Some(Self::Second),
            "minutes" | "minute" | "min" | "mins" | "m" => Some(Self::Minute),
            "hours" | "hour" | "hr" | "hrs" | "h" => Some(Self::Hour),
            "days" | "day" | "d" => Some(Self::Day),
            "weeks" | "week" | "wk" | "wks" | "w" => Some(Self::Week),
            "months" | "month" | "M" => Some(Self::Month),
            "years" | "year" | "yr" | "yrs" | "y" => Some(Self::Year),
            _ => None,
        }
    }
}

struct Parser<'a> {
    iter: Chars<'a>,
    src: &'a str,
}

impl Parser<'_> {
    fn parse(mut self) -> Result<Duration, DurationParseError> {
        let mut n = self.parse_first_char()?.ok_or(DurationParseError::Empty)?;
        let mut out = Duration::ZERO;
        'outer: loop {
            let mut frac = None;
            let mut off = self.off();
            while let Some(c) = self.iter.next() {
                match c {
                    '0'..='9' => {
                        n = n
                            .checked_mul(10)
                            .and_then(|x| x.checked_add(c as u64 - '0' as u64))
                            .ok_or(DurationParseError::NumberOverflow)?;
                    }
                    c if c.is_whitespace() => {}
                    'a'..='z' | 'A'..='Z' | 'µ' => {
                        break;
                    }
                    '.' => {
                        frac = Some(self.parse_fractional_part(&mut off)?);
                        break;
                    }
                    _ => {
                        return Err(DurationParseError::InvalidCharacter(off));
                    }
                }
                off = self.off();
            }
            let start = off;
            let mut off = self.off();
            while let Some(c) = self.iter.next() {
                match c {
                    '0'..='9' => {
                        self.parse_unit(n, frac, start, off, &mut out)?;
                        n = c as u64 - '0' as u64;
                        continue 'outer;
                    }
                    c if c.is_whitespace() => break,
                    'a'..='z' | 'A'..='Z' | 'µ' => {}
                    _ => {
                        return Err(DurationParseError::InvalidCharacter(off));
                    }
                }
                off = self.off();
            }

            self.parse_unit(n, frac, start, off, &mut out)?;
            n = match self.parse_first_char()? {
                Some(n) => n,
                None => return Ok(out),
            };
        }
    }

    fn parse_first_char(&mut self) -> Result<Option<u64>, DurationParseError> {
        let off = self.off();
        for c in self.iter.by_ref() {
            match c {
                '0'..='9' => return Ok(Some(c as u64 - '0' as u64)),
                c if c.is_whitespace() => continue,
                _ => return Err(DurationParseError::NumberExpected(off)),
            }
        }
        Ok(None)
    }

    fn parse_fractional_part(&mut self, off: &mut usize) -> Result<Fraction, DurationParseError> {
        let mut numerator = 0u64;
        let mut denominator = 1u64;
        let mut zeros = true;
        while let Some(c) = self.iter.next() {
            match c {
                '0' => {
                    denominator = checked_mul(denominator, 10)?;
                    if !zeros {
                        numerator = checked_mul(numerator, 10)?;
                    }
                }
                '1'..='9' => {
                    zeros = false;
                    denominator = checked_mul(denominator, 10)?;
                    numerator = checked_mul(numerator, 10).and_then(|x| checked_add(x, c as u64 - '0' as u64))?;
                }
                c if c.is_whitespace() => {}
                'a'..='z' | 'A'..='Z' | 'µ' => {
                    break;
                }
                _ => {
                    return Err(DurationParseError::InvalidCharacter(*off));
                }
            }
            *off = self.off();
        }
        if denominator == 1 {
            return Err(DurationParseError::InvalidCharacter(*off));
        }
        Ok(Fraction { numerator, denominator })
    }

    fn off(&self) -> usize {
        self.src.len() - self.iter.as_str().len()
    }

    fn parse_unit(
        &mut self,
        n: u64,
        frac: Option<Fraction>,
        start: usize,
        end: usize,
        out: &mut Duration,
    ) -> Result<(), DurationParseError> {
        let unit = Unit::from_name(&self.src[start..end]).ok_or_else(|| DurationParseError::UnknownUnit {
            start,
            end,
            unit: self.src[start..end].to_owned(),
            value: n,
        })?;

        let (sec, nsec) = match unit {
            Unit::Nanosecond => (0u64, n),
            Unit::Microsecond => (0u64, checked_mul(n, 1_000)?),
            Unit::Millisecond => (0u64, checked_mul(n, 1_000_000)?),
            Unit::Second => (n, 0),
            Unit::Minute => (checked_mul(n, SECS_PER_MINUTE)?, 0),
            Unit::Hour => (checked_mul(n, SECS_PER_HOUR)?, 0),
            Unit::Day => (checked_mul(n, SECS_PER_DAY)?, 0),
            Unit::Week => (checked_mul(n, SECS_PER_WEEK)?, 0),
            Unit::Month => (checked_mul(n, SECS_PER_MONTH)?, 0),
            Unit::Year => (checked_mul(n, SECS_PER_YEAR)?, 0),
        };
        add_current(sec, nsec, out)?;

        if let Some(Fraction {
            numerator: num,
            denominator: den,
        }) = frac
        {
            let (sec, nsec) = match unit {
                Unit::Nanosecond => return Err(DurationParseError::NumberOverflow),
                Unit::Microsecond => (0, checked_exact_div(checked_mul(num, 1_000)?, den)?),
                Unit::Millisecond => (0, checked_exact_div(checked_mul(num, 1_000_000)?, den)?),
                Unit::Second => (0, checked_exact_div(checked_mul(num, 1_000_000_000)?, den)?),
                Unit::Minute => (0, checked_exact_div(checked_mul(num, 60_000_000_000)?, den)?),
                Unit::Hour => (checked_exact_div(checked_mul(num, SECS_PER_HOUR)?, den)?, 0),
                Unit::Day => (checked_exact_div(checked_mul(num, SECS_PER_DAY)?, den)?, 0),
                Unit::Week => (checked_exact_div(checked_mul(num, SECS_PER_WEEK)?, den)?, 0),
                Unit::Month => (checked_exact_div(checked_mul(num, SECS_PER_MONTH)?, den)?, 0),
                Unit::Year => (checked_exact_div(checked_mul(num, SECS_PER_YEAR)?, den)?, 0),
            };
            add_current(sec, nsec, out)?;
        }

        Ok(())
    }
}

fn add_current(mut sec: u64, nsec: u64, out: &mut Duration) -> Result<(), DurationParseError> {
    let mut nsec = checked_add(u64::from(out.subsec_nanos()), nsec)?;
    if nsec >= 1_000_000_000 {
        sec = checked_add(sec, nsec / 1_000_000_000)?;
        nsec %= 1_000_000_000;
    }
    sec = checked_add(out.as_secs(), sec)?;
    *out = Duration::new(sec, nsec as u32);
    Ok(())
}

/// Parses a human-friendly duration string (`1h30m`, `500ms`, `20 min 17 nsec`).
pub fn parse_human_duration(s: &str) -> Result<Duration, DurationParseError> {
    if s == "0" {
        return Ok(Duration::ZERO);
    }
    Parser {
        iter: s.chars(),
        src: s,
    }
    .parse()
}

fn push_plural(out: &mut String, name: &str, value: u64) {
    if value == 0 {
        return;
    }
    if !out.is_empty() {
        out.push(' ');
    }
    let _ = write!(out, "{value}{name}");
    if value > 1 {
        out.push('s');
    }
}

fn push_unit(out: &mut String, name: &str, value: u32) {
    if value == 0 {
        return;
    }
    if !out.is_empty() {
        out.push(' ');
    }
    let _ = write!(out, "{value}{name}");
}

/// Formats a [`Duration`] as a human-friendly span (`1h 30m 5s`, `500ms`, `0s`).
pub fn format_human_duration(val: Duration) -> String {
    let secs = val.as_secs();
    let nanos = val.subsec_nanos();
    if secs == 0 && nanos == 0 {
        return "0s".to_string();
    }

    let years = secs / SECS_PER_YEAR;
    let ydays = secs % SECS_PER_YEAR;
    let months = ydays / SECS_PER_MONTH;
    let mdays = ydays % SECS_PER_MONTH;
    let days = mdays / SECS_PER_DAY;
    let day_secs = mdays % SECS_PER_DAY;
    let hours = (day_secs / SECS_PER_HOUR) as u32;
    let minutes = ((day_secs % SECS_PER_HOUR) / SECS_PER_MINUTE) as u32;
    let seconds = (day_secs % SECS_PER_MINUTE) as u32;

    let millis = nanos / 1_000_000;
    let micros = nanos / 1_000 % 1_000;
    let nanosec = nanos % 1_000;

    let mut out = String::new();
    push_plural(&mut out, "year", years);
    push_plural(&mut out, "month", months);
    push_plural(&mut out, "day", days);
    push_unit(&mut out, "h", hours);
    push_unit(&mut out, "m", minutes);
    push_unit(&mut out, "s", seconds);
    push_unit(&mut out, "ms", millis);
    push_unit(&mut out, "us", micros);
    push_unit(&mut out, "ns", nanosec);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_durations_and_rejects_invalid_inputs() {
        assert_eq!(parse_human_duration("0"), Ok(Duration::ZERO));
        assert_eq!(parse_human_duration("0s"), Ok(Duration::ZERO));
        assert_eq!(parse_human_duration(" 90s "), Ok(Duration::from_secs(90)));
        assert_eq!(parse_human_duration("1h30m"), Ok(Duration::from_secs(5_400)));
        assert_eq!(parse_human_duration("2h 15m"), Ok(Duration::from_secs(8_100)));
        assert_eq!(parse_human_duration("7d"), Ok(Duration::from_secs(604_800)));
        assert_eq!(parse_human_duration("1.5s"), Ok(Duration::new(1, 500_000_000)));
        assert_eq!(parse_human_duration("500ms"), Ok(Duration::from_millis(500)));
        assert_eq!(parse_human_duration("0.5s"), Ok(Duration::from_millis(500)));
        assert_eq!(parse_human_duration("20 min 17 nsec "), Ok(Duration::new(1_200, 17)));
        assert_eq!(parse_human_duration("4M"), Ok(Duration::from_secs(4 * SECS_PER_MONTH)));
        assert_eq!(parse_human_duration("1y"), Ok(Duration::from_secs(SECS_PER_YEAR)));

        assert_eq!(parse_human_duration(""), Err(DurationParseError::Empty));
        assert_eq!(parse_human_duration("soon"), Err(DurationParseError::NumberExpected(0)));
        assert_eq!(parse_human_duration("0.5ns"), Err(DurationParseError::NumberOverflow));
        assert_eq!(
            parse_human_duration("10000000000000y"),
            Err(DurationParseError::NumberOverflow)
        );
        assert!(matches!(
            parse_human_duration("10nights"),
            Err(DurationParseError::UnknownUnit { .. })
        ));
    }

    #[test]
    fn formats_and_round_trips_durations() {
        assert_eq!(format_human_duration(Duration::ZERO), "0s");
        assert_eq!(format_human_duration(Duration::from_secs(5_405)), "1h 30m 5s");
        assert_eq!(format_human_duration(Duration::from_millis(1_500)), "1s 500ms");
        assert_eq!(format_human_duration(Duration::from_micros(123)), "123us");
        assert_eq!(
            format_human_duration(Duration::from_secs(3 * SECS_PER_DAY + 4 * SECS_PER_HOUR)),
            "3days 4h"
        );
        for secs in [0u64, 1, 59, 60, 3_600, 86_400, 604_800, 142 * 86_400 + 6_456] {
            let d = Duration::from_secs(secs);
            let formatted = format_human_duration(d);
            assert_eq!(parse_human_duration(&formatted), Ok(d), "{formatted}");
        }
    }
}
