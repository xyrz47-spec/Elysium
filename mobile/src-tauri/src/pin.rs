//! 인증서 지문 고정(pinning): PC가 처음 만든 자체 서명 인증서의 SHA-256만 믿는다.

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error, SignatureScheme};
use sha2::{Digest, Sha256};
use std::sync::Arc;

const MISMATCH: &str = "elysium-pin-mismatch";

#[derive(Debug)]
struct Pinned {
    fingerprint: String,
    provider: Arc<CryptoProvider>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(&self, end_entity: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, Error> {
        if hex(&Sha256::digest(end_entity.as_ref())) == self.fingerprint {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(Error::General(MISMATCH.into()))
        }
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// 이 지문만 믿는 TLS 설정
pub fn config(fingerprint: &str) -> rustls::ClientConfig {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("TLS 버전")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned { fingerprint: fingerprint.to_ascii_lowercase(), provider }))
        .with_no_client_auth()
}

/// 지문이 달라 끊겼는지(다른 기기가 끼어든 경우)
pub fn is_mismatch(e: &reqwest::Error) -> bool {
    let mut s: Option<&dyn std::error::Error> = Some(e);
    while let Some(x) = s {
        if x.to_string().contains(MISMATCH) {
            return true;
        }
        s = x.source();
    }
    false
}
