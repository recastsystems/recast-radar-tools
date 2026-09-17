//! Pull-based iterator over NEXRAD Level II real-time chunks.
//!
//! The `unidata-nexrad-level2-chunks` bucket stores each volume as
//! `SITE/VOLUME_ID/YYYYMMDD-HHMMSS-CCC-T`: the volume id cycles 1..=999
//! (999 is followed by 1), `YYYYMMDD-HHMMSS` is the volume start time, `CCC`
//! the 1-based chunk number and `T` the chunk type (`S` start, `I`
//! intermediate, `E` end). Old volumes are purged from the oldest end; a
//! volume that the radar restarts (a VCP change, an outage) never gets its
//! End chunk.
//!
//! The work splits in two layers:
//!
//! - [`ChunkPlanner`] is a sans-I/O state machine. It says which GET to
//!   perform next ([`PlannerStep::Fetch`]) and turns responses
//!   ([`ChunkPlanner::complete`]) into [`ChunkEvent`]s. It never touches the
//!   network, never sleeps and never reads a clock, so it builds without the
//!   `net` feature (and for `wasm32-unknown-unknown`), and the blocking
//!   iterator and the async stream share it.
//! - [`ChunkIterator`] drives a planner with a blocking [`ChunkTransport`].
//!   With `net`, `ReqwestTransport` is the HTTPS client; tests replay
//!   recorded S3 responses through their own transport.
//!
//! # Request plan
//!
//! 1. Join. [`JoinMode::CurrentVolume`] and [`JoinMode::NextVolume`] list the
//!    site's volume-id prefixes (`?list-type=2&prefix=SITE/&delimiter=/`) and
//!    pick the newest id: the id just before the largest gap on the
//!    1..=999 ring, since purging leaves one large gap behind the newest
//!    volume. [`JoinMode::Volume`] starts at a given id without listing.
//! 2. Poll the volume: `?list-type=2&prefix=SITE/ID/`, and once a chunk has
//!    been taken, `&start-after=<last taken key>` so each poll returns only
//!    new keys. Chunks are taken strictly in order from chunk 1 (which must
//!    be a Start chunk); a missing chunk holds back everything after it until
//!    it is listed. Once a poll's chunks are handed out, the planner emits
//!    [`ChunkEvent::Idle`] before polling a volume in progress again (a
//!    truncated listing page is followed up at once).
//! 3. Download each taken chunk (`GET /SITE/ID/<name>`, unless
//!    [`ChunkIteratorConfig::download`] is off) and check its length against
//!    the listed size.
//! 4. After the End chunk, list the next id (999 -> 1) at once. Chunks under
//!    that id that are not newer than the finished volume (leftovers of the
//!    id's previous cycle, or a bogus volume time) are ignored.
//!    Abandoning a volume after a failed download (below) continues the same
//!    way.
//! 5. When [`ChunkIteratorConfig::stall_polls`] polls in a row bring nothing,
//!    probe the next [`ChunkIteratorConfig::probe_ahead`] ids for a newer
//!    volume; every [`ChunkIteratorConfig::rediscover_every`] failed probe
//!    rounds, list the site's ids again and probe the newest. A newer volume
//!    found this way abandons the current one
//!    ([`ChunkEvent::VolumeAbandoned`]).
//!
//! Failed requests are retried under [`ChunkIteratorConfig::retry`]: the
//! planner emits [`ChunkEvent::Retry`] with the delay and repeats the request
//! on the next step. When the budget is spent (or the failure is not
//! retryable) it emits the error followed by [`ChunkEvent::Idle`]. A failed
//! listing then starts over with a fresh budget. A failed chunk download is
//! not simply repeated: the planner goes back to the failed chunk and lists
//! the volume again from there, which refreshes a stale listing and shows
//! whether the chunk still exists. The volume is abandoned, and the planner
//! continues with the next volume id as after an End chunk, when:
//!
//! - the relisting no longer shows the failed chunk (purged or deleted),
//! - the chunk's body exceeded [`ChunkIteratorConfig::max_chunk_bytes`]
//!   (repeating cannot help), or
//! - downloads of the same chunk have failed
//!   [`ChunkIteratorConfig::max_chunk_failures`] times in a row.
//!
//! If some of its chunks were delivered, abandoning emits
//! [`ChunkEvent::VolumeAbandoned`]. A chunk that cannot be downloaded
//! therefore never holds the iterator on one volume.

use std::collections::VecDeque;
use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Utc};

use super::retry::{Backoff, RetryPolicy, random_seed};
use crate::{RealtimeChunkObject, RealtimeChunkType, S3Listing, S3ListingXml, S3Object};

/// Base URL of the public real-time chunk bucket.
pub const CHUNKS_BUCKET_URL: &str = "https://unidata-nexrad-level2-chunks.s3.amazonaws.com";

/// Largest real-time volume id; the id after it is 1 (there is no id 0).
pub const MAX_VOLUME_ID: u16 = 999;

/// Id-listing pages read per join or rediscovery (1000 prefixes per page
/// already covers the whole 1..=999 id ring).
const MAX_ID_LISTING_PAGES: u32 = 4;

/// The volume id that follows `volume_id` (999 -> 1).
pub fn next_volume_id(volume_id: u16) -> u16 {
    if volume_id >= MAX_VOLUME_ID {
        1
    } else {
        volume_id + 1
    }
}

/// What a request fetches.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FetchKind {
    /// A site's volume-id prefixes (`prefix=SITE/&delimiter=/`).
    VolumeIds,
    /// A volume's chunk keys (`prefix=SITE/ID/`).
    ChunkListing,
    /// One chunk object.
    Chunk,
}

