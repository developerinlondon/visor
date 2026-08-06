//! TLS configuration for the visor daemon.
//!
//! Provides mutual TLS (mTLS) support for the visor API, requiring client
//! certificates signed by a trusted CA for all connections. Uses the
//! `aws-lc-rs` crypto provider in FIPS mode.

use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use rustls::server::WebPkiClientVerifier;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::TlsAcceptor;

/// TLS configuration for the visor daemon.
///
/// All three paths must point to valid PEM-encoded files. The CA certificate
/// is used to verify client certificates during the TLS handshake — only
/// clients presenting a certificate signed by this CA will be accepted.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TlsConfig {
    /// Path to the server certificate (PEM).
    pub cert_path: PathBuf,
    /// Path to the server private key (PEM).
    pub key_path: PathBuf,
    /// Path to the CA certificate for client verification (PEM).
    pub ca_path: PathBuf,
}

impl TlsConfig {
    /// Create a new TLS config, validating that all files exist.
    ///
    /// # Errors
    ///
    /// Returns an error if any of the specified files do not exist.
    pub fn new(cert_path: PathBuf, key_path: PathBuf, ca_path: PathBuf) -> anyhow::Result<Self> {
        anyhow::ensure!(
            cert_path.exists(),
            "certificate file does not exist: {}",
            cert_path.display()
        );
        anyhow::ensure!(
            key_path.exists(),
            "private key file does not exist: {}",
            key_path.display()
        );
        anyhow::ensure!(
            ca_path.exists(),
            "CA certificate file does not exist: {}",
            ca_path.display()
        );

        Ok(Self {
            cert_path,
            key_path,
            ca_path,
        })
    }

    /// Build a rustls [`ServerConfig`](rustls::ServerConfig) with mutual TLS
    /// (client cert required).
    ///
    /// The returned configuration requires clients to present a valid
    /// certificate signed by the CA specified in [`ca_path`](Self::ca_path).
    /// Uses the `aws-lc-rs` FIPS crypto provider.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Certificate, key, or CA files cannot be read or parsed.
    /// - The server certificate or key is invalid.
    /// - The crypto provider cannot be initialized.
    pub fn build_server_config(&self) -> anyhow::Result<rustls::ServerConfig> {
        let certs = read_certs(&self.cert_path).context("failed to read server certificates")?;
        let key = read_private_key(&self.key_path).context("failed to read server private key")?;
        let ca_certs = read_certs(&self.ca_path).context("failed to read CA certificates")?;

        let mut root_store = rustls::RootCertStore::empty();
        for ca_cert in ca_certs {
            root_store
                .add(ca_cert)
                .context("failed to add CA certificate to root store")?;
        }

        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());

        let verifier = WebPkiClientVerifier::builder_with_provider(
            Arc::new(root_store),
            Arc::clone(&provider),
        )
        .build()
        .context("failed to build client certificate verifier")?;

        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .context("failed to set TLS protocol versions")?
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs, key)
            .context("failed to configure server certificate")?;

        Ok(config)
    }

    /// Build a [`TlsAcceptor`] from this config.
    ///
    /// Convenience wrapper around [`build_server_config`](Self::build_server_config)
    /// that wraps the result in a `tokio-rustls` acceptor ready for use with
    /// `tokio::net::TcpListener`.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying server config cannot be built.
    pub fn build_acceptor(&self) -> anyhow::Result<TlsAcceptor> {
        let config = self
            .build_server_config()
            .context("failed to build TLS acceptor")?;
        Ok(TlsAcceptor::from(Arc::new(config)))
    }
}

/// Read PEM-encoded certificates from a file.
fn read_certs(path: &Path) -> anyhow::Result<Vec<CertificateDer<'static>>> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("failed to open certificate file: {}", path.display()))?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .context("failed to parse PEM certificates")
}

/// Read a PEM-encoded private key from a file.
fn read_private_key(path: &Path) -> anyhow::Result<PrivateKeyDer<'static>> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("failed to open private key file: {}", path.display()))?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::private_key(&mut reader)
        .context("failed to parse PEM private key")?
        .context("no private key found in PEM file")
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
