//! The connection: QUIC on this app's address on the relay. The streaming
//! app shows a certificate made for this stream; the viewer accepts only
//! the one whose fingerprint the link carries, so no one between, the
//! meeting point included, can stand in for it.

use anyhow::{Context, Result, bail};
use quinn::{
    AsyncUdpSocket, ClientConfig, Endpoint, EndpointConfig, IdleTimeout, ServerConfig,
    TokioRuntime, TransportConfig,
    crypto::rustls::{QuicClientConfig, QuicServerConfig},
};
use rustls::{
    DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{CryptoProvider, ring},
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
};
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};

/// What the connection speaks, so nothing else is mistaken for it.
const ALPN: &[u8] = b"shuttercrab-live/0";
/// The name in the certificate. It is the fingerprint that is checked.
const NAME: &str = "shuttercrab";

/// A certificate for one stream, and its fingerprint for the link.
pub struct Identity {
    certificate: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
    pub fingerprint: [u8; 32],
}

impl Identity {
    pub fn new() -> Result<Self> {
        let made = rcgen::generate_simple_self_signed(vec![NAME.to_string()])?;
        let certificate = made.cert.der().clone();
        Ok(Self {
            fingerprint: Sha256::digest(&certificate).into(),
            key: PrivatePkcs8KeyDer::from(made.signing_key.serialize_der()),
            certificate,
        })
    }
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(ring::default_provider())
}

/// Kept alive through quiet moments, and given up on once nothing has
/// come for a while; windows wide enough for a fast stream far away.
fn transport() -> Arc<TransportConfig> {
    let mut transport = TransportConfig::default();
    transport
        .keep_alive_interval(Some(Duration::from_secs(2)))
        .max_idle_timeout(Some(
            IdleTimeout::try_from(Duration::from_secs(15)).expect("in range"),
        ))
        .stream_receive_window(quinn::VarInt::from_u32(16 << 20))
        .receive_window(quinn::VarInt::from_u32(32 << 20))
        .send_window(32 << 20);
    Arc::new(transport)
}

/// An endpoint on `socket` (a plain one, or the relay) that accepts
/// connections as `identity`.
pub fn server(socket: Arc<dyn AsyncUdpSocket>, identity: &Identity) -> Result<Endpoint> {
    let mut tls = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(
            vec![identity.certificate.clone()],
            PrivateKeyDer::Pkcs8(identity.key.clone_key()),
        )?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let mut config = ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls)?));
    config.transport_config(transport());
    Ok(Endpoint::new_with_abstract_socket(
        EndpointConfig::default(),
        Some(config),
        socket,
        Arc::new(TokioRuntime),
    )?)
}

/// An endpoint on `socket` (a plain one, or the relay) that connects only
/// to the app whose certificate has `fingerprint`.
pub fn client(socket: Arc<dyn AsyncUdpSocket>, fingerprint: [u8; 32]) -> Result<Endpoint> {
    let mut tls = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned {
            fingerprint,
            provider: provider(),
        }))
        .with_no_client_auth();
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let mut config = ClientConfig::new(Arc::new(QuicClientConfig::try_from(tls)?));
    config.transport_config(transport());
    let mut endpoint = Endpoint::new_with_abstract_socket(
        EndpointConfig::default(),
        None,
        socket,
        Arc::new(TokioRuntime),
    )?;
    endpoint.set_default_client_config(config);
    Ok(endpoint)
}

/// Connect `endpoint` to `address`, the app the link is for.
pub async fn connect(
    endpoint: &Endpoint,
    address: std::net::SocketAddr,
) -> Result<quinn::Connection> {
    endpoint
        .connect(address, NAME)?
        .await
        .with_context(|| format!("could not connect to {address}"))
}

/// The certificate check: the one certificate whose fingerprint the link
/// carries, rather than any a certificate authority signed.
#[derive(Debug)]
struct Pinned {
    fingerprint: [u8; 32],
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let fingerprint: [u8; 32] = Sha256::digest(end_entity).into();
        if same(&fingerprint, &self.fingerprint) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "not the certificate the link is for".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        // QUIC is TLS 1.3 only.
        Err(rustls::Error::General("TLS 1.2 is not used".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Whether `a` and `b` are equal, taking the same time either way.
pub fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |d, (x, y)| d | (x ^ y)) == 0
}

/// The viewer's first act: show the link's `token` on a stream of its own.
pub async fn show_token(connection: &quinn::Connection, token: &[u8; 16]) -> Result<()> {
    let mut stream = connection.open_uni().await?;
    stream.write_all(token).await?;
    stream.finish()?;
    Ok(())
}

/// The streaming app's first act: see that the viewer has the link's
/// `token`, and close the connection if not.
pub async fn check_token(connection: &quinn::Connection, token: &[u8; 16]) -> Result<()> {
    let shown = tokio::time::timeout(Duration::from_secs(10), async {
        let mut stream = connection.accept_uni().await?;
        anyhow::Ok(stream.read_to_end(token.len()).await?)
    })
    .await
    .context("the viewer showed no token")??;
    if !same(&shown, token) {
        connection.close(1u32.into(), b"not the link's token");
        bail!("a viewer without the link's token was turned away");
    }
    Ok(())
}
