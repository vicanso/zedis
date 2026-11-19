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

//! What holds a connection of its own: the terminal's session, Pub/Sub,
//! keyspace notifications and MONITOR.

use super::*;

/// The terminal's session (ADR 10): one connection of its own, opened by the
/// first line and shared by its clones, holding what was typed into it — the
/// db a `SELECT` picked, a `MULTI` still open — and a fresh session is how
/// all of that is forgotten.
#[test]
#[ignore]
fn standalone_terminal_session_keeps_its_own_connection_state() {
    smol::block_on(async {
        let id = register(server("it-terminal-session", standalone())).await;
        let at = ServerDb::new(&*id, 0);
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<String>>();
        let session = TerminalSession::default();
        let key = unique("terminal");

        let moved = session.run(&at, "SELECT", &args(&["1"])).await.expect("select");
        assert_eq!(moved.selected_db(), Some(1));
        // A clone is the same connection: it is still on db 1.
        let set = session
            .clone()
            .run(&at, "SET", &args(&[&key, "v", "EX", "60"]))
            .await
            .expect("set");
        assert!(set.is_ok());
        let mut pooled = conn(&id, 0).await;
        let on_db0: bool = cmd("EXISTS").arg(&key).query_async(&mut pooled).await.expect("exists");
        assert!(!on_db0, "the terminal's SELECT leaked into the pooled connection");

        // MULTI … EXEC across separate calls: the transaction lives on the
        // session's connection.
        assert!(session.run(&at, "MULTI", &[]).await.expect("multi").is_ok());
        assert!(
            session
                .run(&at, "INCR", &args(&[&format!("{key}:n")]))
                .await
                .expect("incr")
                .is_queued()
        );
        assert!(session.run(&at, "GET", &args(&[&key])).await.expect("get").is_queued());
        let exec = session.run(&at, "EXEC", &[]).await.expect("exec");
        let replies = exec.exec_replies().expect("EXEC answers an array");
        assert_eq!(replies.len(), 2);
        let text = replies.render(&args(&["INCR n", "GET k"]), ReplyFormat::Text);
        assert!(text.contains("INCR n") && text.contains('v'), "{text}");

        // One command of a transaction failing at run time fails that
        // command, not the EXEC: the SET before it was applied and is shown.
        let list_key = format!("{key}:not-a-list");
        assert!(session.run(&at, "MULTI", &[]).await.expect("multi").is_ok());
        session
            .run(&at, "SET", &args(&[&list_key, "x"]))
            .await
            .expect("queue set");
        session
            .run(&at, "LPUSH", &args(&[&list_key, "y"]))
            .await
            .expect("queue lpush");
        let exec = session
            .run(&at, "EXEC", &[])
            .await
            .expect("an EXEC with one failed command is still an answer");
        let replies = exec.exec_replies().expect("EXEC answers an array");
        assert_eq!(replies.len(), 2);
        let text = replies.render(&args(&["SET", "LPUSH"]), ReplyFormat::Text);
        assert!(text.contains("OK") && text.contains("WRONGTYPE"), "{text}");
        session.run(&at, "DEL", &args(&[&list_key])).await.expect("cleanup");

        // A reply is rendered on demand, in any format, from the same value.
        let _ = session
            .run(&at, "HSET", &args(&[&format!("{key}:h"), "f", "1"]))
            .await
            .expect("hset");
        let hash = session
            .run(&at, "HGETALL", &args(&[&format!("{key}:h")]))
            .await
            .expect("hgetall");
        assert!(
            hash.render(ReplyFormat::Json).contains("\"f\": \"1\""),
            "pairs become an object"
        );
        assert!(hash.render(ReplyFormat::Table).contains("field"));
        assert!(
            session
                .run(&at, "GET", &args(&["zedis:it:no-such-key"]))
                .await
                .expect("get")
                .is_nil()
        );

        // A server error is an `Err` and does not cost the connection.
        let err = session
            .run(&at, "NOSUCHCOMMAND", &[])
            .await
            .expect_err("unknown command");
        assert!(!TerminalSession::drops_link(err.connection_kind()), "{err}");
        assert!(
            session.run(&at, "EXISTS", &args(&[&key])).await.is_ok(),
            "still on db 1"
        );

        // A fresh session starts over on the panel's db.
        let fresh = TerminalSession::default();
        let seen = fresh.run(&at, "EXISTS", &args(&[&key])).await.expect("exists");
        assert_eq!(
            seen.render(ReplyFormat::Text).trim(),
            "0",
            "a new session is back on db 0"
        );

        let _ = session
            .run(&at, "DEL", &args(&[&key, &format!("{key}:n"), &format!("{key}:h")]))
            .await
            .expect("cleanup");
    });
}