/// A GET the planner needs performed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchRequest {
    /// Absolute URL, already percent-encoded.
    pub url: String,
    /// What the response is.
    pub kind: FetchKind,
    /// Largest acceptable body. Transports should stop reading beyond it and
    /// fail with [`TransportErrorKind::TooLarge`]; the planner rejects
    /// larger bodies either way.
    pub max_bytes: usize,
}

/// Why a fetch failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportErrorKind {
    /// The server answered with this non-success HTTP status.
    Status(u16),
    /// The request or the body read timed out.
    Timeout,
    /// Connecting or sending failed before a response arrived.
    Connect,
    /// The body was cut short or unreadable, including a chunk whose length
    /// differs from its listed size.
    Body,
    /// The body exceeded [`FetchRequest::max_bytes`].
    TooLarge,
    /// Anything else (a malformed URL, a redirect loop, ...).
    Other,
}

/// A failed fetch, as reported by a [`ChunkTransport`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransportError {
    /// Classification that drives retrying.
    pub kind: TransportErrorKind,
    /// Human-readable detail.
    pub message: String,
}

impl TransportError {
    /// A transport error of `kind`.
    pub fn new(kind: TransportErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Whether repeating the request can help: timeouts, connection and body
    /// failures, and HTTP 408, 429 and 5xx. Other statuses (403, 404, ...),
    /// oversized bodies and [`TransportErrorKind::Other`] are final.
    pub fn is_retryable(&self) -> bool {
        match self.kind {
            TransportErrorKind::Status(code) => {
                code == 408 || code == 429 || (500..=599).contains(&code)
            }
            TransportErrorKind::Timeout
            | TransportErrorKind::Connect
            | TransportErrorKind::Body => true,
            TransportErrorKind::TooLarge | TransportErrorKind::Other => false,
        }
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            TransportErrorKind::Status(code) => write!(f, "HTTP {code}: {}", self.message),
            kind => write!(f, "{kind:?}: {}", self.message),
        }
    }
}

impl std::error::Error for TransportError {}

/// A blocking HTTP GET.
///
/// The iterator's only access to the network. Implementations return the
/// full body of a 2xx response and map everything else to a
/// [`TransportError`].
pub trait ChunkTransport {
    /// Perform `request`.
    fn fetch(&mut self, request: &FetchRequest) -> Result<Vec<u8>, TransportError>;
}

impl<T: ChunkTransport + ?Sized> ChunkTransport for &mut T {
    fn fetch(&mut self, request: &FetchRequest) -> Result<Vec<u8>, TransportError> {
        (**self).fetch(request)
    }
}

impl<T: ChunkTransport + ?Sized> ChunkTransport for Box<T> {
    fn fetch(&mut self, request: &FetchRequest) -> Result<Vec<u8>, TransportError> {
        (**self).fetch(request)
    }
}

/// Where a new iterator starts.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum JoinMode {
    /// The newest volume from its Start chunk, whether it is still being
    /// collected (joining mid-volume) or already complete; then live.
    #[default]
    CurrentVolume,
    /// Skip the newest volume and start at the Start chunk of the one after
    /// it.
    NextVolume,
    /// Start at the Start chunk of this volume id and walk forward from
    /// there, through the bucket's retained history up to live.
    Volume(u16),
}

/// Settings for a [`ChunkPlanner`] / [`ChunkIterator`].
#[derive(Clone, Debug, PartialEq)]
pub struct ChunkIteratorConfig {
    /// Bucket base URL, without a trailing slash. Defaults to
    /// [`CHUNKS_BUCKET_URL`].
    pub bucket_url: String,
    /// Where to start.
    pub join: JoinMode,
    /// Download chunk bytes. When off, the iterator only lists and yields
    /// chunk metadata ([`Chunk::data`] is `None`), which is enough for
    /// availability and timing monitoring.
    pub download: bool,
    /// Suggested wait after a poll that brought nothing new (the
    /// [`ChunkEvent::Idle`] delay).
    pub poll_interval: Duration,
    /// Empty polls in a row before probing for a newer volume. `0` is
    /// treated as `1`.
    pub stall_polls: u32,
    /// How many following volume ids a probe round lists.
    pub probe_ahead: u16,
    /// Failed probe rounds between site-wide id listings. `0` disables
    /// rediscovery.
    pub rediscover_every: u32,
    /// Retry policy for failed requests.
    pub retry: RetryPolicy,
    /// Seed for retry jitter; `None` draws one from
    /// [`super::retry::random_seed`].
    pub jitter_seed: Option<u64>,
    /// Largest accepted listing body.
    pub max_listing_bytes: usize,
    /// Largest accepted chunk body. A chunk whose body exceeds it can never
    /// be taken, so its volume is abandoned.
    pub max_chunk_bytes: usize,
    /// Failed download rounds for one chunk before its volume is abandoned.
    /// A round ends when a download of the chunk fails for good (retry
    /// budget spent, or a failure that is not retryable) and is followed by
    /// a relisting that still shows the chunk. `0` is treated as `1`.
    pub max_chunk_failures: u32,
}

impl Default for ChunkIteratorConfig {
    fn default() -> Self {
        Self {
            bucket_url: CHUNKS_BUCKET_URL.to_owned(),
            join: JoinMode::CurrentVolume,
            download: true,
            poll_interval: Duration::from_secs(5),
            stall_polls: 12,
            probe_ahead: 2,
            rediscover_every: 5,
            retry: RetryPolicy::default(),
            jitter_seed: None,
            max_listing_bytes: 8 * 1024 * 1024,
            max_chunk_bytes: 32 * 1024 * 1024,
            max_chunk_failures: 3,
        }
    }
}

