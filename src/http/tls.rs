//! TLS for the HTTP listener: the gRPC listener's PEM identity
//! (`MACP_TLS_CERT_PATH`/`MACP_TLS_KEY_PATH`) served through `tokio-rustls`
//! with the `ring` provider chosen explicitly, and ALPN offering `h2` then
//! `http/1.1` (RFC-MACP-0006 §4.9).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::{self, ServerConfig};
use tokio_rustls::TlsAcceptor;

/// A TLS handshake that has not completed in this long is dropped.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// ALPN protocol ids, in server preference order.
pub const ALPN: [&[u8]; 2] = [b"h2", b"http/1.1"];

/// Build the acceptor from PEM files. Every failure names the variable and
/// the file, and is fatal at startup.
pub fn acceptor(cert_path: &Path, key_path: &Path) -> Result<TlsAcceptor, String> {
    let certs = CertificateDer::pem_file_iter(cert_path)
        .and_then(|it| it.collect::<Result<Vec<_>, _>>())
        .map_err(|e| {
            format!(
                "MACP_TLS_CERT_PATH: cannot read certificates from {}: {e}",
                cert_path.display()
            )
        })?;
    if certs.is_empty() {
        return Err(format!(
            "MACP_TLS_CERT_PATH: no PEM certificate in {}",
            cert_path.display()
        ));
    }
    let key = PrivateKeyDer::from_pem_file(key_path).map_err(|e| {
        format!(
            "MACP_TLS_KEY_PATH: cannot read a private key from {}: {e}",
            key_path.display()
        )
    })?;
    server_config(certs, key).map(|c| TlsAcceptor::from(Arc::new(c)))
}

fn server_config(
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<ServerConfig, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("HTTP listener TLS: {e}"))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("MACP_TLS_CERT_PATH/MACP_TLS_KEY_PATH: unusable TLS identity: {e}"))?;
    config.alpn_protocols = ALPN.iter().map(|p| p.to_vec()).collect();
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_identity(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let ck = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let cert = dir.join("cert.pem");
        let key = dir.join("key.pem");
        std::fs::write(&cert, ck.cert.pem()).unwrap();
        std::fs::write(&key, ck.signing_key.serialize_pem()).unwrap();
        (cert, key)
    }

    #[test]
    fn builds_from_pem_with_alpn() {
        let dir = tempfile::tempdir().unwrap();
        let (cert, key) = write_identity(dir.path());
        let ck = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let config = server_config(
            vec![ck.cert.der().clone()],
            PrivateKeyDer::Pkcs8(ck.signing_key.serialize_der().into()),
        )
        .unwrap();
        assert_eq!(
            config.alpn_protocols,
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        );
        acceptor(&cert, &key).expect("acceptor");
    }

    #[test]
    fn bad_files_name_their_variable() {
        let dir = tempfile::tempdir().unwrap();
        let (cert, key) = write_identity(dir.path());
        let junk = dir.path().join("junk.pem");
        std::fs::write(&junk, "not pem").unwrap();
        let missing = dir.path().join("missing.pem");

        let e = acceptor(&junk, &key).err().unwrap();
        assert!(e.contains("MACP_TLS_CERT_PATH: no PEM certificate"), "{e}");
        let e = acceptor(&missing, &key).err().unwrap();
        assert!(e.contains("MACP_TLS_CERT_PATH: cannot read"), "{e}");
        let e = acceptor(&cert, &junk).err().unwrap();
        assert!(e.contains("MACP_TLS_KEY_PATH"), "{e}");
        // A key that does not match the certificate.
        let other = dir.path().join("other");
        std::fs::create_dir(&other).unwrap();
        let (_, other_key) = write_identity(&other);
        let e = acceptor(&cert, &other_key).err().unwrap();
        assert!(e.contains("unusable TLS identity"), "{e}");
    }
}