/// What the Pub/Sub and keyspace panels subscribe through: by pattern on the
/// classic transport, by exact name on the sharded one (Redis 7+), and either
/// way a message is the channel it was published to plus its bytes.
#[test]
#[ignore]
fn standalone_channel_subscription_delivers_both_kinds_of_pubsub() {
    smol::block_on(async {
        let id = register(server("it-channel-subscription", standalone())).await;
        let at = ServerDb::new(&*id, 0);
        let mut c = conn(&id, 0).await;
        let channel = unique("sub");
        let pattern = format!("{channel}:*");

        let mut subscription = ChannelSubscription::open(&at, SubscribeKind::Patterns, &[pattern.as_str()])
            .await
            .expect("psubscribe");
        let receivers: i64 = cmd("PUBLISH")
            .arg(format!("{channel}:a"))
            .arg(b"\xffbytes".as_slice())
            .query_async(&mut c)
            .await
            .expect("publish");
        assert_eq!(receivers, 1, "the subscription is live once `open` returns");
        // The panel's own publish answers the same count.
        assert_eq!(
            publish(&at, &format!("{channel}:b"), "second", false)
                .await
                .expect("publish"),
            1
        );
        let first = subscription.next_message().await.expect("first message");
        assert_eq!(first.channel, format!("{channel}:a"), "the channel, not the pattern");
        assert_eq!(first.payload, b"\xffbytes", "the payload is handed over undecoded");
        let second = subscription.next_message().await.expect("second message");
        assert_eq!(
            (second.channel.as_str(), second.payload.as_slice()),
            (format!("{channel}:b").as_str(), b"second".as_slice())
        );

        // Dropping it is the unsubscribe.
        drop(subscription);
        let gone = wait_until(Duration::from_secs(5), Duration::from_millis(50), async || {
            let receivers: i64 = cmd("PUBLISH")
                .arg(format!("{channel}:a"))
                .arg("x")
                .query_async(&mut c)
                .await
                .expect("publish");
            receivers == 0
        })
        .await;
        assert!(gone, "a dropped subscription must close its connection");

        if !supports(&id, floors::SHARDED_PUBSUB).await {
            eprintln!("skipped the sharded half: SSUBSCRIBE needs Redis 7");
            return;
        }
        let mut sharded = ChannelSubscription::open(&at, SubscribeKind::Sharded, &[channel.as_str()])
            .await
            .expect("ssubscribe");
        // The push connection acknowledges asynchronously: publish until the
        // server counts the subscriber.
        let delivered = wait_until(Duration::from_secs(5), Duration::from_millis(50), async || {
            publish(&at, &channel, "shard", true).await.expect("spublish") > 0
        })
        .await;
        assert!(delivered, "the sharded subscriber never showed up");
        let msg = sharded.next_message().await.expect("sharded message");
        assert_eq!(
            (msg.channel.as_str(), msg.payload.as_slice()),
            (channel.as_str(), b"shard".as_slice())
        );
    });
}

