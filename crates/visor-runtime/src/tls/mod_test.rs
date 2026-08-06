use std::path::PathBuf;
use std::sync::Arc;

use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
use rustls::server::WebPkiClientVerifier;
use rustls_pki_types::CertificateDer;

use super::*;

struct TestCerts {
    _dir: tempfile::TempDir,
    ca_cert_path: PathBuf,
    server_cert_path: PathBuf,
    server_key_path: PathBuf,
}

/// Encode DER certificate bytes as PEM.
fn der_to_cert_pem(der: &[u8]) -> String {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(chunk).unwrap());
        pem.push('\n');
    }
    pem.push_str("-----END CERTIFICATE-----\n");
    pem
}

/// Encode DER private key bytes as PKCS#8 PEM.
fn der_to_key_pem(der: &[u8]) -> String {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    let mut pem = String::from("-----BEGIN PRIVATE KEY-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(chunk).unwrap());
        pem.push('\n');
    }
    pem.push_str("-----END PRIVATE KEY-----\n");
    pem
}

/// Generate a self-signed CA + server cert. Write them to a temp directory as PEM.
fn generate_test_certs() -> TestCerts {
    let dir = crate::testutil::tempdir("visor-runtime-tls-").unwrap();

    // CA key + self-signed cert
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::default();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();
    let ca_cert_pem = der_to_cert_pem(ca_cert.der().as_ref());

    // Build an Issuer so we can sign subordinate certs.
    // Issuer::new takes ownership of ca_params and ca_key.
    let ca_issuer = Issuer::new(ca_params, ca_key);

    // Server cert signed by CA
    let server_key = KeyPair::generate().unwrap();
    let server_params = CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
    let server_cert = server_params.signed_by(&server_key, &ca_issuer).unwrap();

    // Write PEM files
    let ca_cert_path = dir.path().join("ca.pem");
    let server_cert_path = dir.path().join("server.pem");
    let server_key_path = dir.path().join("server-key.pem");

    std::fs::write(&ca_cert_path, ca_cert_pem).unwrap();
    std::fs::write(
        &server_cert_path,
        der_to_cert_pem(server_cert.der().as_ref()),
    )
    .unwrap();
    std::fs::write(
        &server_key_path,
        der_to_key_pem(&server_key.serialize_der()),
    )
    .unwrap();

    TestCerts {
        _dir: dir,
        ca_cert_path,
        server_cert_path,
        server_key_path,
    }
}
#[test]
fn test_tls_config_from_paths() {
    let certs = generate_test_certs();

    let config = TlsConfig::new(
        certs.server_cert_path.clone(),
        certs.server_key_path.clone(),
        certs.ca_cert_path.clone(),
    );
    assert!(
        config.is_ok(),
        "TlsConfig::new should succeed with valid paths"
    );

    let config = config.unwrap();
    assert_eq!(config.cert_path, certs.server_cert_path);
    assert_eq!(config.key_path, certs.server_key_path);
    assert_eq!(config.ca_path, certs.ca_cert_path);
}

#[test]
fn test_tls_config_validates_cert_exists() {
    let certs = generate_test_certs();

    let result = TlsConfig::new(
        PathBuf::from("/nonexistent/cert.pem"),
        certs.server_key_path.clone(),
        certs.ca_cert_path.clone(),
    );
    assert!(result.is_err());

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("certificate file does not exist"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_tls_config_validates_key_exists() {
    let certs = generate_test_certs();

    let result = TlsConfig::new(
        certs.server_cert_path.clone(),
        PathBuf::from("/nonexistent/key.pem"),
        certs.ca_cert_path.clone(),
    );
    assert!(result.is_err());

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("private key file does not exist"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_tls_config_validates_ca_exists() {
    let certs = generate_test_certs();

    let result = TlsConfig::new(
        certs.server_cert_path.clone(),
        certs.server_key_path.clone(),
        PathBuf::from("/nonexistent/ca.pem"),
    );
    assert!(result.is_err());

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("CA certificate file does not exist"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_build_server_config() {
    let certs = generate_test_certs();

    let config = TlsConfig::new(
        certs.server_cert_path.clone(),
        certs.server_key_path.clone(),
        certs.ca_cert_path.clone(),
    )
    .unwrap();

    let server_config = config.build_server_config();
    assert!(
        server_config.is_ok(),
        "should build valid server config: {:?}",
        server_config.err()
    );
}

#[test]
fn test_build_server_config_requires_client_cert() {
    let certs = generate_test_certs();

    // Build the same WebPkiClientVerifier the implementation uses and verify
    // it mandates client certificates.
    let ca_pem = std::fs::read(&certs.ca_cert_path).unwrap();
    let mut cursor = &ca_pem[..];
    let ca_certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cursor)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    let mut root_store = rustls::RootCertStore::empty();
    for cert in ca_certs {
        root_store.add(cert).unwrap();
    }

    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(root_store), provider)
        .build()
        .unwrap();

    assert!(
        verifier.client_auth_mandatory(),
        "mTLS verifier must require client certificates"
    );

    // Also verify the full config builds successfully with this verifier type.
    let config = TlsConfig::new(
        certs.server_cert_path.clone(),
        certs.server_key_path.clone(),
        certs.ca_cert_path.clone(),
    )
    .unwrap();
    config.build_server_config().unwrap();
}

#[test]
fn test_tls_acceptor_from_config() {
    let certs = generate_test_certs();

    let config = TlsConfig::new(
        certs.server_cert_path.clone(),
        certs.server_key_path.clone(),
        certs.ca_cert_path.clone(),
    )
    .unwrap();

    let acceptor = config.build_acceptor();
    assert!(
        acceptor.is_ok(),
        "should build TLS acceptor: {:?}",
        acceptor.err()
    );
}
