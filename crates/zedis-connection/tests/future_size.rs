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

//! An operation's future has to stay small enough to inline a dozen of them.
//!
//! The app awaits these from one async block per task, and a caller like the
//! key editor's value load inlines *every* key type's first-load future into
//! a single state machine. A future that carries its whole connection-acquire
//! chain by value measured ~12 KB here, so that one frame outgrew the 512 KB
//! stack of a background thread — in a debug build, selecting a key crashed
//! with a stack-guard fault and no recursion in sight (26 frames).
//!
//! The fix is one `Box::pin` inside the connection accessors of `ServerDb`
//! and `ClusterNode`, which is invisible at every call site and costs one
//! allocation per operation. This test is what keeps it: the numbers are
//! debug-build sizes, and the cap is loose enough not to chase a compiler
//! release and tight enough to catch the chain being inlined by value again.

use zedis_connection::{
    ClusterNode, ServerDb, hash_scan, key_type_and_ttl, node_add_slots, node_load, scan_page, server_summary,
    stream_info,
};

/// Comfortably above what these measure (hundreds of bytes) and far below
/// what inlining the connection chain costs (~12 KB).
const MAX_FUTURE_BYTES: usize = 4096;

#[test]
fn an_operation_does_not_carry_its_connection_chain_on_the_stack() {
    let at = ServerDb::new("probe", 0);
    let node = ClusterNode::new("probe", "127.0.0.1:7000");
    // Nothing is awaited: constructing the future is what has the size, and
    // these never reach a server.
    let sizes = [
        ("key_type_and_ttl", size_of_val(&key_type_and_ttl(&at, "k"))),
        ("hash_scan", size_of_val(&hash_scan(&at, "k", None, 0, 10))),
        ("stream_info", size_of_val(&stream_info(&at, "k"))),
        ("scan_page", size_of_val(&scan_page(&at, None, "*", 10, false, None))),
        ("server_summary", size_of_val(&server_summary(&at))),
        // The node-addressed half dials per node, and a reshard opens
        // several in one loop.
        ("node_load", size_of_val(&node_load(&node))),
        ("node_add_slots", size_of_val(&node_add_slots(&node, &[1]))),
    ];
    let too_big: Vec<String> = sizes
        .iter()
        .filter(|(_, bytes)| *bytes > MAX_FUTURE_BYTES)
        .map(|(name, bytes)| format!("{name}: {bytes} bytes"))
        .collect();
    assert!(
        too_big.is_empty(),
        "an operation's future grew past {MAX_FUTURE_BYTES} bytes — a caller that inlines a \
         dozen of them will overflow a background thread's stack. Box the future that got \
         inlined (see `ServerDb::connection`):\n  {}\nall: {sizes:?}",
        too_big.join("\n  ")
    );
}
