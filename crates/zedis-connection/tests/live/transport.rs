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

//! What the connection travels over: TLS, mutual TLS and SSH tunnels.

use super::*;

#[test]
#[ignore]
fn tls_connects_with_the_root_cert_and_in_insecure_mode() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_TLS");
        let ca = std::fs::read_to_string(env::var("ZEDIS_IT_TLS_CA").expect("ZEDIS_IT_TLS_CA")).expect("read ca");

        let mut trusted = server("it-tls", addr.clone());
        trusted.tls = Some(true);
        trusted.root_cert = Some(ca);
        let id = register(trusted).await;
        let client = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("tls client (root cert)");
        client.ping().await.expect("ping over tls");

        let mut insecure = server("it-tls-insecure", addr.clone());
        insecure.tls = Some(true);
        insecure.insecure = Some(true);
        let id = register(insecure).await;
        let client = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("tls client (insecure)");
        client.ping().await.expect("ping over tls (insecure)");

        // Plaintext against a TLS-only port must fail, not hang.
        let id = register(server("it-tls-plain", addr)).await;
        assert!(get_connection_manager().get_client(&id, 0).await.is_err());
    });
}

/// A TLS server name that is not the dialed host: the certificate is checked
/// against that name and the handshake carries it, on the connection Zedis
/// dials itself (`open_single_sni_tls_connection`) — an endpoint reached by
/// IP whose certificate names a host. The test server's certificate names
/// `127.0.0.1` and `localhost`, so a name it does not carry must be refused:
/// were the name ignored, the IP alone would pass.
#[test]
#[ignore]
fn tls_checks_the_certificate_against_the_server_name_it_was_given() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_TLS");
        let ca = std::fs::read_to_string(env::var("ZEDIS_IT_TLS_CA").expect("ZEDIS_IT_TLS_CA")).expect("read ca");
        let named = |id: &str, name: &str| {
            let mut entry = server(id, addr.clone());
            entry.tls = Some(true);
            entry.root_cert = Some(ca.clone());
            entry.tls_server_name = Some(name.to_string());
            entry
        };

        let id = register(named("it-tls-sni", "localhost")).await;
        let client = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("tls client under the certificate's name");
        client.ping().await.expect("ping over tls");

        let id = register(named("it-tls-sni-wrong", "wrong.example")).await;
        assert!(
            get_connection_manager().get_client(&id, 0).await.is_err(),
            "a name the certificate does not carry is refused"
        );
    });
}

/// Mutual TLS: the server verifies the client too (`tls-auth-clients yes`).
///
/// Three things the plain-TLS test cannot reach — the `client_cert` /
/// `client_key` pair actually being presented, a PKCS#8-encrypted key being
/// decrypted with `client_key_passphrase`, and a certificate-less client
/// being *refused* rather than quietly connecting. The last one is what
/// makes the first two mean anything.
#[test]
#[ignore]
fn mtls_requires_the_client_certificate() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_MTLS");
        let ca = env::var("ZEDIS_IT_TLS_CA").expect("ZEDIS_IT_TLS_CA");
        let cert = env::var("ZEDIS_IT_TLS_CLIENT_CERT").expect("ZEDIS_IT_TLS_CLIENT_CERT");
        let key = env::var("ZEDIS_IT_TLS_CLIENT_KEY").expect("ZEDIS_IT_TLS_CLIENT_KEY");
        let encrypted_key = env::var("ZEDIS_IT_TLS_CLIENT_KEY_ENC").expect("ZEDIS_IT_TLS_CLIENT_KEY_ENC");
        let passphrase = env::var("ZEDIS_IT_TLS_CLIENT_KEY_PASSPHRASE").expect("ZEDIS_IT_TLS_CLIENT_KEY_PASSPHRASE");

        // Paths, not pasted PEM: `tls_material` takes either, and the file
        // form is the one the connection dialog writes.
        let mut client = server("it-mtls", addr.clone());
        client.tls = Some(true);
        client.root_cert = Some(ca.clone());
        client.client_cert = Some(cert.clone());
        client.client_key = Some(key);
        let id = register(client).await;
        get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("mtls client")
            .ping()
            .await
            .expect("ping over mtls");

        // The same certificate with its key encrypted: only the passphrase
        // path can open it.
        let mut encrypted = server("it-mtls-encrypted-key", addr.clone());
        encrypted.tls = Some(true);
        encrypted.root_cert = Some(ca.clone());
        encrypted.client_cert = Some(cert);
        encrypted.client_key = Some(encrypted_key.clone());
        encrypted.client_key_passphrase = Some(passphrase);
        let id = register(encrypted).await;
        get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("mtls client (encrypted key)")
            .ping()
            .await
            .expect("ping over mtls with an encrypted key");

        // …and without it the key is unreadable, so the connection never
        // even gets to the handshake.
        let mut no_passphrase = server("it-mtls-no-passphrase", addr.clone());
        no_passphrase.tls = Some(true);
        no_passphrase.root_cert = Some(ca.clone());
        no_passphrase.client_key = Some(encrypted_key);
        let id = register(no_passphrase).await;
        assert!(get_connection_manager().get_client(&id, 0).await.is_err());

        // Trusting the server is not enough when the server also wants to
        // trust you.
        let mut anonymous = server("it-mtls-anonymous", addr);
        anonymous.tls = Some(true);
        anonymous.root_cert = Some(ca);
        let id = register(anonymous).await;
        let refused = match get_connection_manager().get_client(&id, 0).await {
            Err(e) => Err(e),
            // Some TLS stacks only surface the peer's alert on first use,
            // so a handshake that "succeeded" still has to fail here.
            Ok(client) => client.ping().await,
        };
        assert!(refused.is_err(), "a client without a certificate was accepted");
    });
}