/// One chunk, in volume order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Chunk {
    /// Key, size, S3 `LastModified`, site, volume id and time, chunk number
    /// and type.
    pub info: RealtimeChunkObject,
    /// The chunk bytes (length checked against the listed size), or `None`
    /// when [`ChunkIteratorConfig::download`] is off.
    pub data: Option<Vec<u8>>,
}

/// What the iterator produces.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ChunkEvent {
    /// The next chunk of the current volume.
    Chunk(Chunk),
    /// Caught up with the bucket (or pausing after an error); poll again
    /// after `poll_after`.
    Idle {
        /// Suggested wait.
        poll_after: Duration,
    },
    /// A request failed and will be repeated on the next step, after
    /// `after`.
    Retry {
        /// Suggested wait (from the retry policy).
        after: Duration,
        /// The attempt number the repeat will be (2 for the first retry).
        attempt: u32,
        /// What was being fetched.
        kind: FetchKind,
        /// The failed request's URL.
        url: String,
        /// The failure.
        error: TransportError,
    },
    /// The iterator left a volume before its End chunk after delivering some
    /// of its chunks: a newer volume appeared, or one of its chunks could
    /// not be downloaded (the download error comes first; see the module
    /// documentation).
    VolumeAbandoned {
        /// Site.
        site: String,
        /// The abandoned volume's id.
        volume_id: u16,
        /// The abandoned volume's start time.
        volume_time: DateTime<Utc>,
        /// Last chunk number delivered from the abandoned volume.
        last_chunk_id: u16,
        /// The volume id the iterator continues with.
        next_volume_id: u16,
    },
}

/// A failure the iterator could not retry away.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ChunkIterError {
    /// A request failed and the retry policy gave up (or the failure is not
    /// retryable).
    #[error("{kind:?} request {url} failed after {attempts} attempt(s): {error}")]
    Transport {
        /// What was being fetched.
        kind: FetchKind,
        /// The request URL.
        url: String,
        /// Attempts made.
        attempts: u32,
        /// The last failure.
        error: TransportError,
    },
    /// A listing response was not a readable `ListObjectsV2` result.
    #[error("unreadable S3 listing from {url}: {message}")]
    Listing {
        /// The request URL.
        url: String,
        /// Parser detail.
        message: String,
    },
    /// The site has no real-time volume prefixes.
    #[error("no real-time volumes listed for site {site}")]
    NoVolumes {
        /// Site.
        site: String,
    },
}

/// Request and byte counters.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ChunkIteratorStats {
    /// Completed request attempts, successful or not.
    pub requests: u64,
    /// Of `requests`: volume-id and chunk listings.
    pub listing_requests: u64,
    /// Of `requests`: chunk downloads.
    pub chunk_requests: u64,
    /// Of `requests`: failures (transport errors, rejected bodies).
    pub failed_requests: u64,
    /// Retries scheduled ([`ChunkEvent::Retry`] events).
    pub retries: u64,
    /// Listing body bytes received.
    pub listing_bytes: u64,
    /// Chunk body bytes received.
    pub chunk_bytes: u64,
    /// Chunks yielded.
    pub chunks: u64,
    /// End chunks yielded.
    pub volumes_completed: u64,
    /// Volumes abandoned before their End chunk.
    pub volumes_abandoned: u64,
}

/// The volume a planner is following.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VolumePosition {
    /// Volume id.
    pub volume_id: u16,
    /// Volume start time, once a chunk of it has been listed.
    pub volume_time: Option<DateTime<Utc>>,
    /// Number of the next chunk to take from listings.
    pub next_chunk_id: u16,
    /// True while skipping the volume that was current at a
    /// [`JoinMode::NextVolume`] join.
    pub skipping: bool,
}

/// The next thing a driver must do for a [`ChunkPlanner`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlannerStep {
    /// Perform this GET and pass the result to [`ChunkPlanner::complete`].
    Fetch(FetchRequest),
    /// Hand this to the caller.
    Event(Result<ChunkEvent, ChunkIterError>),
}

#[derive(Clone, Debug)]
struct Cursor {
    volume_id: u16,
    volume_time: Option<DateTime<Utc>>,
    /// Only volumes that started after this time qualify for this id.
    after_time: Option<DateTime<Utc>>,
    next_chunk_id: u16,
    last_key: Option<String>,
    /// Number and key of the last chunk handed to the caller.
    delivered: Option<(u16, String)>,
    deliver: bool,
    empty_polls: u32,
    failed_probes: u32,
    /// Failed download rounds of the chunk at `next_chunk_id`; reset when a
    /// chunk is delivered. While nonzero, a poll that does not advance means
    /// the failed chunk is gone.
    chunk_failures: u32,
}

impl Cursor {
    fn new(volume_id: u16, after_time: Option<DateTime<Utc>>, deliver: bool) -> Self {
        Self {
            volume_id,
            volume_time: None,
            after_time,
            next_chunk_id: 1,
            last_key: None,
            delivered: None,
            deliver,
            empty_polls: 0,
            failed_probes: 0,
            chunk_failures: 0,
        }
    }

    /// Chunks of a candidate volume must be newer than this.
    fn reference_time(&self) -> Option<DateTime<Utc>> {
        self.volume_time.or(self.after_time)
    }
}

/// What [`ChunkPlanner::integrate`] took from a listing.
#[derive(Clone, Copy, Debug, Default)]
struct Taken {
    /// At least one chunk continued the volume.
    advanced: bool,
    /// The End chunk was among them.
    ended: bool,
}

#[derive(Clone, Debug)]
enum Phase {
    /// List the site's volume ids. `rediscover` is false for the join.
    Ids {
        rediscover: bool,
        ids: Vec<u16>,
        token: Option<String>,
        pages: u32,
    },
    /// Poll the cursor's volume.
    Poll,
    /// List candidate volume ids for a newer volume.
    Probe {
        candidates: VecDeque<u16>,
        rediscovered: bool,
    },
}

