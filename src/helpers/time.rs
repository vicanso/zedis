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

use crate::error::Error;
use chrono::Local;
use std::time::Duration;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Helper function to get current Unix timestamp in seconds.
pub fn unix_ts() -> i64 {
    Local::now().timestamp()
}

/// Parse a duration string into a Duration.
pub fn parse_duration(s: &str) -> Result<Duration> {
    if let Ok(secs) = s.parse::<u64>() {
        return Ok(Duration::from_secs(secs));
    }
    humantime::parse_duration(s).map_err(|e| Error::Invalid { message: e.to_string() })
}
