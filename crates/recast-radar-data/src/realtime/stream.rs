//! Async [`Stream`] over real-time chunks (feature `async`).
//!
//! [`ChunkStream`] drives the same [`ChunkPlanner`] as the blocking
//! [`ChunkIterator`](super::iterator::ChunkIterator) with an
//! [`AsyncChunkTransport`], so both produce identical event sequences for
//! identical responses. The stream depends on no runtime: it never sleeps or
//! spawns. On [`ChunkEvent::Idle`] and [`ChunkEvent::Retry`] the caller waits
//! with its own runtime's timer before polling the stream again.

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
