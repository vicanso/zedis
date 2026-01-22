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

use dashmap::DashMap;
use std::hash::Hash;
use std::time::Instant;
use std::{
    fmt::Debug,
    sync::LazyLock,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

static APP_START: LazyLock<Instant> = LazyLock::new(Instant::now);

pub fn now_secs() -> u64 {
    Instant::now().duration_since(*APP_START).as_secs()
}

struct TtlCacheItem<V> {
    value: V,
    expired_at: AtomicU64,
}

pub struct TtlCache<K, V: Clone> {
    idle: Duration,
    cache: DashMap<K, TtlCacheItem<V>>,
}

impl<K: Eq + Hash + Debug, V: Clone> TtlCache<K, V> {
    pub fn new(idle: Duration) -> Self {
        Self {
            idle,
            cache: DashMap::with_capacity(10),
        }
    }
    pub fn get(&self, key: &K) -> Option<V> {
        let item = self.cache.get(key)?;
        let now = now_secs();
        if item.expired_at.load(Ordering::Relaxed) < now {
            return None;
        }
        item.expired_at.store(now + self.idle.as_secs(), Ordering::Relaxed);
        Some(item.value.clone())
    }
    pub fn insert(&self, key: K, value: V) {
        let expired_at = now_secs() + self.idle.as_secs();
        self.cache.insert(
            key,
            TtlCacheItem {
                value,
                expired_at: AtomicU64::new(expired_at),
            },
        );
    }
    pub fn remove(&self, key: &K) {
        self.cache.remove(key);
    }
    pub fn clear_expired(&self) -> (usize, usize) {
        let now = now_secs();
        let mut count = 0;
        self.cache.retain(|_, item| {
            let availabled = item.expired_at.load(Ordering::Relaxed) > now;
            if !availabled {
                count += 1;
            }
            availabled
        });
        (count, self.cache.len())
    }
}
