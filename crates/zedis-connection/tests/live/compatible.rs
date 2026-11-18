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

//! Dragonfly, Garnet, and KiviDB: smoke lanes, not matrix lanes. Each speaks
//! Redis's protocol and is not Redis: a dozen of the `standalone_*` tests do not
//! hold on Dragonfly and half of them do not on Garnet (no streams, no `DUMP`,
//! ACL selectors, `CLIENT PAUSE`, `OBJECT`, `PUBSUB CHANNELS`…), and holding
//! every new test to additional server implementations is a cost their share
//! of users does not pay for. What the README does promise is that they connect
//! and the basics work — which is what broke unnoticed (Dragonfly's two-element
//! `ROLE` failed the whole connect) for want of one test that dials it. These
//! are those tests: `IT_SCENARIOS=dragonfly scripts/it/up.sh` (or `garnet`,
//! `kividb`), then the filter of the same name. Everywhere else the variable is
//! unset and they skip.

use super::keys::scan_sees_every_type_it_wrote;
use super::values::collections_page_and_write_through_their_operations;
use super::*;

/// The connect is the first thing under test, then what it learned: which
/// product answered, in its own version, beside the Redis it claims.
async fn compatible_server_connects_and_is_named(id: String, flavor: ServerFlavor) {
    let client = get_connection_manager().get_client(&id, 0).await.expect("client");
    client.ping().await.expect("ping");
    assert_eq!(client.nodes(), (1, 1), "one master, one node in total");
    assert_eq!(format!("{:?}", client.access_mode()), "ReadWrite");
    assert!(!client.is_valkey(), "the floors' Redis column, not Valkey's");

    let description = client.nodes_description();
    assert_eq!(description.flavor, flavor);
    // Its own release as a bare number (Dragonfly's `INFO` says `df-v2.0.2`),
    // and the Redis it claims — which is what the floors are asked, so it
    // has to be a version in Redis's numbers and not the product's own 2.x.
    Version::parse(&description.flavor_version).expect("its own version, as a bare number");
    let claimed = Version::parse(&client.version()).expect("the Redis version it claims");
    assert!(
        claimed.major >= 6,
        "a floor is a question in Redis's numbers: {claimed}"
    );

    // A beat is one `INFO` on the client's own connection.
    let info = client
        .heartbeat_probe()
        .await
        .expect("beat")
        .expect("one master beats with INFO");
    let own_version = flavor
        .version_field()
        .expect("a compatible server names its own version");
    assert!(info.contains(&format!("{own_version}:")), "{info}");
    client.dbsize().await.expect("dbsize");

    // The probe is what decides which panels the app offers there.
    let features = probe_server_features(&id, 0).await.expect("probe");
    assert!(features.probed);
    assert_eq!(features.flavor, flavor);
    for command in [ServerCommand::Info, ServerCommand::Scan, ServerCommand::Dbsize] {
        assert_eq!(features.status(command), CommandStatus::Available, "{command:?}");
    }
}

#[test]
#[ignore]
fn dragonfly_connects_and_is_named_in_its_own_version() {
    let addr = skip_unless!("ZEDIS_IT_DRAGONFLY");
    smol::block_on(async {
        let id = register(server("it-dragonfly", addr)).await;
        compatible_server_connects_and_is_named(id, ServerFlavor::Dragonfly).await;
    });
}

#[test]
#[ignore]
fn dragonfly_scan_sees_every_type_it_wrote() {
    let addr = skip_unless!("ZEDIS_IT_DRAGONFLY");
    smol::block_on(async {
        let id = register(server("it-dragonfly", addr)).await;
        scan_sees_every_type_it_wrote(id).await;
    });
}

#[test]
#[ignore]
fn dragonfly_collections_page_and_write_through_their_operations() {
    let addr = skip_unless!("ZEDIS_IT_DRAGONFLY");
    smol::block_on(async {
        let id = register(server("it-dragonfly-collections", addr)).await;
        collections_page_and_write_through_their_operations(id).await;
    });
}

#[test]
#[ignore]
fn garnet_connects_and_is_named_in_its_own_version() {
    let addr = skip_unless!("ZEDIS_IT_GARNET");
    smol::block_on(async {
        let id = register(server("it-garnet", addr)).await;
        compatible_server_connects_and_is_named(id, ServerFlavor::Garnet).await;
    });
}

#[test]
#[ignore]
fn garnet_scan_sees_every_type_it_wrote() {
    let addr = skip_unless!("ZEDIS_IT_GARNET");
    smol::block_on(async {
        let id = register(server("it-garnet", addr)).await;
        scan_sees_every_type_it_wrote(id).await;
    });
}

#[test]
#[ignore]
fn garnet_collections_page_and_write_through_their_operations() {
    let addr = skip_unless!("ZEDIS_IT_GARNET");
    smol::block_on(async {
        let id = register(server("it-garnet-collections", addr)).await;
        collections_page_and_write_through_their_operations(id).await;
    });
}

#[test]
#[ignore]
fn kividb_connects_and_is_named_in_its_own_version() {
    let addr = skip_unless!("ZEDIS_IT_KIVIDB");
    smol::block_on(async {
        let id = register(server("it-kividb", addr)).await;
        compatible_server_connects_and_is_named(id, ServerFlavor::KiviDb).await;
    });
}

#[test]
#[ignore]
fn kividb_scan_sees_every_type_it_wrote() {
    let addr = skip_unless!("ZEDIS_IT_KIVIDB");
    smol::block_on(async {
        let id = register(server("it-kividb", addr)).await;
        scan_sees_every_type_it_wrote(id).await;
    });
}

#[test]
#[ignore]
fn kividb_collections_page_and_write_through_their_operations() {
    let addr = skip_unless!("ZEDIS_IT_KIVIDB");
    smol::block_on(async {
        let id = register(server("it-kividb-collections", addr)).await;
        collections_page_and_write_through_their_operations(id).await;
    });
}
