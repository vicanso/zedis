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

//! The TLS material of a [`RedisServer`]: the root certificate, the client
//! certificate and its key, each given as PEM content or as a path, the key
//! optionally encrypted. Native only — in the browser the bridge dials.

use super::*;

impl RedisServer {
    /// The root certificate as PEM bytes — the field's content, or the
    /// file it names. `None` when unset.
    ///
    /// Native only: TLS material is for dialling, which the bridge does.
    pub fn root_cert_pem(&self) -> Result<Option<Vec<u8>>> {
        self.root_cert.as_deref().map(tls_material).transpose()
    }

    /// The client certificate as PEM bytes. `None` when unset.
    ///
    /// Native only: TLS material is for dialling, which the bridge does.
    pub fn client_cert_pem(&self) -> Result<Option<Vec<u8>>> {
        self.client_cert.as_deref().map(tls_material).transpose()
    }

    /// The client key as *unencrypted* PEM bytes: the field's content or the
    /// file it names, decrypted with `client_key_passphrase` when it is a
    /// PKCS#8 `ENCRYPTED PRIVATE KEY`. `None` when unset.
    ///
    /// Native only: it may have to decrypt the key, and the browser presents
    /// no client certificate — the bridge dials (ADR 9).
    pub fn client_key_pem(&self) -> Result<Option<Vec<u8>>> {
        let Some(key) = self.client_key.as_deref() else {
            return Ok(None);
        };
        let pem = tls_material(key)?;
        let passphrase = self.client_key_passphrase.as_deref().map(str::trim).unwrap_or_default();
        decrypt_private_key_pem(pem, passphrase).map(Some)
    }

    /// The TLS material redis-rs needs, or `None` when TLS is off or every
    /// certificate field is empty (system roots, no client auth).
    /// The TLS material redis-rs needs to dial. Native only: the browser
    /// never opens a socket, so it never presents a certificate.
    pub fn tls_certificates(&self) -> Result<Option<TlsCertificates>> {
        if !self.tls.unwrap_or(false) {
            return Ok(None);
        }
        let mut client_tls = None;
        if let Some(client_cert) = self.client_cert_pem()?
            && let Some(client_key) = self.client_key_pem()?
        {
            client_tls = Some(ClientTlsConfig {
                client_cert,
                client_key,
            });
        }
        let root_cert = self.root_cert_pem()?;
        if client_tls.is_none() && root_cert.is_none() {
            return Ok(None);
        }
        Ok(Some(TlsCertificates { client_tls, root_cert }))
    }
}

/// A certificate field's bytes: pasted PEM is taken as is, anything else is
/// a path (`~` expanded) to read. Empty fields never reach here — the
/// callers filter them out.
fn tls_material(value: &str) -> Result<Vec<u8>> {
    let trimmed = value.trim();
    if trimmed.starts_with("-----BEGIN ") {
        return Ok(trimmed.as_bytes().to_vec());
    }
    let path = resolve_path(trimmed);
    std::fs::read(&path).map_err(|e| Error::Invalid {
        message: format!("could not read the certificate file {path}: {e}"),
    })
}

/// A private key as unencrypted PEM. A PKCS#8 `ENCRYPTED PRIVATE KEY` is
/// decrypted with `passphrase`; the legacy OpenSSL form (`Proc-Type:
/// 4,ENCRYPTED` inside a `BEGIN RSA PRIVATE KEY` block) is refused with a
/// pointer at `openssl pkcs8 -topk8`, which rewrites it as PKCS#8.
fn decrypt_private_key_pem(pem: Vec<u8>, passphrase: &str) -> Result<Vec<u8>> {
    let text = String::from_utf8_lossy(&pem);
    if text.contains("Proc-Type: 4,ENCRYPTED") {
        return Err(Error::Invalid {
            message: "the client key uses the legacy OpenSSL encryption; convert it with \
                      `openssl pkcs8 -topk8 -in key.pem -out key.pk8` (PKCS#8, which Zedis decrypts)"
                .to_string(),
        });
    }
    if !text.contains("ENCRYPTED PRIVATE KEY") {
        return Ok(pem);
    }
    if passphrase.is_empty() {
        return Err(Error::Invalid {
            message: "the client key is encrypted — fill in the client key passphrase".to_string(),
        });
    }
    let (_label, document) = pkcs8::SecretDocument::from_pem(&text).map_err(|e| Error::Invalid {
        message: format!("could not parse the encrypted client key: {e}"),
    })?;
    let encrypted = pkcs8::EncryptedPrivateKeyInfoRef::try_from(document.as_bytes()).map_err(|e| Error::Invalid {
        message: format!("could not parse the encrypted client key: {e}"),
    })?;
    let decrypted = encrypted.decrypt(passphrase).map_err(|e| Error::Invalid {
        message: format!("could not decrypt the client key (wrong passphrase?): {e}"),
    })?;
    let pem = decrypted
        .to_pem("PRIVATE KEY", pkcs8::LineEnding::LF)
        .map_err(|e| Error::Invalid {
            message: format!("could not re-encode the client key: {e}"),
        })?;
    Ok(pem.as_bytes().to_vec())
}