/// Sans-I/O planner behind [`ChunkIterator`] and the async `ChunkStream`.
///
/// Drive it by calling [`ChunkPlanner::next_step`]: perform each
/// [`PlannerStep::Fetch`] and report the outcome with
/// [`ChunkPlanner::complete`]; pass each [`PlannerStep::Event`] to the
/// caller. The event sequence never ends. Calling `next_step` again before
/// `complete` returns the same request.
#[derive(Clone, Debug)]
pub struct ChunkPlanner {
    site: String,
    config: ChunkIteratorConfig,
    phase: Phase,
    cursor: Option<Cursor>,
    ready: VecDeque<RealtimeChunkObject>,
    events: VecDeque<Result<ChunkEvent, ChunkIterError>>,
    pending: Option<FetchRequest>,
    /// The last poll caught up with a volume still being collected: pause
    /// before polling it again.
    caught_up: bool,
    backoff: Backoff,
    stats: ChunkIteratorStats,
}

impl ChunkPlanner {
    /// A planner for `site` (upper-cased, e.g. `KTLX`).
    pub fn new(site: &str, config: ChunkIteratorConfig) -> Self {
        let site = site.trim().to_ascii_uppercase();
        let seed = config.jitter_seed.unwrap_or_else(random_seed);
        let backoff = config.retry.backoff(seed);
        let (phase, cursor) = match config.join {
            JoinMode::Volume(volume_id) => (Phase::Poll, Some(Cursor::new(volume_id, None, true))),
            JoinMode::CurrentVolume | JoinMode::NextVolume => (Self::ids_phase(false), None),
        };
        Self {
            site,
            config,
            phase,
            cursor,
            ready: VecDeque::new(),
            events: VecDeque::new(),
            pending: None,
            caught_up: false,
            backoff,
            stats: ChunkIteratorStats::default(),
        }
    }

    /// The site, upper-cased.
    pub fn site(&self) -> &str {
        &self.site
    }

    /// The configuration.
    pub fn config(&self) -> &ChunkIteratorConfig {
        &self.config
    }

    /// Request and byte counters so far.
    pub fn stats(&self) -> ChunkIteratorStats {
        self.stats
    }

    /// The volume being followed, once the join has picked one.
    pub fn position(&self) -> Option<VolumePosition> {
        self.cursor.as_ref().map(|cursor| VolumePosition {
            volume_id: cursor.volume_id,
            volume_time: cursor.volume_time,
            next_chunk_id: cursor.next_chunk_id,
            skipping: !cursor.deliver,
        })
    }

    /// The next request to perform or event to hand out.
    pub fn next_step(&mut self) -> PlannerStep {
        if let Some(event) = self.events.pop_front() {
            return PlannerStep::Event(event);
        }
        if let Some(request) = &self.pending {
            return PlannerStep::Fetch(request.clone());
        }
        let request = if let Some(chunk) = self.ready.front() {
            if !self.config.download {
                let event = self.take_ready(None);
                return PlannerStep::Event(Ok(event));
            }
            FetchRequest {
                url: self.object_url(&chunk.object.key),
                kind: FetchKind::Chunk,
                max_bytes: self.config.max_chunk_bytes,
            }
        } else if self.caught_up && matches!(self.phase, Phase::Poll) {
            self.caught_up = false;
            return PlannerStep::Event(Ok(ChunkEvent::Idle {
                poll_after: self.config.poll_interval,
            }));
        } else {
            self.phase_request()
        };
        self.backoff.reset();
        self.pending = Some(request.clone());
        PlannerStep::Fetch(request)
    }

    /// Report the outcome of the request from the last
    /// [`PlannerStep::Fetch`]. Ignored when no request is outstanding.
    pub fn complete(&mut self, result: Result<Vec<u8>, TransportError>) {
        let Some(request) = self.pending.take() else {
            return;
        };
        self.stats.requests += 1;
        let is_chunk = request.kind == FetchKind::Chunk;
        if is_chunk {
            self.stats.chunk_requests += 1;
        } else {
            self.stats.listing_requests += 1;
        }
        let body = match result {
            Ok(body) => body,
            Err(error) => return self.fail(request, error),
        };
        if is_chunk {
            self.stats.chunk_bytes += body.len() as u64;
        } else {
            self.stats.listing_bytes += body.len() as u64;
        }
        if let Err(error) = self.check_body(&request, &body) {
            return self.fail(request, error);
        }
        if is_chunk {
            let event = self.take_ready(Some(body));
            self.events.push_back(Ok(event));
            return;
        }
        match parse_listing(&body) {
            Ok(listing) => self.handle_listing(listing),
            Err(message) => {
                self.stats.failed_requests += 1;
                self.events.push_back(Err(ChunkIterError::Listing {
                    url: request.url,
                    message,
                }));
                self.push_idle();
            }
        }
    }

    /// Reject oversized bodies and chunks whose length differs from the
    /// listing.
    fn check_body(&self, request: &FetchRequest, body: &[u8]) -> Result<(), TransportError> {
        if body.len() > request.max_bytes {
            return Err(TransportError::new(
                TransportErrorKind::TooLarge,
                format!(
                    "body of {} bytes exceeds the {}-byte limit",
                    body.len(),
                    request.max_bytes
                ),
            ));
        }
        if request.kind == FetchKind::Chunk
            && let Some(chunk) = self.ready.front()
            && chunk.object.size != body.len() as u64
        {
            return Err(TransportError::new(
                TransportErrorKind::Body,
                format!(
                    "{} is listed at {} bytes, received {}",
                    chunk.object.key,
                    chunk.object.size,
                    body.len()
                ),
            ));
        }
        Ok(())
    }

