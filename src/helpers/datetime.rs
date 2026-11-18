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

//! Process-wide date / time rendering preference.
//!
//! Every user-facing timestamp (slow log, persistence, monitor clock,
//! timestamp preview, metrics axis, trash) goes through here, so the
//! Settings "Time zone" / "Date format" choice applies everywhere at once.
//! The preference is mirrored into a process-wide slot (like the HTTP
//! proxy) because several call sites format on background threads or in
//! pure helpers with no `App` in reach. File-name stamps and diagnostics
//! deliberately stay on their own fixed formats.

use gpui_kit::component::calendar::{Date as PickerDate, Matcher};
use jiff::{
    Timestamp,
    civil::{Date, Time},
    tz::{Disambiguation, TimeZone},
};
use std::sync::RwLock;

/// Which zone timestamps are rendered in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TimeZonePref {
    #[default]
    Local,
    Utc,
}

impl TimeZonePref {
    pub const ALL: [TimeZonePref; 2] = [TimeZonePref::Local, TimeZonePref::Utc];

    pub fn from_name(name: &str) -> Self {
        match name {
            "utc" => TimeZonePref::Utc,
            _ => TimeZonePref::Local,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            TimeZonePref::Local => "local",
            TimeZonePref::Utc => "utc",
        }
    }

    /// Short label for the timestamp preview ("Local: …" / "UTC: …").
    pub fn label(self) -> &'static str {
        match self {
            TimeZonePref::Local => "Local",
            TimeZonePref::Utc => "UTC",
        }
    }

    fn other(self) -> Self {
        match self {
            TimeZonePref::Local => TimeZonePref::Utc,
            TimeZonePref::Utc => TimeZonePref::Local,
        }
    }
}

/// One selectable date + time layout. `id` is what `zedis.toml` stores.
#[derive(Debug, PartialEq, Eq)]
pub struct DateFormat {
    pub id: &'static str,
    /// strftime pattern for a full date + time.
    pub pattern: &'static str,
}

/// The selectable layouts, in the order the Settings select lists them.
/// The first entry is the default and matches what Zedis always rendered.
pub const DATE_FORMATS: &[DateFormat] = &[
    DateFormat {
        id: "iso",
        pattern: "%Y-%m-%d %H:%M:%S",
    },
    DateFormat {
        id: "rfc3339",
        pattern: "%Y-%m-%dT%H:%M:%S%:z",
    },
    DateFormat {
        id: "dmy",
        pattern: "%d/%m/%Y %H:%M:%S",
    },
    DateFormat {
        id: "dmy_dot",
        pattern: "%d.%m.%Y %H:%M:%S",
    },
    DateFormat {
        id: "mdy",
        pattern: "%m/%d/%Y %I:%M:%S %p",
    },
];

pub const DEFAULT_DATE_FORMAT: &str = "iso";

/// The clock pattern used by the live panels (monitor, keyspace events,
/// INFO "taken at"), with and without milliseconds. Independent of the
/// date layout: a clock column has no date to lay out.
const CLOCK_PATTERN: &str = "%H:%M:%S";
const CLOCK_MILLIS_PATTERN: &str = "%H:%M:%S%.3f";

/// Resolve a stored id, falling back to the default for an unknown one
/// (an older / hand-edited config).
pub fn date_format_by_id(id: &str) -> &'static DateFormat {
    DATE_FORMATS
        .iter()
        .find(|format| format.id == id)
        .or_else(|| DATE_FORMATS.iter().find(|format| format.id == DEFAULT_DATE_FORMAT))
        .unwrap_or(&DATE_FORMATS[0])
}

/// A fixed instant rendered in the layout, for the Settings select labels:
/// the sample explains the format better than any name would.
pub fn date_format_sample(format: &DateFormat) -> String {
    // 2026-03-04 15:06:07 UTC — every field is distinct, so the day / month
    // order and the 12-hour marker are visible at a glance.
    match jiff::civil::date(2026, 3, 4)
        .at(15, 6, 7, 0)
        .to_zoned_datetime(TimeZone::UTC)
    {
        Ok(sample) => sample.strftime(format.pattern).to_string(),
        Err(_) => format.pattern.to_string(),
    }
}

