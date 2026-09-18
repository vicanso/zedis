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

//! The async channel every background loop ferries its batches over.
//!
//! This is `async-channel`, which is also exactly what `smol::channel` was —
//! smol re-exports this crate unchanged. Naming it directly costs nothing and
//! buys the browser build: `smol` as a whole reaches `async-io` → `rustix` →
//! `errno`, none of which has a wasm port, while the channel on its own is
//! pure `alloc` and compiles anywhere (ADR 9).
//!
//! The pattern these serve is in CLAUDE.md: a cancellable `gpui::Task` owns a
//! `cx.background_spawn` loop, the loop sends batches down one of these, and a
//! foreground drainer applies them. Dropping the task drops the sender, the
//! receiver ends, and the loop is gone.

pub use async_channel::{Receiver, RecvError, SendError, Sender, TryRecvError, TrySendError, bounded, unbounded};