    fn fail(&mut self, request: FetchRequest, error: TransportError) {
        self.stats.failed_requests += 1;
        let delay = self.backoff.next_delay();
        let attempts = self.backoff.failures();
        match delay {
            Some(after) if error.is_retryable() => {
                self.stats.retries += 1;
                self.events.push_back(Ok(ChunkEvent::Retry {
                    after,
                    attempt: attempts + 1,
                    kind: request.kind,
                    url: request.url.clone(),
                    error,
                }));
                self.pending = Some(request);
            }
            _ => {
                let kind = request.kind;
                let too_large = error.kind == TransportErrorKind::TooLarge;
                self.events.push_back(Err(ChunkIterError::Transport {
                    kind,
                    url: request.url,
                    attempts,
                    error,
                }));
                if kind == FetchKind::Chunk {
                    self.chunk_download_failed(too_large);
                }
                self.push_idle();
            }
        }
    }

    /// A chunk download failed for good. Go back to the failed chunk so the
    /// next poll lists the volume again from there, or abandon the volume
    /// when the chunk can never be taken.
    fn chunk_download_failed(&mut self, too_large: bool) {
        let Some(failed_chunk_id) = self.ready.front().map(|chunk| chunk.chunk_id) else {
            return;
        };
        let max_failures = self.config.max_chunk_failures.max(1);
        let Some(cursor) = self.cursor.as_mut() else {
            return;
        };
        cursor.next_chunk_id = failed_chunk_id;
        cursor.last_key = cursor.delivered.as_ref().map(|(_, key)| key.clone());
        cursor.chunk_failures = cursor.chunk_failures.saturating_add(1);
        let give_up = too_large || cursor.chunk_failures >= max_failures;
        self.ready.clear();
        self.caught_up = false;
        self.phase = Phase::Poll;
        if give_up {
            self.abandon_volume();
        }
    }

    /// Give up on the cursor's volume and continue with the next volume id,
    /// taking only volumes newer than the abandoned one.
    fn abandon_volume(&mut self) {
        let Some(old) = self.cursor.take() else {
            return;
        };
        let next_id = next_volume_id(old.volume_id);
        if old.deliver
            && let (Some(volume_time), Some((last_chunk_id, _))) = (old.volume_time, &old.delivered)
        {
            self.stats.volumes_abandoned += 1;
            self.events.push_back(Ok(ChunkEvent::VolumeAbandoned {
                site: self.site.clone(),
                volume_id: old.volume_id,
                volume_time,
                last_chunk_id: *last_chunk_id,
                next_volume_id: next_id,
            }));
        }
        self.ready.clear();
        self.caught_up = false;
        self.cursor = Some(Cursor::new(next_id, old.reference_time(), true));
        self.phase = Phase::Poll;
    }

    fn push_idle(&mut self) {
        self.events.push_back(Ok(ChunkEvent::Idle {
            poll_after: self.config.poll_interval,
        }));
    }

    fn ids_phase(rediscover: bool) -> Phase {
        Phase::Ids {
            rediscover,
            ids: Vec::new(),
            token: None,
            pages: 0,
        }
    }

    fn phase_request(&self) -> FetchRequest {
        match (&self.phase, &self.cursor) {
            (Phase::Ids { token, .. }, _) => FetchRequest {
                url: self.listing_url(&format!("{}/", self.site), true, None, token.as_deref()),
                kind: FetchKind::VolumeIds,
                max_bytes: self.config.max_listing_bytes,
            },
            (Phase::Probe { candidates, .. }, _) if !candidates.is_empty() => {
                let volume_id = candidates.front().copied().unwrap_or(1);
                self.chunk_listing_request(volume_id, None)
            }
            (_, Some(cursor)) => {
                self.chunk_listing_request(cursor.volume_id, cursor.last_key.as_deref())
            }
            // Unreachable by construction (polling always has a cursor, probe
            // rounds are never empty); relisting the site ids is a safe way
            // back.
            (_, None) => FetchRequest {
                url: self.listing_url(&format!("{}/", self.site), true, None, None),
                kind: FetchKind::VolumeIds,
                max_bytes: self.config.max_listing_bytes,
            },
        }
    }

    fn chunk_listing_request(&self, volume_id: u16, start_after: Option<&str>) -> FetchRequest {
        FetchRequest {
            url: self.listing_url(
                &format!("{}/{volume_id}/", self.site),
                false,
                start_after,
                None,
            ),
            kind: FetchKind::ChunkListing,
            max_bytes: self.config.max_listing_bytes,
        }
    }

    fn listing_url(
        &self,
        prefix: &str,
        delimiter: bool,
        start_after: Option<&str>,
        token: Option<&str>,
    ) -> String {
        let mut url = format!(
            "{}/?list-type=2&prefix={}",
            self.config.bucket_url.trim_end_matches('/'),
            percent_encode(prefix, false)
        );
        if delimiter {
            url.push_str("&delimiter=%2F");
        }
        if let Some(start_after) = start_after {
            url.push_str("&start-after=");
            url.push_str(&percent_encode(start_after, false));
        }
        if let Some(token) = token {
            url.push_str("&continuation-token=");
            url.push_str(&percent_encode(token, false));
        }
        url
    }

    fn object_url(&self, key: &str) -> String {
        format!(
            "{}/{}",
            self.config.bucket_url.trim_end_matches('/'),
            percent_encode(key, true)
        )
    }

