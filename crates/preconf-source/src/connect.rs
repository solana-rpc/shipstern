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
    /// The config names no harmonic region.
    #[error("the config names no harmonic region")]
    NoRegion,
    /// The endpoint scheme is neither `https` nor `http`.
    #[error(
        "the endpoint scheme is {}, the source dials only https or http",
        .0.as_deref().unwrap_or("missing")
    )]
    Scheme(Option<String>),
    /// An `http` endpoint on another host would carry the token in clear text.
    #[error("the source sends an x-token over http only to a loopback host")]
    PlainTextToken,
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

/// Adds the `x-token` to every request, because the server checks the token on each call.
#[derive(Clone)]
pub(crate) struct TokenInterceptor {
    token: Option<MetadataValue<Ascii>>,
}

impl TokenInterceptor {
    pub(crate) fn new(token: Option<&str>) -> Result<Self, ConnectError> {
        let mut token: Option<MetadataValue<Ascii>> = token
            .map(str::parse)
            .transpose()
            .map_err(ConnectError::Token)?;
        // A sensitive value stays out of header Debug output and HPACK tables.
        if let Some(token) = token.as_mut() {
            token.set_sensitive(true);
        }
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
    with_token: bool,
    connect_timeout: Duration,
) -> Result<Channel, ConnectError> {
    // tonic dials a `unix:` endpoint on a socket and reports its scheme as http.
    if endpoint.starts_with("unix:") {
        return Err(ConnectError::Scheme(Some("unix".to_string())));
    }
    let endpoint = Endpoint::from_shared(endpoint.to_string()).map_err(ConnectError::Transport)?;
    // tonic dials every scheme but https in clear text, so allow only these two.
    let tls = match endpoint.uri().scheme_str() {
        Some("https") => true,
        Some("http") => false,
        other => return Err(ConnectError::Scheme(other.map(str::to_owned))),
    };
    let loopback = matches!(
        endpoint.uri().host(),
        Some("127.0.0.1" | "localhost" | "[::1]")
    );
    if with_token && !tls && !loopback {
        return Err(ConnectError::PlainTextToken);
    }

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
