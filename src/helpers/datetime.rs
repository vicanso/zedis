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

use chrono::{DateTime, Local, NaiveDate, NaiveTime, TimeZone, Utc};
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
    match Utc.with_ymd_and_hms(2026, 3, 4, 15, 6, 7).single() {
        Some(sample) => sample.format(format.pattern).to_string(),
        None => format.pattern.to_string(),
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

fn render<Tz: TimeZone>(dt: &DateTime<Tz>, zone: TimeZonePref, pattern: &str) -> String {
    match zone {
        TimeZonePref::Local => dt.with_timezone(&Local).format(pattern).to_string(),
        TimeZonePref::Utc => dt.with_timezone(&Utc).format(pattern).to_string(),
    }
}

/// Full date + time in the configured zone and layout.
pub fn format_datetime<Tz: TimeZone>(dt: &DateTime<Tz>) -> String {
    let (zone, format) = current();
    render(dt, zone, format.pattern)
}

/// Full date + time in an explicit zone but the configured layout — the
/// timestamp preview shows the instant in both zones.
pub fn format_datetime_in<Tz: TimeZone>(dt: &DateTime<Tz>, zone: TimeZonePref) -> String {
    render(dt, zone, current().1.pattern)
}

/// The instant in the zone the user did *not* pick, labelled — the second
/// line of the timestamp preview.
pub fn format_datetime_other_zone<Tz: TimeZone>(dt: &DateTime<Tz>) -> (&'static str, String) {
    let other = configured_time_zone().other();
    (other.label(), format_datetime_in(dt, other))
}

/// Unix seconds → configured date + time; `None` when out of chrono's range.
pub fn format_unix_secs(ts: i64) -> Option<String> {
    DateTime::from_timestamp(ts, 0).map(|dt| format_datetime(&dt))
}

/// Unix milliseconds rendered with an explicit pattern in the configured
/// zone — chart axis ticks and other short forms that keep their own layout.
pub fn format_unix_millis_with(ms: i64, pattern: &str) -> Option<String> {
    DateTime::from_timestamp_millis(ms).map(|dt| render(&dt, configured_time_zone(), pattern))
}

/// Split a unix instant into the calendar date and wall-clock time the
/// configured zone shows it at — what a date picker and a time field have
/// to be filled with so they agree with every timestamp elsewhere.
pub fn unix_secs_to_parts(ts: i64) -> Option<(NaiveDate, NaiveTime)> {
    let dt = DateTime::from_timestamp(ts, 0)?;
    let naive = match current().0 {
        TimeZonePref::Local => dt.with_timezone(&Local).naive_local(),
        TimeZonePref::Utc => dt.naive_utc(),
    };
    Some((naive.date(), naive.time()))
}

/// A picked date + typed time back to a unix instant, read in the
/// configured zone — the inverse of [`unix_secs_to_parts`].
///
/// A local time that a DST jump makes ambiguous resolves to the earlier of
/// the two readings rather than failing; the alternative is refusing an
/// hour of the year.
pub fn unix_secs_from_parts(date: NaiveDate, time: NaiveTime) -> Option<i64> {
    let naive = date.and_time(time);
    match current().0 {
        TimeZonePref::Local => Local.from_local_datetime(&naive).earliest().map(|dt| dt.timestamp()),
        TimeZonePref::Utc => Some(naive.and_utc().timestamp()),
    }
}

/// A typed time of day: `HH:MM` or `HH:MM:SS`, 24-hour. Deliberately not
/// forgiving about 12-hour input — "7:30" with no meridiem would be a
/// twelve-hour guess on a field that sets when a key dies.
pub fn parse_clock_input(input: &str) -> Option<NaiveTime> {
    let input = input.trim();
    NaiveTime::parse_from_str(input, "%H:%M:%S")
        .or_else(|_| NaiveTime::parse_from_str(input, "%H:%M"))
        .ok()
}

/// Time of day (`HH:MM:SS`, optionally with milliseconds) in the
/// configured zone, for the live panels' clock columns.
pub fn format_clock<Tz: TimeZone>(dt: &DateTime<Tz>, millis: bool) -> String {
    let pattern = if millis { CLOCK_MILLIS_PATTERN } else { CLOCK_PATTERN };
    render(dt, configured_time_zone(), pattern)
}

/// The current time of day in the configured zone.
pub fn now_clock(millis: bool) -> String {
    format_clock(&Utc::now(), millis)
}

/// The current date + time in the configured zone and layout.
pub fn now_datetime() -> String {
    format_datetime(&Utc::now())
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
        let dt = DateTime::from_timestamp(ts, 250_000_000).expect("in range");
        assert_eq!(format_clock(&dt, true), "15:06:07.250");
        assert_eq!(format_clock(&dt, false), "15:06:07");
        let (label, other) = format_datetime_other_zone(&dt);
        assert_eq!(label, "Local");
        assert_eq!(other, format_datetime_in(&dt, TimeZonePref::Local));
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
                    Some(date.and_time(time).format("%Y-%m-%d %H:%M:%S").to_string().as_str()),
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
        let date = NaiveDate::from_ymd_opt(2026, 3, 4).expect("valid date");
        let time = NaiveTime::from_hms_opt(15, 6, 7).expect("valid time");
        set_datetime_prefs(TimeZonePref::Utc, DEFAULT_DATE_FORMAT);
        assert_eq!(unix_secs_from_parts(date, time), Some(1_772_636_767));
        assert_eq!(unix_secs_to_parts(1_772_636_767), Some((date, time)));
        // The same wall clock is a different instant in a zone that is not
        // UTC, which is the whole reason this goes through the preference.
        set_datetime_prefs(TimeZonePref::Local, DEFAULT_DATE_FORMAT);
        let local = unix_secs_from_parts(date, time).expect("resolvable");
        let offset = Local
            .from_local_datetime(&date.and_time(time))
            .earliest()
            .expect("resolvable")
            .offset()
            .local_minus_utc() as i64;
        assert_eq!(local, 1_772_636_767 - offset);
    }

    #[test]
    fn clock_input_takes_hh_mm_and_hh_mm_ss_only() {
        assert_eq!(parse_clock_input("14:30"), NaiveTime::from_hms_opt(14, 30, 0));
        assert_eq!(parse_clock_input("  14:30:05 "), NaiveTime::from_hms_opt(14, 30, 5));
        assert_eq!(parse_clock_input("00:00"), NaiveTime::from_hms_opt(0, 0, 0));
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
}