    fn handle_listing(&mut self, listing: S3Listing) {
        let phase = std::mem::replace(&mut self.phase, Phase::Poll);
        match phase {
            Phase::Ids {
                rediscover,
                mut ids,
                pages,
                ..
            } => {
                ids.extend(listing.common_prefixes.iter().filter_map(|prefix| {
                    crate::realtime_volume_id_from_prefix(&self.site, &prefix.prefix)
                }));
                let pages = pages + 1;
                if let Some(token) = listing.next_continuation_token
                    && pages < MAX_ID_LISTING_PAGES
                {
                    self.phase = Phase::Ids {
                        rediscover,
                        ids,
                        token: Some(token),
                        pages,
                    };
                    return;
                }
                self.handle_ids(rediscover, &ids);
            }
            Phase::Poll => {
                let truncated = listing.next_continuation_token.is_some();
                self.handle_poll(listing.contents, truncated);
            }
            Phase::Probe {
                mut candidates,
                rediscovered,
            } => {
                let Some(volume_id) = candidates.pop_front() else {
                    return;
                };
                self.handle_probe(volume_id, listing.contents, candidates, rediscovered);
            }
        }
    }

    fn handle_ids(&mut self, rediscover: bool, ids: &[u16]) {
        let Some(latest) = crate::latest_realtime_volume_id_from_active_ids(ids) else {
            self.events.push_back(Err(ChunkIterError::NoVolumes {
                site: self.site.clone(),
            }));
            self.push_idle();
            self.phase = if rediscover {
                Phase::Poll
            } else {
                Self::ids_phase(false)
            };
            return;
        };
        if !rediscover {
            let deliver = self.config.join != JoinMode::NextVolume;
            self.cursor = Some(Cursor::new(latest, None, deliver));
            self.phase = Phase::Poll;
            return;
        }
        let Some(cursor) = &self.cursor else {
            self.cursor = Some(Cursor::new(latest, None, true));
            self.phase = Phase::Poll;
            return;
        };
        let already_checked =
            latest == cursor.volume_id || self.successors(cursor.volume_id).contains(&latest);
        if already_checked {
            self.phase = Phase::Poll;
            self.push_idle();
        } else {
            self.phase = Phase::Probe {
                candidates: VecDeque::from([latest]),
                rediscovered: true,
            };
        }
    }

    fn handle_poll(&mut self, objects: Vec<S3Object>, truncated: bool) {
        self.phase = Phase::Poll;
        let taken = self.integrate(objects);
        if taken.advanced {
            if let Some(cursor) = &mut self.cursor {
                cursor.empty_polls = 0;
                cursor.failed_probes = 0;
            }
            // More keys may be waiting behind a truncated page; after an End
            // chunk the next volume id is listed right away.
            self.caught_up = !truncated && !taken.ended;
            return;
        }
        let stall_polls = self.config.stall_polls.max(1);
        let Some(cursor) = &mut self.cursor else {
            self.push_idle();
            return;
        };
        if cursor.chunk_failures > 0 {
            // The relisting after a failed download no longer shows the
            // failed chunk: it was purged or deleted, so the volume cannot be
            // completed in order.
            self.abandon_volume();
            return;
        }
        cursor.empty_polls += 1;
        if cursor.empty_polls < stall_polls {
            self.push_idle();
            return;
        }
        cursor.empty_polls = 0;
        let volume_id = cursor.volume_id;
        let candidates: VecDeque<u16> = self.successors(volume_id).into();
        if candidates.is_empty() {
            self.after_failed_probe_round(false);
        } else {
            self.phase = Phase::Probe {
                candidates,
                rediscovered: false,
            };
        }
    }

    fn handle_probe(
        &mut self,
        volume_id: u16,
        objects: Vec<S3Object>,
        remaining: VecDeque<u16>,
        rediscovered: bool,
    ) {
        let reference = self.cursor.as_ref().and_then(Cursor::reference_time);
        let newer = objects
            .iter()
            .filter(|object| object.size > 0)
            .filter_map(|object| crate::parse_realtime_chunk_object(object.clone()))
            .any(|chunk| {
                chunk.site == self.site
                    && chunk.volume_id == volume_id
                    && reference.is_none_or(|reference| chunk.volume_time > reference)
            });
        if newer {
            self.switch_volume(volume_id, reference, objects);
            return;
        }
        if !remaining.is_empty() {
            self.phase = Phase::Probe {
                candidates: remaining,
                rediscovered,
            };
            return;
        }
        self.after_failed_probe_round(rediscovered);
    }

    fn after_failed_probe_round(&mut self, rediscovered: bool) {
        self.phase = Phase::Poll;
        let rediscover_every = self.config.rediscover_every;
        if let Some(cursor) = &mut self.cursor
            && !rediscovered
        {
            cursor.failed_probes += 1;
            if rediscover_every > 0 && cursor.failed_probes % rediscover_every == 0 {
                self.phase = Self::ids_phase(true);
                return;
            }
        }
        self.push_idle();
    }

    fn successors(&self, volume_id: u16) -> Vec<u16> {
        let mut ids = Vec::with_capacity(usize::from(self.config.probe_ahead));
        let mut id = volume_id;
        for _ in 0..self.config.probe_ahead {
            id = next_volume_id(id);
            if id == volume_id || ids.contains(&id) {
                break;
            }
            ids.push(id);
        }
        ids
    }

    fn switch_volume(
        &mut self,
        volume_id: u16,
        reference: Option<DateTime<Utc>>,
        objects: Vec<S3Object>,
    ) {
        if let Some(old) = self.cursor.take()
            && old.deliver
            && old.next_chunk_id > 1
            && let Some(volume_time) = old.volume_time
        {
            self.stats.volumes_abandoned += 1;
            self.events.push_back(Ok(ChunkEvent::VolumeAbandoned {
                site: self.site.clone(),
                volume_id: old.volume_id,
                volume_time,
                last_chunk_id: old.next_chunk_id - 1,
                next_volume_id: volume_id,
            }));
        }
        self.ready.clear();
        self.cursor = Some(Cursor::new(volume_id, reference, true));
        self.phase = Phase::Poll;
        let taken = self.integrate(objects);
        self.caught_up = taken.advanced && !taken.ended;
    }