struct DateTimePrefs {
    zone: TimeZonePref,
    format: &'static DateFormat,
}

static PREFS: RwLock<DateTimePrefs> = RwLock::new(DateTimePrefs {
    zone: TimeZonePref::Local,
    format: &DATE_FORMATS[0],
});

/// Mirror the persisted preference into the process-wide slot. Called at
/// startup and whenever the Settings page changes either value.
pub fn set_datetime_prefs(zone: TimeZonePref, format_id: &str) {
    let format = date_format_by_id(format_id);
    let mut prefs = PREFS.write().unwrap_or_else(|e| e.into_inner());
    prefs.zone = zone;
    prefs.format = format;
}

fn current() -> (TimeZonePref, &'static DateFormat) {
    let prefs = PREFS.read().unwrap_or_else(|e| e.into_inner());
    (prefs.zone, prefs.format)
}

/// The configured zone (for callers that label their output with it).
pub fn configured_time_zone() -> TimeZonePref {
    current().0
}

fn tz_for(zone: TimeZonePref) -> TimeZone {
    match zone {
        TimeZonePref::Local => TimeZone::system(),
        TimeZonePref::Utc => TimeZone::UTC,
    }
}

fn render(ts: Timestamp, zone: TimeZonePref, pattern: &str) -> String {
    ts.to_zoned_datetime(tz_for(zone)).strftime(pattern).to_string()
}

/// Full date + time in the configured zone and layout.
pub fn format_datetime(ts: Timestamp) -> String {
    let (zone, format) = current();
    render(ts, zone, format.pattern)
}

/// Full date + time in an explicit zone but the configured layout — the
/// timestamp preview shows the instant in both zones.
pub fn format_datetime_in(ts: Timestamp, zone: TimeZonePref) -> String {
    render(ts, zone, current().1.pattern)
}

/// The instant in the zone the user did *not* pick, labelled — the second
/// line of the timestamp preview.
pub fn format_datetime_other_zone(ts: Timestamp) -> (&'static str, String) {
    let other = configured_time_zone().other();
    (other.label(), format_datetime_in(ts, other))
}

/// Unix seconds → configured date + time; `None` when out of `jiff`'s range.
pub fn format_unix_secs(ts: i64) -> Option<String> {
    Timestamp::from_second(ts).ok().map(format_datetime)
}

/// Unix milliseconds rendered with an explicit pattern in the configured
/// zone — chart axis ticks and other short forms that keep their own layout.
pub fn format_unix_millis_with(ms: i64, pattern: &str) -> Option<String> {
    Timestamp::from_millisecond(ms)
        .ok()
        .map(|ts| render(ts, configured_time_zone(), pattern))
}

/// Split a unix instant into the calendar date and wall-clock time the
/// configured zone shows it at — what a date picker and a time field have
/// to be filled with so they agree with every timestamp elsewhere.
pub fn unix_secs_to_parts(ts: i64) -> Option<(Date, Time)> {
    let ts = Timestamp::from_second(ts).ok()?;
    let dt = ts.to_zoned_datetime(tz_for(current().0)).datetime();
    Some((dt.date(), dt.time()))
}

/// A picked date + typed time back to a unix instant, read in the
/// configured zone — the inverse of [`unix_secs_to_parts`].
///
/// A local time that a DST jump makes ambiguous resolves to the earlier of
/// the two readings rather than failing; the alternative is refusing an
/// hour of the year.
pub fn unix_secs_from_parts(date: Date, time: Time) -> Option<i64> {
    let dt = jiff::civil::DateTime::from_parts(date, time);
    tz_for(current().0)
        .to_ambiguous_zoned_datetime(dt)
        .disambiguate(Disambiguation::Earlier)
        .ok()
        .map(|z| z.timestamp().as_second())
}

