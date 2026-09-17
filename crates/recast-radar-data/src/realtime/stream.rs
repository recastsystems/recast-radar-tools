//! Async [`Stream`] over real-time chunks (feature `async`).
//!
//! [`ChunkStream`] drives the same [`ChunkPlanner`] as the blocking
//! [`ChunkIterator`](super::iterator::ChunkIterator) with an
//! [`AsyncChunkTransport`], so both produce identical event sequences for
//! identical responses. The stream depends on no runtime: it never sleeps or
//! spawns. On [`ChunkEvent::Idle`] and [`ChunkEvent::Retry`] the caller waits
//! with its own runtime's timer before polling the stream again.
//!
//! Feature `async-client` adds `AsyncReqwestTransport`, a non-blocking
//! HTTPS client (reqwest). On native targets its requests run on a Tokio
//! runtime, as reqwest requires; on `wasm32-unknown-unknown`, which has no
//! blocking client, it uses the browser's `fetch`.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::Stream;

use super::iterator::{
    ChunkEvent, ChunkIterError, ChunkIteratorConfig, ChunkIteratorStats, ChunkPlanner,
    FetchRequest, PlannerStep, TransportError,
};

/// A non-blocking HTTP GET.
///
/// `fetch` returns a future that owns what it needs (clone the client into
/// it), so the stream can hold it across polls. Implementations return the
/// full body of a 2xx response and map everything else to a
/// [`TransportError`].
pub trait AsyncChunkTransport {
    /// The future performing one request.
    type Fetch: Future<Output = Result<Vec<u8>, TransportError>>;

    /// Start `request`.
    fn fetch(&mut self, request: &FetchRequest) -> Self::Fetch;
}

/// A [`Stream`] of [`ChunkEvent`]s for one site. It never ends.
pub struct ChunkStream<T: AsyncChunkTransport> {
    planner: ChunkPlanner,
    transport: T,
    in_flight: Option<Pin<Box<T::Fetch>>>,
}

// The stream never pins `transport` or the planner structurally (the in-flight
// future is boxed), so moving it after a poll is fine whatever `T` is.
impl<T: AsyncChunkTransport> Unpin for ChunkStream<T> {}

impl<T: AsyncChunkTransport> ChunkStream<T> {
    /// A stream for `site` over `transport`.
    pub fn new(site: &str, config: ChunkIteratorConfig, transport: T) -> Self {
        Self {
            planner: ChunkPlanner::new(site, config),
            transport,
            in_flight: None,
        }
    }

    /// The planner (position, configuration).
    pub fn planner(&self) -> &ChunkPlanner {
        &self.planner
    }

    /// Request and byte counters so far.
    pub fn stats(&self) -> ChunkIteratorStats {
        self.planner.stats()
    }

    /// The transport.
    pub fn transport(&self) -> &T {
        &self.transport
    }
}

impl<T: AsyncChunkTransport> std::fmt::Debug for ChunkStream<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChunkStream")
            .field("planner", &self.planner)
            .field("in_flight", &self.in_flight.is_some())
            .finish_non_exhaustive()
    }
}

impl<T: AsyncChunkTransport> Stream for ChunkStream<T> {
    type Item = Result<ChunkEvent, ChunkIterError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some(fetch) = this.in_flight.as_mut() {
                let result = match fetch.as_mut().poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(result) => result,
                };
                this.in_flight = None;
                this.planner.complete(result);
            }
            match this.planner.next_step() {
                PlannerStep::Fetch(request) => {
                    this.in_flight = Some(Box::pin(this.transport.fetch(&request)));
                }
                PlannerStep::Event(event) => return Poll::Ready(Some(event)),
            }
        }
    }
}

/// The future [`AsyncReqwestTransport`] returns for one request.
#[cfg(all(feature = "async-client", not(target_arch = "wasm32")))]
pub type ReqwestFetch = Pin<Box<dyn Future<Output = Result<Vec<u8>, TransportError>> + Send>>;

