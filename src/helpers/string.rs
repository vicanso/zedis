// Copyright 2025 Tree xie.
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

pub fn fast_contains_ignore_case(haystack: &str, needle_lower: &str) -> bool {
    // 1. 长度剪枝
    if needle_lower.len() > haystack.len() {
        return false;
    }

    if haystack.is_ascii() {
        let needle_bytes = needle_lower.as_bytes();
        return haystack
            .as_bytes()
            .windows(needle_bytes.len())
            .any(|window| window.eq_ignore_ascii_case(needle_bytes));
    }

    haystack.to_lowercase().contains(needle_lower)
}