/// The Monitor panel's feeds: one per master, each naming its node, carrying
/// the server's own lines — and a server it cannot reach at all is the `Err`.
#[test]
#[ignore]
fn standalone_monitor_feed_carries_the_commands_the_server_receives() {
    smol::block_on(async {
        let (host, port) = standalone();
        let id = register(server("it-monitor-feed", (host.clone(), port))).await;
        let at = ServerDb::new(&*id, 0);
        let opened = open_monitor_feeds(&at).await.expect("feeds");
        assert!(opened.failures.is_empty(), "{:?}", opened.failures);
        let [mut feed] = <[_; 1]>::try_from(opened.feeds)
            .ok()
            .expect("a standalone has one master");
        assert_eq!(feed.node(), format!("{host}:{port}"));

        let marker = unique("monitored");
        let mut c = conn(&id, 0).await;
        let _: Option<String> = cmd("GET").arg(&marker).query_async(&mut c).await.expect("get");
        // The suite runs in parallel against this server, so the feed carries
        // everyone's commands; ours is in there.
        let line = monitored(&mut feed, &marker)
            .await
            .expect("the GET never appeared in the feed");
        assert!(line.to_ascii_uppercase().contains("\"GET\""), "{line}");

        let nowhere = ServerDb::new("it-no-such-server", 0);
        assert!(open_monitor_feeds(&nowhere).await.is_err());
    });
}

/// The channel browser: a channel is listed with its subscriber count
/// only while someone is subscribed to it, filtered by the glob, and the
/// sharded pair (`SHARDCHANNELS` / `SHARDNUMSUB`) sees shard subscriptions
/// — on every server that has sharded Pub/Sub at all.
#[test]
#[ignore]
fn standalone_pubsub_channels_lists_the_subscribed_ones() {
    smol::block_on(async {
        let id = register(server("it-pubsub-channels", standalone())).await;
        let client = get_connection_manager().get_client(&id, 0).await.expect("client");
        let channel = unique("pubsub");

        let none = client
            .pubsub_channels("zedis:it:pubsub:*", false)
            .await
            .expect("pubsub channels");
        assert!(
            !none.channels.iter().any(|c| c.name == channel),
            "nobody is subscribed yet: {none:?}"
        );

        let mut subscriber = get_connection_manager()
            .get_pubsub_connection(&id)
            .await
            .expect("pubsub connection");
        subscriber.subscribe(&channel).await.expect("subscribe");
        subscriber
            .psubscribe("zedis:it:pubsub:pattern:*")
            .await
            .expect("psubscribe");

        let listed = client
            .pubsub_channels("zedis:it:pubsub:*", false)
            .await
            .expect("pubsub channels");
        assert_eq!(listed.nodes, 1);
        assert_eq!(
            listed.channels.iter().find(|c| c.name == channel),
            Some(&PubsubChannel {
                name: channel.clone(),
                subscribers: 1
            }),
            "the subscribed channel is listed once with one subscriber: {listed:?}"
        );
        assert!(
            listed.pattern_subscriptions.expect("NUMPAT in classic mode") >= 1,
            "the pattern subscription counts: {listed:?}"
        );
        let other = client
            .pubsub_channels("zedis:it:elsewhere:*", false)
            .await
            .expect("pubsub channels");
        assert!(other.channels.is_empty(), "the glob filters: {other:?}");

        if supports(&id, floors::SHARDED_PUBSUB).await {
            let shard_channel = unique("spubsub");
            let mut shard_subscriber = get_connection_manager()
                .get_sharded_pubsub(&id)
                .await
                .expect("sharded pubsub");
            shard_subscriber
                .ssubscribe(&[shard_channel.as_str()])
                .await
                .expect("ssubscribe");
            let shards = client
                .pubsub_channels("zedis:it:spubsub:*", true)
                .await
                .expect("shard channels");
            assert_eq!(
                shards.channels,
                vec![PubsubChannel {
                    name: shard_channel,
                    subscribers: 1
                }],
                "the shard subscription is listed: {shards:?}"
            );
            assert!(shards.pattern_subscriptions.is_none(), "no NUMPAT in sharded mode");
            let classic = client
                .pubsub_channels("zedis:it:spubsub:*", false)
                .await
                .expect("pubsub channels");
            assert!(
                classic.channels.is_empty(),
                "a shard channel is not a classic one: {classic:?}"
            );
            drop(shard_subscriber);
        }
        drop(subscriber);
    });
}