    /// Take the listed chunks that continue the cursor's volume in order.
    fn integrate(&mut self, objects: Vec<S3Object>) -> Taken {
        let Some(cursor) = self.cursor.as_mut() else {
            return Taken::default();
        };
        let mut chunks: Vec<RealtimeChunkObject> = objects
            .into_iter()
            .filter(|object| object.size > 0)
            .filter_map(crate::parse_realtime_chunk_object)
            .filter(|chunk| chunk.site == self.site && chunk.volume_id == cursor.volume_id)
            .collect();
        if cursor.volume_time.is_none() {
            let after = cursor.after_time;
            let newest = chunks
                .iter()
                .map(|chunk| chunk.volume_time)
                .filter(|time| after.is_none_or(|after| *time > after))
                .max();
            match newest {
                Some(time) => cursor.volume_time = Some(time),
                None => return Taken::default(),
            }
        }
        let volume_time = cursor.volume_time;
        chunks.retain(|chunk| Some(chunk.volume_time) == volume_time);
        chunks.sort_by(|left, right| {
            left.chunk_id
                .cmp(&right.chunk_id)
                .then_with(|| left.object.key.cmp(&right.object.key))
        });

        let mut advanced = false;
        let mut ended = false;
        for chunk in chunks {
            if chunk.chunk_id < cursor.next_chunk_id {
                continue;
            }
            let in_order = chunk.chunk_id == cursor.next_chunk_id
                && (chunk.chunk_type == RealtimeChunkType::Start) == (chunk.chunk_id == 1);
            let Some(next_chunk_id) = cursor.next_chunk_id.checked_add(1) else {
                break;
            };
            if !in_order {
                break;
            }
            cursor.next_chunk_id = next_chunk_id;
            cursor.last_key = Some(chunk.object.key.clone());
            advanced = true;
            ended = chunk.chunk_type == RealtimeChunkType::End;
            if cursor.deliver {
                self.ready.push_back(chunk);
            }
            if ended {
                break;
            }
        }
        if ended && !cursor.deliver {
            self.roll_over();
        }
        Taken { advanced, ended }
    }

    /// Hand out the front ready chunk; roll over after an End chunk.
    fn take_ready(&mut self, data: Option<Vec<u8>>) -> ChunkEvent {
        let Some(info) = self.ready.pop_front() else {
            // Only reachable if a caller completes a chunk request twice;
            // report it as an empty poll.
            return ChunkEvent::Idle {
                poll_after: self.config.poll_interval,
            };
        };
        self.stats.chunks += 1;
        if let Some(cursor) = self.cursor.as_mut() {
            cursor.delivered = Some((info.chunk_id, info.object.key.clone()));
            cursor.chunk_failures = 0;
        }
        if info.chunk_type == RealtimeChunkType::End && self.ready.is_empty() {
            self.stats.volumes_completed += 1;
            self.roll_over();
        }
        ChunkEvent::Chunk(Chunk { info, data })
    }

    fn roll_over(&mut self) {
        let Some(old) = self.cursor.take() else {
            return;
        };
        self.cursor = Some(Cursor::new(
            next_volume_id(old.volume_id),
            old.reference_time(),
            true,
        ));
        self.phase = Phase::Poll;
    }
}

fn parse_listing(body: &[u8]) -> Result<S3Listing, String> {
    let text = std::str::from_utf8(body).map_err(|err| format!("not UTF-8: {err}"))?;
    if !text.contains("<ListBucketResult") {
        return Err("no ListBucketResult element".to_owned());
    }
    let parsed: S3ListingXml = quick_xml::de::from_str(text).map_err(|err| err.to_string())?;
    Ok(parsed.into())
}

/// Percent-encode everything but RFC 3986 unreserved characters (and `/`
/// when `keep_slash`).
fn percent_encode(value: &str, keep_slash: bool) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        let unreserved = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~');
        if unreserved || (keep_slash && byte == b'/') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// A pull-based iterator over real-time chunks with a blocking transport.
///
/// Each `next()` performs the requests needed for one [`ChunkEvent`]. The
/// iterator never sleeps and never ends: on [`ChunkEvent::Idle`] and
/// [`ChunkEvent::Retry`] the caller waits the suggested delay (or not)
/// before calling `next()` again. [`ChunkIterator::chunks`] wraps that loop
/// for callers that only want chunks.
#[derive(Debug)]
pub struct ChunkIterator<T> {
    planner: ChunkPlanner,
    transport: T,
}

impl<T: ChunkTransport> ChunkIterator<T> {
    /// An iterator for `site` over `transport`.
    pub fn new(site: &str, config: ChunkIteratorConfig, transport: T) -> Self {
        Self {
            planner: ChunkPlanner::new(site, config),
            transport,
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

    /// The transport, mutably.
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// Consume the iterator, returning the transport.
    pub fn into_transport(self) -> T {
        self.transport
    }

    /// Only the chunks: waits out [`ChunkEvent::Idle`] and
    /// [`ChunkEvent::Retry`] by calling `sleep` (for example
    /// `std::thread::sleep`) and skips [`ChunkEvent::VolumeAbandoned`].
    /// Errors are still yielded; the iterator pauses and carries on after
    /// each.
    pub fn chunks<S: FnMut(Duration)>(&mut self, sleep: S) -> Chunks<'_, T, S> {
        Chunks { iter: self, sleep }
    }
}

impl<T: ChunkTransport> Iterator for ChunkIterator<T> {
    type Item = Result<ChunkEvent, ChunkIterError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.planner.next_step() {
                PlannerStep::Fetch(request) => {
                    let result = self.transport.fetch(&request);
                    self.planner.complete(result);
                }
                PlannerStep::Event(event) => return Some(event),
            }
        }
    }
}

