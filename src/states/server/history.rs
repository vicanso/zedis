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

//! In-memory write history for string-key values.
//!
//! Each time the user saves a value through the bytes editor we snapshot
//! the *previous* bytes here so the user can roll back a few versions
//! without leaving the app. The history is purely local — it is never
//! persisted to disk and is cleared on app exit, on key delete, and on
//! server switch. Behavior is roughly that of an undo stack with a
//! bounded ring buffer per key.

use bytes::Bytes;
use std::collections::VecDeque;

/// Maximum number of historical versions kept per key. Older entries get
/// evicted FIFO once this is exceeded.
pub const VALUE_HISTORY_CAPACITY: usize = 10;

/// A single past version of a string-key value.
#[derive(Debug, Clone)]
pub struct ValueHistoryEntry {
    /// The bytes that were overwritten by the SET this entry was captured for.
    pub bytes: Bytes,
    /// Unix timestamp (seconds) of when the overwrite happened.
    pub at: i64,
}

impl ValueHistoryEntry {
    pub fn size(&self) -> usize {
        self.bytes.len()
    }
}

/// Push a new entry onto the front of `buffer`, evicting the oldest from
/// the back if it would exceed `VALUE_HISTORY_CAPACITY`. Newest-first
/// ordering keeps the rendering code straightforward — index 0 is "most
/// recent".
///
/// Skips identical consecutive entries: if the same bytes are saved twice
/// in a row, the second push is a no-op. This avoids history bloat from
/// users hitting save without actually changing anything.
pub fn push_history(buffer: &mut VecDeque<ValueHistoryEntry>, entry: ValueHistoryEntry) {
    if let Some(front) = buffer.front()
        && front.bytes == entry.bytes
    {
        return;
    }
    if buffer.len() >= VALUE_HISTORY_CAPACITY {
        buffer.pop_back();
    }
    buffer.push_front(entry);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(s: &[u8], ts: i64) -> ValueHistoryEntry {
        ValueHistoryEntry {
            bytes: Bytes::copy_from_slice(s),
            at: ts,
        }
    }

    #[test]
    fn pushes_newest_first() {
        let mut buf = VecDeque::new();
        push_history(&mut buf, entry(b"v1", 1));
        push_history(&mut buf, entry(b"v2", 2));
        assert_eq!(buf.len(), 2);
        assert_eq!(buf[0].bytes.as_ref(), b"v2");
        assert_eq!(buf[1].bytes.as_ref(), b"v1");
    }

    #[test]
    fn evicts_oldest_at_capacity() {
        let mut buf = VecDeque::new();
        for i in 0..(VALUE_HISTORY_CAPACITY + 5) {
            // Vary the bytes to avoid the consecutive-dedup optimization.
            let s = format!("v{i}");
            push_history(&mut buf, entry(s.as_bytes(), i as i64));
        }
        assert_eq!(buf.len(), VALUE_HISTORY_CAPACITY);
        // Newest is index 0, oldest survivor should be entry index 5
        // (entries 0..=4 evicted).
        assert_eq!(
            buf.front().expect("buffer should have a front entry").at,
            (VALUE_HISTORY_CAPACITY + 4) as i64,
        );
        assert_eq!(buf.back().expect("buffer should have a back entry").at, 5);
    }

    #[test]
    fn dedups_consecutive_identical_writes() {
        let mut buf = VecDeque::new();
        push_history(&mut buf, entry(b"same", 1));
        push_history(&mut buf, entry(b"same", 2));
        push_history(&mut buf, entry(b"same", 3));
        assert_eq!(buf.len(), 1, "identical consecutive writes should collapse");
        // But a change in between still creates two entries.
        push_history(&mut buf, entry(b"diff", 4));
        push_history(&mut buf, entry(b"same", 5));
        assert_eq!(buf.len(), 3);
    }
}
