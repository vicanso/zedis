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

pub fn validate_ttl(s: &str) -> bool {
    if s.is_empty() || s.parse::<usize>().is_ok() {
        return true;
    }
    humantime::parse_duration(s).is_ok()
}

pub fn validate_long_string(s: &str) -> bool {
    s.len() <= 4096
}

pub fn validate_common_string(s: &str) -> bool {
    s.len() <= 255
}

pub fn validate_host(s: &str) -> bool {
    s.len() <= 255 && s.is_ascii()
}
