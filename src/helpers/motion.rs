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

//! Centralized motion duration tokens and reduced-motion helpers.
//!
//! Any custom `with_animation` transition must take its duration from here
//! and honor [`should_animate`] / [`motion_duration`].

use gpui::App;
use std::time::Duration;

/// Instant micro-feedback: hover washes and quick color transitions.
pub const MOTION_INSTANT: Duration = Duration::from_millis(120);
/// Fast tactile response: button presses and control state changes.
pub const MOTION_FAST: Duration = Duration::from_millis(180);
/// Base transition: moving selection indicators, glyph swaps, surface exit.
pub const MOTION_BASE: Duration = Duration::from_millis(240);
/// Slow reveal: menus, dialogs, and container reveals.
pub const MOTION_SLOW: Duration = Duration::from_millis(360);

/// Whether motion should play for the current application preferences.
#[inline]
pub fn should_animate(cx: &App) -> bool {
    !cx.reduce_motion()
}

/// Resolve a motion token duration, collapsing to zero when reduced motion is enabled.
#[inline]
pub fn motion_duration(cx: &App, token: Duration) -> Duration {
    if should_animate(cx) { token } else { Duration::ZERO }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn motion_tokens_honor_reduce_motion(cx: &mut TestAppContext) {
        assert!(MOTION_INSTANT < MOTION_FAST);
        assert!(MOTION_FAST < MOTION_BASE);
        assert!(MOTION_BASE < MOTION_SLOW);

        cx.update(|cx| {
            cx.set_reduce_motion(false);
            assert!(should_animate(cx));
            assert_eq!(motion_duration(cx, MOTION_FAST), MOTION_FAST);

            cx.set_reduce_motion(true);
            assert!(!should_animate(cx));
            assert_eq!(motion_duration(cx, MOTION_FAST), Duration::ZERO);
        });
    }
}