/// Convert a [`Date`] to `gpui_kit`'s `DatePicker` date value.
pub fn to_picker_date(date: Date) -> Option<PickerDate> {
    date.to_string().parse().ok().map(|d| PickerDate::Single(Some(d)))
}

/// Convert `gpui_kit`'s `DatePicker` date value back to [`Date`].
pub fn from_picker_date(date: PickerDate) -> Option<Date> {
    date.format("%Y-%m-%d")?.parse().ok()
}

/// A `DatePicker` matcher that disables every day strictly before `before`.
pub fn picker_before_matcher(before: Date) -> Matcher {
    Matcher::interval(before.to_string().parse().ok(), None)
}

/// A typed time of day: `HH:MM` or `HH:MM:SS`, 24-hour. Deliberately not
/// forgiving about 12-hour input — "7:30" with no meridiem would be a
/// twelve-hour guess on a field that sets when a key dies.
pub fn parse_clock_input(input: &str) -> Option<Time> {
    let input = input.trim();
    Time::strptime("%H:%M:%S", input)
        .or_else(|_| Time::strptime("%H:%M", input))
        .ok()
}

/// Time of day (`HH:MM:SS`, optionally with milliseconds) in the
/// configured zone, for the live panels' clock columns.
pub fn format_clock(ts: Timestamp, millis: bool) -> String {
    let pattern = if millis { CLOCK_MILLIS_PATTERN } else { CLOCK_PATTERN };
    render(ts, configured_time_zone(), pattern)
}

/// The current time of day in the configured zone.
pub fn now_clock(millis: bool) -> String {
    format_clock(Timestamp::now(), millis)
}

