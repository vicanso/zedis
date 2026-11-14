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

//! TLS this crate dials itself: rustls's process-level crypto provider, the
//! connector built from an entry's TLS settings (its CA or the web roots, a
//! client certificate, insecure mode), and a direct connection whose
//! certificate check and SNI use the configured server name rather than the
//! dialed host. The SSH tunnel's TLS is built here too.

use crate::async_connection::{resolve_connection_timeout, resolve_response_timeout};
use crate::config::RedisServer;
use crate::error::Error;
use crate::ssh_tunnel::run_in_tokio;
use redis::RedisConnectionInfo;
use redis::aio::MultiplexedConnection;
use rustls::pki_types::ServerName;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use std::sync::{Arc, Once};
use tokio_rustls::TlsConnector;
use tracing::{debug, info};

type Result<T, E = Error> = std::result::Result<T, E>;

/// A rustls `ServerCertVerifier` that accepts any server certificate.
/// Used when the user enables "insecure" / skip-verification mode.
#[derive(Debug)]
struct InsecureServerCertVerifier;

impl rustls::client::danger::ServerCertVerifier for InsecureServerCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Pick rustls's process-level crypto provider: ring, the backend every
/// TLS user in this tree (redis, tokio-rustls, ureq, russh) is built on.
/// With `aws-lc-rs` also in a binary — rustls's default, which gpui-kit's
/// HTTP client turns on — rustls refuses to choose on its own and
/// `ClientConfig::builder()` panics, in whichever thread first opens a TLS
/// connection. Idempotent, and a provider someone else installed first is
/// kept. `main` calls it before anything can dial; the TLS builder below
/// calls it too, for a binary that never went through `main`.
pub fn install_crypto_provider() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if rustls::crypto::ring::default_provider().install_default().is_err() {
            debug!("rustls crypto provider already installed; keeping it");
        }
    });
}

/// Builds a `TlsConnector` from the server's TLS configuration.
///
/// Handles insecure mode (skip verification), custom root CA, and
/// optional mTLS (client certificate + key).
pub(crate) fn build_tls_connector(config: &RedisServer) -> Result<TlsConnector> {
    install_crypto_provider();
    let insecure = config.insecure.unwrap_or(false);

    let builder = if insecure {
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(InsecureServerCertVerifier))
    } else {
        let mut root_store = rustls::RootCertStore::empty();
        if let Some(root_cert) = config.root_cert_pem()? {
            let certs: Vec<_> = CertificateDer::pem_slice_iter(&root_cert)
                .filter_map(|r| r.ok())
                .collect();
            root_store.add_parsable_certificates(certs);
        } else {
            root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        }
        rustls::ClientConfig::builder().with_root_certificates(root_store)
    };

    let tls_config = if let Some(client_cert) = config.client_cert_pem()?
        && let Some(client_key) = config.client_key_pem()?
    {
        let certs: Vec<_> = CertificateDer::pem_slice_iter(&client_cert)
            .filter_map(|r| r.ok())
            .collect();
        let key = PrivateKeyDer::from_pem_slice(&client_key).map_err(|e| Error::Invalid {
            message: format!("Failed to parse client key: {e}"),
        })?;
        builder.with_client_auth_cert(certs, key).map_err(|e| Error::Invalid {
            message: format!("TLS client auth config failed: {e}"),
        })?
    } else {
        builder.with_no_client_auth()
    };

    Ok(TlsConnector::from(Arc::new(tls_config)))
}

/// The configured TLS server name, if any.
pub(crate) fn tls_server_name(config: &RedisServer) -> Option<String> {
    config
        .tls_server_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// A direct TLS connection whose certificate check and SNI use
/// `server_name` instead of the dialed host. redis-rs's own connector
/// ties both to the URL host, which fails for an endpoint reached by IP:
/// the certificate carries its DNS name. Same stream plumbing as the
/// tunnel path, over a plain TCP socket.
pub async fn open_single_sni_tls_connection(config: &RedisServer, server_name: &str) -> Result<MultiplexedConnection> {
    let (host, port) = config.primary_endpoint();
    let connection_timeout = resolve_connection_timeout(config);
    let response_timeout = resolve_response_timeout(config);
    let tls_connector = build_tls_connector(config)?;
    let server_name = ServerName::try_from(server_name)
        .map_err(|_| Error::Invalid {
            message: format!("Invalid TLS server name: {server_name}"),
        })?
        .to_owned();
    let username = config.username.clone();
    let password = config.password.clone();
    let use_resp3 = config.uses_resp3();

    run_in_tokio(async move {
        let tcp = tokio::time::timeout(
            connection_timeout,
            tokio::net::TcpStream::connect((host.as_str(), port)),
        )
        .await
        .map_err(|_| Error::Invalid {
            message: format!("connect to {host}:{port} timed out"),
        })??;
        let tls_stream = tls_connector
            .connect(server_name, tcp)
            .await
            .map_err(|e| Error::Invalid {
                message: format!("TLS handshake failed: {e}"),
            })?;
        let mut info = RedisConnectionInfo::default();
        if let Some(user) = username.filter(|u| !u.trim().is_empty()) {
            info = info.set_username(user);
        }
        if let Some(pass) = password.filter(|p| !p.trim().is_empty()) {
            info = info.set_password(pass);
        }
        if use_resp3 {
            info = info.set_protocol(redis::ProtocolVersion::RESP3);
        }
        let conn_config = redis::AsyncConnectionConfig::new()
            .set_connection_timeout(Some(connection_timeout))
            .set_response_timeout(Some(response_timeout));
        let (connection, driver) = MultiplexedConnection::new_with_config(&info, tls_stream, conn_config).await?;
        tokio::spawn(async move {
            driver.await;
            info!("Redis driver task finished");
        });
        Ok(connection)
    })
    .await
}