/// Iterator returned by [`ChunkIterator::chunks`].
#[derive(Debug)]
pub struct Chunks<'a, T, S> {
    iter: &'a mut ChunkIterator<T>,
    sleep: S,
}

impl<T: ChunkTransport, S: FnMut(Duration)> Iterator for Chunks<'_, T, S> {
    type Item = Result<Chunk, ChunkIterError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.iter.next()? {
                Ok(ChunkEvent::Chunk(chunk)) => return Some(Ok(chunk)),
                Ok(
                    ChunkEvent::Idle { poll_after: delay } | ChunkEvent::Retry { after: delay, .. },
                ) => (self.sleep)(delay),
                Ok(ChunkEvent::VolumeAbandoned { .. }) => {}
                Err(error) => return Some(Err(error)),
            }
        }
    }
}

/// [`ChunkTransport`] over the crate's blocking HTTPS client (reqwest +
/// rustls).
#[cfg(feature = "net")]
#[derive(Clone, Debug)]
pub struct ReqwestTransport {
    client: reqwest::blocking::Client,
}

#[cfg(feature = "net")]
impl ReqwestTransport {
    /// A transport over the crate's shared metadata client (25 s request
    /// timeout), or the error that kept the client from being built (the TLS
    /// backend failing to initialize).
    pub fn new() -> Result<Self, TransportError> {
        crate::metadata_http_client()
            .map(Self::with_client)
            .map_err(|err| TransportError::new(TransportErrorKind::Other, err.to_string()))
    }

    /// A caller-configured client.
    pub fn with_client(client: reqwest::blocking::Client) -> Self {
        Self { client }
    }
}

#[cfg(feature = "net")]
impl ChunkTransport for ReqwestTransport {
    fn fetch(&mut self, request: &FetchRequest) -> Result<Vec<u8>, TransportError> {
        let response = self
            .client
            .get(&request.url)
            .send()
            .map_err(reqwest_transport_error)?;
        let status = response.status();
        if !status.is_success() {
            return Err(TransportError::new(
                TransportErrorKind::Status(status.as_u16()),
                format!("{status} for {}", request.url),
            ));
        }
        crate::read_response_limited(response, request.max_bytes, "real-time chunk resource")
            .map_err(|err| match err {
                crate::DataSourceError::Http(err) => reqwest_transport_error(err),
                crate::DataSourceError::Io(err)
                    if err.kind() == std::io::ErrorKind::InvalidData =>
                {
                    TransportError::new(TransportErrorKind::TooLarge, err.to_string())
                }
                other => TransportError::new(TransportErrorKind::Other, other.to_string()),
            })
    }
}

#[cfg(feature = "net")]
fn reqwest_transport_error(err: reqwest::Error) -> TransportError {
    let kind = if err.is_timeout() {
        TransportErrorKind::Timeout
    } else if err.is_connect() || err.is_request() {
        TransportErrorKind::Connect
    } else if let Some(status) = err.status() {
        TransportErrorKind::Status(status.as_u16())
    } else if err.is_body() || err.is_decode() {
        TransportErrorKind::Body
    } else {
        TransportErrorKind::Other
    };
    TransportError::new(kind, crate::reqwest_error_chain(&err))
}

#[cfg(feature = "net")]
impl ChunkIterator<ReqwestTransport> {
    /// A live iterator over the public bucket with the crate's HTTPS client
    /// ([`ReqwestTransport::new`], whose error it returns).
    pub fn live(site: &str, config: ChunkIteratorConfig) -> Result<Self, TransportError> {
        Ok(Self::new(site, config, ReqwestTransport::new()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_ids_wrap_from_999_to_1() {
        assert_eq!(next_volume_id(1), 2);
        assert_eq!(next_volume_id(998), 999);
        assert_eq!(next_volume_id(999), 1);
        assert_eq!(next_volume_id(0), 1);
    }

    #[test]
    fn urls_are_percent_encoded() {
        let planner = ChunkPlanner::new(
            " ktlx",
            ChunkIteratorConfig {
                join: JoinMode::Volume(632),
                ..ChunkIteratorConfig::default()
            },
        );
        assert_eq!(planner.site(), "KTLX");
        assert_eq!(
            planner.listing_url(
                "KTLX/632/",
                false,
                Some("KTLX/632/20260917-012228-015-I"),
                None
            ),
            "https://unidata-nexrad-level2-chunks.s3.amazonaws.com/?list-type=2&prefix=KTLX%2F632%2F\
             &start-after=KTLX%2F632%2F20260917-012228-015-I"
        );
        assert_eq!(
            planner.object_url("KTLX/632/20260917-012228-001-S"),
            "https://unidata-nexrad-level2-chunks.s3.amazonaws.com/KTLX/632/20260917-012228-001-S"
        );
        assert_eq!(percent_encode("a b+c/=", false), "a%20b%2Bc%2F%3D");
    }

    #[test]
    fn retryable_classification() {
        let status = |code| TransportError::new(TransportErrorKind::Status(code), "");
        assert!(status(503).is_retryable());
        assert!(status(429).is_retryable());
        assert!(!status(404).is_retryable());
        assert!(!status(403).is_retryable());
        assert!(TransportError::new(TransportErrorKind::Timeout, "").is_retryable());
        assert!(!TransportError::new(TransportErrorKind::TooLarge, "").is_retryable());
    }
}