/// The current date + time in the configured zone and layout.
pub fn now_datetime() -> String {
    format_datetime(Timestamp::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    /// The prefs slot is process-wide, so the tests that set it serialise.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn lock() -> MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn utc_zone_and_layout_apply_to_every_helper() {
        let _guard = lock();
        set_datetime_prefs(TimeZonePref::Utc, "rfc3339");
        // 2026-03-04T15:06:07Z
        let ts = 1_772_636_767;
        assert_eq!(format_unix_secs(ts).as_deref(), Some("2026-03-04T15:06:07+00:00"));
        assert_eq!(
            format_unix_millis_with(ts * 1000, "%m-%d %H:%M").as_deref(),
            Some("03-04 15:06")
        );
        let dt = Timestamp::new(ts, 250_000_000).expect("in range");
        assert_eq!(format_clock(dt, true), "15:06:07.250");
        assert_eq!(format_clock(dt, false), "15:06:07");
        let (label, other) = format_datetime_other_zone(dt);
        assert_eq!(label, "Local");
        assert_eq!(other, format_datetime_in(dt, TimeZonePref::Local));
        set_datetime_prefs(TimeZonePref::Local, DEFAULT_DATE_FORMAT);
    }

    #[test]
    fn unknown_format_id_falls_back_to_the_default_layout() {
        let _guard = lock();
        set_datetime_prefs(TimeZonePref::Utc, "no-such-layout");
        assert_eq!(format_unix_secs(0).as_deref(), Some("1970-01-01 00:00:00"));
        set_datetime_prefs(TimeZonePref::Local, DEFAULT_DATE_FORMAT);
    }

    #[test]
    fn every_layout_round_trips_through_the_date_and_time_parts() {
        let _guard = lock();
        // 2026-03-04T15:06:07Z — a whole second, so no precision is lost.
        let ts = 1_772_636_767;
        for zone in TimeZonePref::ALL {
            for format in DATE_FORMATS {
                set_datetime_prefs(zone, format.id);
                let (date, time) = unix_secs_to_parts(ts).expect("in range");
                assert_eq!(
                    unix_secs_from_parts(date, time),
                    Some(ts),
                    "{} / {} did not round-trip ({date} {time})",
                    zone.name(),
                    format.id
                );
                // The parts must also agree with what the rest of the app
                // renders for the same instant, or the picker would show a
                // different clock from every other timestamp on screen.
                assert_eq!(
                    format_unix_millis_with(ts * 1000, "%Y-%m-%d %H:%M:%S").as_deref(),
                    Some(
                        jiff::civil::DateTime::from_parts(date, time)
                            .strftime("%Y-%m-%d %H:%M:%S")
                            .to_string()
                            .as_str()
                    ),
                    "{} / {} clock mismatch",
                    zone.name(),
                    format.id
                );
            }
        }
        set_datetime_prefs(TimeZonePref::Local, DEFAULT_DATE_FORMAT);
    }

    #[test]
    fn parts_are_read_in_the_configured_zone() {
        let _guard = lock();
        let date = jiff::civil::date(2026, 3, 4);
        let time = jiff::civil::time(15, 6, 7, 0);
        set_datetime_prefs(TimeZonePref::Utc, DEFAULT_DATE_FORMAT);
        assert_eq!(unix_secs_from_parts(date, time), Some(1_772_636_767));
        assert_eq!(unix_secs_to_parts(1_772_636_767), Some((date, time)));
        // The same wall clock is a different instant in a zone that is not
        // UTC, which is the whole reason this goes through the preference.
        set_datetime_prefs(TimeZonePref::Local, DEFAULT_DATE_FORMAT);
        let local = unix_secs_from_parts(date, time).expect("resolvable");
        let offset = TimeZone::system()
            .to_ambiguous_zoned_datetime(jiff::civil::DateTime::from_parts(date, time))
            .disambiguate(Disambiguation::Earlier)
            .expect("resolvable")
            .offset()
            .seconds() as i64;
        assert_eq!(local, 1_772_636_767 - offset);
    }

    #[test]
    fn clock_input_takes_hh_mm_and_hh_mm_ss_only() {
        assert_eq!(parse_clock_input("14:30"), Some(jiff::civil::time(14, 30, 0, 0)));
        assert_eq!(parse_clock_input("  14:30:05 "), Some(jiff::civil::time(14, 30, 5, 0)));
        assert_eq!(parse_clock_input("00:00"), Some(jiff::civil::time(0, 0, 0, 0)));
        // No meridiem guessing, no bare hours, no nonsense.
        assert_eq!(parse_clock_input("2:30 PM"), None);
        assert_eq!(parse_clock_input("14"), None);
        assert_eq!(parse_clock_input("25:00"), None);
        assert_eq!(parse_clock_input(""), None);
    }

    #[test]
    fn samples_show_the_field_order() {
        assert_eq!(date_format_sample(date_format_by_id("iso")), "2026-03-04 15:06:07");
        assert_eq!(date_format_sample(date_format_by_id("dmy")), "04/03/2026 15:06:07");
        assert_eq!(date_format_sample(date_format_by_id("mdy")), "03/04/2026 03:06:07 PM");
        assert_eq!(TimeZonePref::from_name("utc"), TimeZonePref::Utc);
        assert_eq!(TimeZonePref::from_name("anything"), TimeZonePref::Local);
    }

    #[test]
    fn picker_dates_round_trip_and_disable_earlier_days() {
        let day = jiff::civil::date(2026, 3, 4);
        let picker = to_picker_date(day).expect("valid picker date");
        assert_eq!(from_picker_date(picker), Some(day));

        let matcher = picker_before_matcher(day);
        let yesterday = to_picker_date(jiff::civil::date(2026, 3, 3)).expect("valid yesterday");
        let tomorrow = to_picker_date(jiff::civil::date(2026, 3, 5)).expect("valid tomorrow");
        assert!(matcher.is_match(&yesterday));
        assert!(!matcher.is_match(&picker));
        assert!(!matcher.is_match(&tomorrow));
    }
}
