//! The gRPC connection: TLS for https, the token on every request, keepalive across the
//! quiet gaps between leader windows.

use std::time::Duration;

use tonic::{
    metadata::{errors::InvalidMetadataValue, Ascii, MetadataValue},
    service::Interceptor,
    transport::{Channel, ClientTlsConfig, Endpoint},
    Request, Status,
};

/// A failure before the first subscribe.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConnectError {
    /// The token has bytes a header cannot carry.
    #[error("the x-token is not a valid header value")]
    Token(#[source] InvalidMetadataValue),
    /// The endpoint is not a URI, or the TCP or TLS handshake failed.
    #[error("could not connect")]
    Transport(#[source] tonic::transport::Error),
}

impl From<ConnectError> for shipstern::Error {
    fn from(err: ConnectError) -> Self { Self::Other(Box::new(err)) }
}

/// Adds the `x-token` to every request, as the server needs it on each call.
#[derive(Clone)]
pub(crate) struct TokenInterceptor {
    token: Option<MetadataValue<Ascii>>,
}

impl TokenInterceptor {
    pub(crate) fn new(token: Option<&str>) -> Result<Self, ConnectError> {
        let token = token
            .map(str::parse)
            .transpose()
            .map_err(ConnectError::Token)?;
        Ok(Self { token })
    }
}

impl Interceptor for TokenInterceptor {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        if let Some(token) = &self.token {
            request.metadata_mut().insert("x-token", token.clone());
        }
        Ok(request)
    }
}

pub(crate) async fn channel(
    endpoint: &str,
    connect_timeout: Duration,
) -> Result<Channel, ConnectError> {
    let endpoint = Endpoint::from_shared(endpoint.to_string()).map_err(ConnectError::Transport)?;
    let tls = endpoint.uri().scheme_str() == Some("https");

    let mut endpoint = endpoint
        .connect_timeout(connect_timeout)
        .http2_keep_alive_interval(Duration::from_secs(30))
        .keep_alive_timeout(Duration::from_secs(10))
        .keep_alive_while_idle(true)
        // Slot bursts on a long path need more than the h2 default window.
        .http2_adaptive_window(true);
    if tls {
        // `connect_timeout` bounds the TCP connect only, so the handshake gets its own.
        let tls = ClientTlsConfig::new()
            .with_native_roots()
            .timeout(connect_timeout);
        endpoint = endpoint.tls_config(tls).map_err(ConnectError::Transport)?;
    }

    endpoint.connect().await.map_err(ConnectError::Transport)
}