/// The SSH tunnel, against a real sshd: an encrypted key that needs its
/// passphrase, a plain key, and a login that must be refused.
///
/// **The order inside this test matters.** SSH sessions are cached globally
/// by `user@addr` ([`SshTarget::cache_id`]), so the second entry with the
/// same user and host reuses the first one's session instead of
/// authenticating again. The cases that have to *authenticate* therefore run
/// before any session exists, and the one negative case that must not be
/// answered from the cache uses a different user.
#[test]
#[ignore]
fn ssh_tunnel_carries_the_connection_to_the_standalone_server() {
    smol::block_on(async {
        let (ssh_host, ssh_port) = skip_unless!("ZEDIS_IT_SSH");
        let user = env::var("ZEDIS_IT_SSH_USER").expect("ZEDIS_IT_SSH_USER");
        let key = env::var("ZEDIS_IT_SSH_KEY").expect("ZEDIS_IT_SSH_KEY");
        let encrypted_key = env::var("ZEDIS_IT_SSH_KEY_ENC").expect("ZEDIS_IT_SSH_KEY_ENC");
        let passphrase = env::var("ZEDIS_IT_SSH_KEY_PASSPHRASE").expect("ZEDIS_IT_SSH_KEY_PASSPHRASE");
        let ssh_addr = format!("{ssh_host}:{ssh_port}");

        let tunnelled = |id: &str| {
            let mut s = server(id, standalone());
            s.ssh_tunnel = Some(true);
            s.ssh_addr = Some(ssh_addr.clone());
            s.ssh_username = Some(user.clone());
            s
        };

        // 1. An encrypted key with no passphrase cannot be read at all —
        //    and this runs first, so no cached session can mask it.
        let mut no_passphrase = tunnelled("it-ssh-no-passphrase");
        no_passphrase.ssh_key = Some(encrypted_key.clone());
        let id = register(no_passphrase).await;
        assert!(
            get_connection_manager().get_client(&id, 0).await.is_err(),
            "an encrypted key was accepted without its passphrase"
        );

        // 2. The same key with the passphrase: a real authentication, since
        //    nothing has succeeded yet.
        let mut encrypted = tunnelled("it-ssh-encrypted-key");
        encrypted.ssh_key = Some(encrypted_key);
        encrypted.ssh_key_passphrase = Some(passphrase);
        let id = register(encrypted).await;
        get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("tunnelled client (encrypted key)")
            .ping()
            .await
            .expect("ping through the tunnel");

        // 3. A plain key, and a round trip that proves the forwarded stream
        //    really reaches the standalone server: the value is written
        //    through the tunnel and read back on a direct connection.
        let mut plain = tunnelled("it-ssh");
        plain.ssh_key = Some(key);
        let id = register(plain).await;
        let client = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("tunnelled client");
        assert_eq!(client.nodes_description().server_type, "Standalone");
        let name = unique("ssh");
        let mut tunnel_conn = conn(&id, 0).await;
        cmd("SET")
            .arg(&name)
            .arg("through-the-tunnel")
            .exec_async(&mut tunnel_conn)
            .await
            .expect("set through the tunnel");

        let direct = register(server("it-ssh-direct", standalone())).await;
        let mut direct_conn = conn(&direct, 0).await;
        let value: String = cmd("GET")
            .arg(&name)
            .query_async(&mut direct_conn)
            .await
            .expect("get directly");
        assert_eq!(value, "through-the-tunnel", "the tunnel reached the real server");
        cmd("DEL")
            .arg(&name)
            .exec_async(&mut direct_conn)
            .await
            .expect("cleanup");

        // A subscription goes through the tunnel too, on a push connection:
        // the classic Pub/Sub connection dialled the entry's address itself,
        // which for a tunnel is the far side's `127.0.0.1`. Here, after the
        // cases that must authenticate, because the session is cached.
        let channel = unique("ssh-sub");
        let pattern = format!("{channel}:*");
        let mut subscription =
            ChannelSubscription::open(&ServerDb::new(&*id, 0), SubscribeKind::Patterns, &[pattern.as_str()])
                .await
                .expect("psubscribe through the tunnel");
        let receivers: i64 = cmd("PUBLISH")
            .arg(format!("{channel}:a"))
            .arg("hello")
            .query_async(&mut direct_conn)
            .await
            .expect("publish");
        assert_eq!(receivers, 1, "the tunnelled subscription is listening");
        let message = subscription.next_message().await.expect("a message");
        assert_eq!(
            (message.channel.as_str(), message.payload.as_slice()),
            (format!("{channel}:a").as_str(), b"hello".as_slice())
        );
        drop(subscription);

        // MONITOR through the tunnel as well. redis-rs's `Monitor` cannot be
        // built on a tunnel, so the feed dialled the entry's address from
        // here; the tunnelled one is read by hand and names itself, which is
        // how this test tells the two apart — sshd and Redis share a host
        // here, so the old direct dial would have worked too.
        let is_our_monitor = |client: &str| {
            client
                .split(' ')
                .any(|field| field == format!("name={}", client_name()))
                && client
                    .split(' ')
                    .any(|field| field.strip_prefix("flags=").is_some_and(|flags| flags.contains('O')))
        };
        let our_monitors = |list: String| list.lines().filter(|client| is_our_monitor(client)).count();
        let opened = open_monitor_feeds(&ServerDb::new(&*id, 0)).await.expect("feeds");
        assert!(opened.failures.is_empty(), "{:?}", opened.failures);
        let [mut feed] = <[_; 1]>::try_from(opened.feeds)
            .ok()
            .expect("a standalone has one master");
        let clients: String = cmd("CLIENT")
            .arg("LIST")
            .query_async(&mut direct_conn)
            .await
            .expect("client list");
        assert_eq!(our_monitors(clients), 1, "the feed is not the tunnelled MONITOR");
        let marker = unique("ssh-monitored");
        let _: Option<String> = cmd("GET")
            .arg(&marker)
            .query_async(&mut direct_conn)
            .await
            .expect("get");
        let line = monitored(&mut feed, &marker)
            .await
            .expect("the GET never came through the tunnelled feed");
        assert!(line.to_ascii_uppercase().contains("\"GET\""), "{line}");
        // Dropping the feed closes its connection, busy server or idle.
        drop(feed);
        let closed = wait_until(Duration::from_secs(10), Duration::from_millis(100), async || {
            let clients: String = cmd("CLIENT")
                .arg("LIST")
                .query_async(&mut direct_conn)
                .await
                .expect("client list");
            our_monitors(clients) == 0
        })
        .await;
        assert!(closed, "the tunnelled MONITOR outlived its feed");

        // And through a password, which the tunnelled feed sends itself:
        // Sentinel's master is the topology's password-protected data node.
        if let Some(sentinel) = scenario("ZEDIS_IT_SENTINEL") {
            let sentinel_id = register(sentinel_server("it-ssh-monitor-sentinel", sentinel)).await;
            let masters = master_addrs(&ServerDb::new(&*sentinel_id, 0)).await.expect("masters");
            let (host, port) = masters
                .first()
                .and_then(|addr| addr.rsplit_once(':'))
                .expect("the master's host:port");
            let addr = (host.to_string(), port.parse::<u16>().expect("port"));
            let mut protected = protected_server("it-ssh-monitor-auth", addr.clone());
            protected.ssh_tunnel = Some(true);
            protected.ssh_addr = Some(ssh_addr.clone());
            protected.ssh_username = Some(user.clone());
            protected.ssh_key = Some(env::var("ZEDIS_IT_SSH_KEY").expect("ZEDIS_IT_SSH_KEY"));
            let protected_id = register(protected).await;
            let opened = open_monitor_feeds(&ServerDb::new(&*protected_id, 0))
                .await
                .expect("feeds");
            assert!(opened.failures.is_empty(), "{:?}", opened.failures);
            let [mut feed] = <[_; 1]>::try_from(opened.feeds).ok().expect("one node");
            let direct = register(protected_server("it-ssh-monitor-auth-direct", addr)).await;
            let mut direct = conn(&direct, 0).await;
            let marker = unique("ssh-monitored-auth");
            let _: Option<String> = cmd("GET").arg(&marker).query_async(&mut direct).await.expect("get");
            monitored(&mut feed, &marker)
                .await
                .expect("the GET never came through the authenticated tunnelled feed");
        }

        // 4. A login the sshd cannot grant. Its own `user@addr`, so the
        //    successful session above is not reused for it.
        let mut refused = tunnelled("it-ssh-wrong-user");
        refused.ssh_username = Some("zedis-it-no-such-user".to_string());
        refused.ssh_key = Some(env::var("ZEDIS_IT_SSH_KEY").expect("ZEDIS_IT_SSH_KEY"));
        let id = register(refused).await;
        assert!(
            get_connection_manager().get_client(&id, 0).await.is_err(),
            "an unknown ssh user was let in"
        );

        // 5. An RSA key. It reaches the same sshd on its own port, so this is
        //    a real handshake and not the cached session from case 2 — the
        //    point of the case, because RSA is the one algorithm whose
        //    signature is negotiated: signed as the legacy ssh-rsa (SHA-1) it
        //    is refused by every OpenSSH since 8.8, and the fixture's sshd
        //    pins SHA-2 so an older host cannot let it pass either.
        let rsa_addr = env::var("ZEDIS_IT_SSH_RSA").expect("ZEDIS_IT_SSH_RSA");
        let rsa_key = env::var("ZEDIS_IT_SSH_KEY_RSA").expect("ZEDIS_IT_SSH_KEY_RSA");
        let mut rsa = server("it-ssh-rsa", standalone());
        rsa.ssh_tunnel = Some(true);
        rsa.ssh_addr = Some(rsa_addr);
        rsa.ssh_username = Some(user.clone());
        rsa.ssh_key = Some(rsa_key);
        let id = register(rsa).await;
        get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("tunnelled client (rsa key)")
            .ping()
            .await
            .expect("ping through the rsa-authenticated tunnel");
    });
}