/// The future [`AsyncReqwestTransport`] returns for one request.
#[cfg(all(feature = "async-client", target_arch = "wasm32"))]
pub type ReqwestFetch = Pin<Box<dyn Future<Output = Result<Vec<u8>, TransportError>>>>;

/// [`AsyncChunkTransport`] over reqwest's async client (feature
/// `async-client`).
///
/// Status, timeout, connection and body failures map to
/// [`TransportErrorKind`](super::iterator::TransportErrorKind) as in the
/// blocking `ReqwestTransport`, and a body larger than
/// [`FetchRequest::max_bytes`] fails with `TooLarge` (from `Content-Length`
/// when present, else while reading). On native targets the futures must be
/// polled inside a Tokio runtime.
#[cfg(feature = "async-client")]
#[derive(Clone, Debug)]
pub struct AsyncReqwestTransport {
    client: reqwest::Client,
}

#[cfg(feature = "async-client")]
impl AsyncReqwestTransport {
    /// Request timeout on native targets (on wasm32 the browser applies its
    /// own).
    pub const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);

    /// A transport with a new client: on native targets rustls, a 10 s
    /// connect timeout and a [`Self::TIMEOUT`] request timeout.
    pub fn new() -> Result<Self, TransportError> {
        let builder = reqwest::Client::builder();
        #[cfg(not(target_arch = "wasm32"))]
        let builder = builder
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(Self::TIMEOUT)
            .user_agent(concat!("recast-radar-data/", env!("CARGO_PKG_VERSION")));
        builder
            .build()
            .map(Self::with_client)
            .map_err(async_transport_error)
    }

    /// A transport over a caller-configured client.
    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }
}

#[cfg(feature = "async-client")]
impl AsyncChunkTransport for AsyncReqwestTransport {
    type Fetch = ReqwestFetch;

    fn fetch(&mut self, request: &FetchRequest) -> Self::Fetch {
        let client = self.client.clone();
        let url = request.url.clone();
        let max_bytes = request.max_bytes;
        Box::pin(async move {
            use super::iterator::TransportErrorKind;

            let too_large = |bytes: String| {
                TransportError::new(
                    TransportErrorKind::TooLarge,
                    format!("{url}: body of {bytes} bytes exceeds the {max_bytes}-byte limit"),
                )
            };
            let response = client
                .get(&url)
                .send()
                .await
                .map_err(async_transport_error)?;
            let status = response.status();
            if !status.is_success() {
                return Err(TransportError::new(
                    TransportErrorKind::Status(status.as_u16()),
                    format!("{status} for {url}"),
                ));
            }
            if let Some(length) = response
                .content_length()
                .filter(|length| *length > max_bytes as u64)
            {
                return Err(too_large(length.to_string()));
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                let mut response = response;
                let mut body = Vec::new();
                while let Some(part) = response.chunk().await.map_err(async_transport_error)? {
                    if body.len() + part.len() > max_bytes {
                        return Err(too_large(format!("more than {max_bytes}")));
                    }
                    body.extend_from_slice(&part);
                }
                Ok(body)
            }
            #[cfg(target_arch = "wasm32")]
            {
                let body = response.bytes().await.map_err(async_transport_error)?;
                if body.len() > max_bytes {
                    return Err(too_large(body.len().to_string()));
                }
                Ok(body.to_vec())
            }
        })
    }
}

#[cfg(feature = "async-client")]
fn async_transport_error(err: reqwest::Error) -> TransportError {
    use super::iterator::TransportErrorKind;

    // reqwest has no connect classification on wasm32.
    #[cfg(not(target_arch = "wasm32"))]
    let connect = err.is_connect();
    #[cfg(target_arch = "wasm32")]
    let connect = false;
    let kind = if err.is_timeout() {
        TransportErrorKind::Timeout
    } else if connect || err.is_request() {
        TransportErrorKind::Connect
    } else if let Some(status) = err.status() {
        TransportErrorKind::Status(status.as_u16())
    } else if err.is_body() || err.is_decode() {
        TransportErrorKind::Body
    } else {
        TransportErrorKind::Other
    };
    let mut message = err.to_string();
    let mut source = std::error::Error::source(&err);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    TransportError::new(kind, message)
}