/// TLS *inside* the tunnel: the forwarded stream carries the handshake, so
/// the certificate is checked against the endpoint the sshd dials, not the
/// sshd. Its own test (and the RSA port, so its own `user@addr`) because
/// neither the `tls` nor the `ssh` test ever combined the two.
#[test]
#[ignore]
fn ssh_tunnel_carries_a_tls_connection() {
    smol::block_on(async {
        let addr = skip_unless!("ZEDIS_IT_TLS");
        let Ok(ssh_addr) = env::var("ZEDIS_IT_SSH_RSA") else {
            eprintln!("skipped: ZEDIS_IT_SSH_RSA not set");
            return;
        };
        let ca = std::fs::read_to_string(env::var("ZEDIS_IT_TLS_CA").expect("ZEDIS_IT_TLS_CA")).expect("read ca");

        let mut tunnelled = server("it-ssh-tls", addr);
        tunnelled.tls = Some(true);
        tunnelled.root_cert = Some(ca);
        tunnelled.ssh_tunnel = Some(true);
        tunnelled.ssh_addr = Some(ssh_addr);
        tunnelled.ssh_username = Some(env::var("ZEDIS_IT_SSH_USER").expect("ZEDIS_IT_SSH_USER"));
        tunnelled.ssh_key = Some(env::var("ZEDIS_IT_SSH_KEY").expect("ZEDIS_IT_SSH_KEY"));
        let mut unreachable = tunnelled.clone();
        let id = register(tunnelled).await;
        let client = get_connection_manager()
            .get_client(&id, 0)
            .await
            .expect("tls client through the tunnel");
        client.ping().await.expect("ping over tls through the tunnel");

        // An endpoint the sshd cannot reach: the session is fine and the
        // forward is refused, so the error has to say *which* address the
        // SSH server was asked for — `ConnectFailed` alone names nothing. A
        // port that was just bound and released is closed.
        let closed_port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("a free port")
            .port();
        unreachable.id = "it-ssh-tls-unreachable".to_string();
        unreachable.name = unreachable.id.clone();
        unreachable.port = closed_port;
        let id = register(unreachable).await;
        let Err(e) = get_connection_manager().get_client(&id, 0).await else {
            panic!("a closed port answered through the tunnel");
        };
        let message = e.to_string();
        assert!(
            message.contains(&format!("127.0.0.1:{closed_port}")),
            "the refused forward does not name its destination: {message}"
        );
    });
}
