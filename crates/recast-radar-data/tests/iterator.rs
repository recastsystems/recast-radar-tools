//! `ChunkIterator`, `ChunkPlanner` and (feature `async`) `ChunkStream` against
//! recorded real S3 traffic.
//!
//! Each cassette in `tests/fixtures/listings/` was recorded by driving the
//! real iterator against the live `unidata-nexrad-level2-chunks` bucket
//! (`capture_cassette` below, run through `capture.sh`). It holds every
//! request URL with the verbatim response (listing XML inline, chunk bytes in
//! `chunks/`) and every event the iterator produced. Replaying serves the
//! recorded responses in order, fails on the first request that differs from
//! the recording, and compares the events and counters with the live run.
//! Scenario tests add expectations that were checked independently against
//! the recorded XML.

// Tests may unwrap and expect (spec section 2).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::VecDeque;
use std::fs;
use std::path::Path;
use std::time::Duration;

use recast_radar_data::RealtimeChunkType;
use recast_radar_data::realtime::iterator::{
    Chunk, ChunkEvent, ChunkIterError, ChunkIterator, ChunkIteratorConfig, ChunkIteratorStats,
    ChunkPlanner, ChunkTransport, FetchKind, FetchRequest, JoinMode, PlannerStep, TransportError,
    TransportErrorKind,
};
use recast_radar_data::realtime::retry::{Jitter, RetryPolicy};
use serde_json::{Value, json};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/listings");

type Event = Result<ChunkEvent, ChunkIterError>;

// ---------------------------------------------------------------------------
// Cassette format (shared by capture and replay)
// ---------------------------------------------------------------------------

fn kind_name(kind: FetchKind) -> &'static str {
    match kind {
        FetchKind::VolumeIds => "volume_ids",
        FetchKind::ChunkListing => "chunk_listing",
        FetchKind::Chunk => "chunk",
    }
}

fn kind_from_name(name: &str) -> FetchKind {
    match name {
        "volume_ids" => FetchKind::VolumeIds,
        "chunk_listing" => FetchKind::ChunkListing,
        "chunk" => FetchKind::Chunk,
        other => panic!("unknown request kind {other}"),
    }
}

fn error_kind_name(kind: TransportErrorKind) -> String {
    match kind {
        TransportErrorKind::Status(code) => format!("status:{code}"),
        TransportErrorKind::Timeout => "timeout".into(),
        TransportErrorKind::Connect => "connect".into(),
        TransportErrorKind::Body => "body".into(),
        TransportErrorKind::TooLarge => "too_large".into(),
        TransportErrorKind::Other => "other".into(),
    }
}

fn error_kind_from_name(name: &str) -> TransportErrorKind {
    match name {
        "timeout" => TransportErrorKind::Timeout,
        "connect" => TransportErrorKind::Connect,
        "body" => TransportErrorKind::Body,
        "too_large" => TransportErrorKind::TooLarge,
        "other" => TransportErrorKind::Other,
        status => TransportErrorKind::Status(
            status
                .strip_prefix("status:")
                .and_then(|code| code.parse().ok())
                .unwrap_or_else(|| panic!("unknown transport error kind {status}")),
        ),
    }
}

#[cfg_attr(not(feature = "net"), allow(dead_code))] // capture only
fn join_name(join: JoinMode) -> String {
    match join {
        JoinMode::CurrentVolume => "current".into(),
        JoinMode::NextVolume => "next".into(),
        JoinMode::Volume(id) => format!("volume:{id}"),
    }
}

fn join_from_name(name: &str) -> JoinMode {
    match name {
        "current" => JoinMode::CurrentVolume,
        "next" => JoinMode::NextVolume,
        volume => JoinMode::Volume(
            volume
                .strip_prefix("volume:")
                .and_then(|id| id.parse().ok())
                .unwrap_or_else(|| panic!("unknown join mode {volume}")),
        ),
    }
}

#[cfg_attr(not(feature = "net"), allow(dead_code))] // capture only
fn config_json(config: &ChunkIteratorConfig) -> Value {
    json!({
        "bucket_url": config.bucket_url,
        "join": join_name(config.join),
        "download": config.download,
        "poll_interval_ms": config.poll_interval.as_millis() as u64,
        "stall_polls": config.stall_polls,
        "probe_ahead": config.probe_ahead,
        "rediscover_every": config.rediscover_every,
        "retry": {
            "max_attempts": config.retry.max_attempts,
            "initial_delay_ms": config.retry.initial_delay.as_millis() as u64,
            "max_delay_ms": config.retry.max_delay.as_millis() as u64,
            "multiplier": config.retry.multiplier,
            "jitter": format!("{:?}", config.retry.jitter),
        },
        "jitter_seed": config.jitter_seed,
        "max_listing_bytes": config.max_listing_bytes,
        "max_chunk_bytes": config.max_chunk_bytes,
        "max_chunk_failures": config.max_chunk_failures,
    })
}

fn config_from_json(value: &Value) -> ChunkIteratorConfig {
    let u64_at = |key: &str| {
        value[key]
            .as_u64()
            .unwrap_or_else(|| panic!("config {key}"))
    };
    let retry = &value["retry"];
    ChunkIteratorConfig {
        bucket_url: value["bucket_url"].as_str().expect("bucket_url").to_owned(),
        join: join_from_name(value["join"].as_str().expect("join")),
        download: value["download"].as_bool().expect("download"),
        poll_interval: Duration::from_millis(u64_at("poll_interval_ms")),
        stall_polls: u64_at("stall_polls") as u32,
        probe_ahead: u64_at("probe_ahead") as u16,
        rediscover_every: u64_at("rediscover_every") as u32,
        retry: RetryPolicy {
            max_attempts: retry["max_attempts"].as_u64().expect("max_attempts") as u32,
            initial_delay: Duration::from_millis(
                retry["initial_delay_ms"].as_u64().expect("initial"),
            ),
            max_delay: Duration::from_millis(retry["max_delay_ms"].as_u64().expect("max")),
            multiplier: retry["multiplier"].as_f64().expect("multiplier"),
            jitter: match retry["jitter"].as_str().expect("jitter") {
                "None" => Jitter::None,
                "Full" => Jitter::Full,
                "Equal" => Jitter::Equal,
                other => panic!("unknown jitter {other}"),
            },
        },
        jitter_seed: value["jitter_seed"].as_u64(),
        max_listing_bytes: u64_at("max_listing_bytes") as usize,
        max_chunk_bytes: u64_at("max_chunk_bytes") as usize,
        // Cassettes recorded before the field existed ran with the default.
        max_chunk_failures: value["max_chunk_failures"]
            .as_u64()
            .map_or(ChunkIteratorConfig::default().max_chunk_failures, |n| {
                n as u32
            }),
    }
}

fn stats_json(stats: &ChunkIteratorStats) -> Value {
    json!({
        "requests": stats.requests,
        "listing_requests": stats.listing_requests,
        "chunk_requests": stats.chunk_requests,
        "failed_requests": stats.failed_requests,
        "retries": stats.retries,
        "listing_bytes": stats.listing_bytes,
        "chunk_bytes": stats.chunk_bytes,
        "chunks": stats.chunks,
        "volumes_completed": stats.volumes_completed,
        "volumes_abandoned": stats.volumes_abandoned,
    })
}

/// Chunk key -> file name under `chunks/` (`/` becomes `-`, as in testdata).
fn chunk_file_name(key: &str) -> String {
    key.replace('/', "-")
}

/// Everything about an event that the replay must reproduce.
fn summarize(event: &Event) -> Value {
    match event {
        Ok(ChunkEvent::Chunk(chunk)) => json!({
            "event": "chunk",
            "key": chunk.info.object.key,
            "size": chunk.info.object.size,
            "last_modified": chunk.info.object.last_modified.map(|time| time.to_rfc3339()),
            "volume_id": chunk.info.volume_id,
            "volume_time": chunk.info.volume_time.to_rfc3339(),
            "chunk_id": chunk.info.chunk_id,
            "chunk_type": chunk.info.chunk_type.label(),
            "data_len": chunk.data.as_ref().map(Vec::len),
        }),
        Ok(ChunkEvent::Idle { poll_after }) => json!({
            "event": "idle",
            "poll_after_ns": poll_after.as_nanos() as u64,
        }),
        Ok(ChunkEvent::Retry {
            after,
            attempt,
            kind,
            url,
            error,
        }) => json!({
            "event": "retry",
            "after_ns": after.as_nanos() as u64,
            "attempt": attempt,
            "kind": kind_name(*kind),
            "url": url,
            "error_kind": error_kind_name(error.kind),
        }),
        Ok(ChunkEvent::VolumeAbandoned {
            site,
            volume_id,
            volume_time,
            last_chunk_id,
            next_volume_id,
        }) => json!({
            "event": "abandoned",
            "site": site,
            "volume_id": volume_id,
            "volume_time": volume_time.to_rfc3339(),
            "last_chunk_id": last_chunk_id,
            "next_volume_id": next_volume_id,
        }),
        Ok(other) => json!({ "event": format!("{other:?}") }),
        Err(error) => json!({ "event": "error", "message": error.to_string() }),
    }
}

// ---------------------------------------------------------------------------
// Replay
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum RecordedBody {
    Text(String),
    File(String),
}

#[derive(Clone, Debug)]
struct RecordedRequest {
    url: String,
    kind: FetchKind,
    response: Result<RecordedBody, TransportError>,
}

#[derive(Clone, Debug)]
struct Cassette {
    name: String,
    header: Value,
    requests: Vec<RecordedRequest>,
    events: Vec<Value>,
    end: Value,
}

impl Cassette {
    fn load(name: &str) -> Self {
        let path = Path::new(FIXTURES).join(format!("{name}.jsonl"));
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
        let mut header = Value::Null;
        let mut requests = Vec::new();
        let mut events = Vec::new();
        let mut end = Value::Null;
        for (number, line) in text.lines().enumerate() {
            let value: Value = serde_json::from_str(line)
                .unwrap_or_else(|err| panic!("{name}.jsonl line {}: {err}", number + 1));
            match value["type"].as_str() {
                Some("header") => header = value,
                Some("request") => {
                    let response = if let Some(ok) = value.get("ok") {
                        Ok(match (ok["text"].as_str(), ok["file"].as_str()) {
                            (Some(text), _) => RecordedBody::Text(text.to_owned()),
                            (None, Some(file)) => RecordedBody::File(file.to_owned()),
                            _ => panic!("{name}.jsonl line {}: empty response", number + 1),
                        })
                    } else {
                        let err = &value["err"];
                        Err(TransportError::new(
                            error_kind_from_name(err["kind"].as_str().expect("error kind")),
                            err["message"].as_str().unwrap_or_default(),
                        ))
                    };
                    requests.push(RecordedRequest {
                        url: value["url"].as_str().expect("url").to_owned(),
                        kind: kind_from_name(value["kind"].as_str().expect("kind")),
                        response,
                    });
                }
                Some("event") => events.push(value["summary"].clone()),
                Some("end") => end = value,
                other => panic!("{name}.jsonl line {}: unknown type {other:?}", number + 1),
            }
        }
        assert!(header.is_object(), "{name}: no header");
        assert!(end.is_object(), "{name}: no end record");
        Self {
            name: name.to_owned(),
            header,
            requests,
            events,
            end,
        }
    }

    fn site(&self) -> &str {
        self.header["site"].as_str().expect("site")
    }

    fn config(&self) -> ChunkIteratorConfig {
        config_from_json(&self.header["config"])
    }

    fn recorded_stats(&self) -> &Value {
        &self.end["stats"]
    }

    fn body(&self, body: &RecordedBody) -> Vec<u8> {
        match body {
            RecordedBody::Text(text) => text.as_bytes().to_vec(),
            RecordedBody::File(file) => {
                let path = Path::new(FIXTURES).join("chunks").join(file);
                fs::read(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
            }
        }
    }

    fn replay(&self) -> Replay {
        Replay {
            cassette: self.clone(),
            next: 0,
            served: Vec::new(),
        }
    }

    /// Listing XML of recorded request `index`.
    fn listing_text(&self, index: usize) -> &str {
        match &self.requests[index].response {
            Ok(RecordedBody::Text(text)) => text,
            other => panic!("request {index} is not a listing: {other:?}"),
        }
    }
}

/// Serves a cassette's responses in recorded order.
#[derive(Debug)]
struct Replay {
    cassette: Cassette,
    next: usize,
    served: Vec<FetchRequest>,
}

impl Replay {
    fn exhausted(&self) -> bool {
        self.next == self.cassette.requests.len()
    }
}

impl ChunkTransport for Replay {
    fn fetch(&mut self, request: &FetchRequest) -> Result<Vec<u8>, TransportError> {
        let name = &self.cassette.name;
        let Some(recorded) = self.cassette.requests.get(self.next) else {
            panic!(
                "{name}: request {} past the end of the cassette: {}",
                self.next, request.url
            );
        };
        assert_eq!(
            request.url, recorded.url,
            "{name}: request {} differs from the recording (re-capture with capture.sh if the request plan changed)",
            self.next
        );
        assert_eq!(
            request.kind, recorded.kind,
            "{name}: request {} kind",
            self.next
        );
        self.next += 1;
        self.served.push(request.clone());
        match &recorded.response {
            Ok(body) => Ok(self.cassette.body(body)),
            Err(error) => Err(error.clone()),
        }
    }
}

/// Fault injection on top of a replay: before serving the `n`-th fetch
/// (0-based, counting injected failures), fail it with the given error
/// without consuming the recorded response.
struct Faulty {
    inner: Replay,
    fetches: usize,
    faults: VecDeque<(usize, TransportError)>,
}

impl ChunkTransport for Faulty {
    fn fetch(&mut self, request: &FetchRequest) -> Result<Vec<u8>, TransportError> {
        let index = self.fetches;
        self.fetches += 1;
        if self.faults.front().is_some_and(|(at, _)| *at == index)
            && let Some((_, error)) = self.faults.pop_front()
        {
            return Err(error);
        }
        self.inner.fetch(request)
    }
}

/// Mutation on top of a replay: the `n`-th fetch returns the recorded body
/// passed through `mutate`, without consuming the recorded response.
struct Mutating<F> {
    inner: Replay,
    fetches: usize,
    at: usize,
    mutate: F,
}

impl<F: FnMut(Vec<u8>) -> Vec<u8>> ChunkTransport for Mutating<F> {
    fn fetch(&mut self, request: &FetchRequest) -> Result<Vec<u8>, TransportError> {
        let index = self.fetches;
        self.fetches += 1;
        if index == self.at {
            let recorded = self.inner.cassette.requests[self.inner.next].clone();
            let body = self
                .inner
                .cassette
                .body(recorded.response.as_ref().expect("recorded ok"));
            return Ok((self.mutate)(body));
        }
        self.inner.fetch(request)
    }
}

/// What [`Scripted`] does instead of replaying the next recorded response.
#[derive(Clone, Debug)]
enum Injection {
    /// Fail the fetch.
    Fail(TransportError),
    /// Serve the recorded response of request `recorded`. With `same_url`
    /// the fetch must ask for that request's URL.
    Serve { recorded: usize, same_url: bool },
}

/// Injections on top of a replay: the `n`-th fetch (0-based, counting
/// injected ones) gets the injection instead of the next recorded response,
/// which stays unconsumed.
struct Scripted {
    inner: Replay,
    fetches: usize,
    script: VecDeque<(usize, Injection)>,
    injected: Vec<FetchRequest>,
}

impl Scripted {
    fn new(cassette: &Cassette, script: Vec<(usize, Injection)>) -> Self {
        Self {
            inner: cassette.replay(),
            fetches: 0,
            script: script.into(),
            injected: Vec::new(),
        }
    }
}

impl ChunkTransport for Scripted {
    fn fetch(&mut self, request: &FetchRequest) -> Result<Vec<u8>, TransportError> {
        let index = self.fetches;
        self.fetches += 1;
        if self.script.front().is_some_and(|(at, _)| *at == index)
            && let Some((_, injection)) = self.script.pop_front()
        {
            self.injected.push(request.clone());
            return match injection {
                Injection::Fail(error) => Err(error),
                Injection::Serve { recorded, same_url } => {
                    let cassette = &self.inner.cassette;
                    let recorded = &cassette.requests[recorded];
                    if same_url {
                        assert_eq!(request.url, recorded.url, "fetch {index}");
                    }
                    assert_eq!(request.kind, recorded.kind, "fetch {index}");
                    Ok(cassette.body(recorded.response.as_ref().expect("recorded ok")))
                }
            };
        }
        self.inner.fetch(request)
    }
}

fn take_events<T: ChunkTransport>(iter: &mut ChunkIterator<T>, count: usize) -> Vec<Event> {
    (0..count)
        .map(|_| iter.next().expect("the chunk iterator never ends"))
        .collect()
}

/// Replay a whole cassette through a blocking iterator and check events,
/// counters and request coverage against the recording.
fn replay_blocking(name: &str) -> (Cassette, ChunkIterator<Replay>, Vec<Event>) {
    let cassette = Cassette::load(name);
    let mut iter = ChunkIterator::new(cassette.site(), cassette.config(), cassette.replay());
    let events = take_events(&mut iter, cassette.events.len());
    let summaries: Vec<Value> = events.iter().map(summarize).collect();
    for (index, (replayed, recorded)) in summaries.iter().zip(&cassette.events).enumerate() {
        assert_eq!(replayed, recorded, "{name}: event {index}");
    }
    assert!(
        iter.transport().exhausted(),
        "{name}: replay used {} of {} recorded requests",
        iter.transport().next,
        cassette.requests.len()
    );
    assert_eq!(
        &stats_json(&iter.stats()),
        cassette.recorded_stats(),
        "{name}: counters"
    );
    (cassette, iter, events)
}

fn chunks(events: &[Event]) -> Vec<&Chunk> {
    events
        .iter()
        .filter_map(|event| match event {
            Ok(ChunkEvent::Chunk(chunk)) => Some(chunk),
            _ => None,
        })
        .collect()
}

/// Keys listed in an S3 listing, parsed independently of the crate.
fn listed_keys(xml: &str) -> Vec<String> {
    xml.split("<Key>")
        .skip(1)
        .map(|rest| rest.split("</Key>").next().expect("closing Key").to_owned())
        .collect()
}

fn body_len_total(cassette: &Cassette, kind: FetchKind) -> u64 {
    cassette
        .requests
        .iter()
        .filter(|request| (request.kind == FetchKind::Chunk) == (kind == FetchKind::Chunk))
        .filter_map(|request| request.response.as_ref().ok())
        .map(|body| cassette.body(body).len() as u64)
        .sum()
}

const CASSETTES: &[&str] = &[
    "tlas-999-wrap",
    "tmco-710-abandoned",
    "phkm-live-join",
    "kmxx-offline",
    "tlas-next-volume-bytes",
    "pabc-rollover-leftover",
    "tlas-chunk-too-large",
];

// ---------------------------------------------------------------------------
// Whole-cassette replays
// ---------------------------------------------------------------------------

#[test]
fn every_cassette_replays_identically() {
    for name in CASSETTES {
        let (cassette, iter, _) = replay_blocking(name);
        // Byte counters equal the recorded bodies, split by kind.
        assert_eq!(
            iter.stats().listing_bytes,
            body_len_total(&cassette, FetchKind::ChunkListing),
            "{name}"
        );
        assert_eq!(
            iter.stats().chunk_bytes,
            body_len_total(&cassette, FetchKind::Chunk),
            "{name}"
        );
        assert_eq!(
            iter.stats().requests,
            cassette.requests.len() as u64,
            "{name}"
        );
    }
}

#[test]
fn planner_driven_by_hand_matches_the_iterator() {
    let cassette = Cassette::load("tmco-710-abandoned");
    let mut planner = ChunkPlanner::new(cassette.site(), cassette.config());
    let mut transport = cassette.replay();
    let mut summaries = Vec::new();
    // A driver may ask for the step twice before completing it.
    while summaries.len() < cassette.events.len() {
        match planner.next_step() {
            PlannerStep::Fetch(request) => {
                assert_eq!(planner.next_step(), PlannerStep::Fetch(request.clone()));
                let result = transport.fetch(&request);
                planner.complete(result);
            }
            PlannerStep::Event(event) => summaries.push(summarize(&event)),
        }
    }
    assert_eq!(summaries, cassette.events);
    // A completion with nothing outstanding is ignored.
    planner.complete(Ok(Vec::new()));
    assert_eq!(&stats_json(&planner.stats()), cassette.recorded_stats());
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// Historical walk from TLAS volume 999 across the id wrap to volume 1.
#[test]
fn walks_volume_999_and_wraps_to_volume_1() {
    let (cassette, iter, events) = replay_blocking("tlas-999-wrap");
    let served: Vec<&str> = iter
        .transport()
        .served
        .iter()
        .map(|r| r.url.as_str())
        .collect();
    assert_eq!(
        served[0],
        "https://unidata-nexrad-level2-chunks.s3.amazonaws.com/?list-type=2&prefix=TLAS%2F999%2F"
    );
    assert!(served.iter().all(|url| !url.contains("prefix=TLAS%2F0%2F")));
    assert!(
        served.iter().all(|url| !url.contains("delimiter")),
        "no id listing for Volume(999)"
    );

    let keys_999 = listed_keys(cassette.listing_text(0));
    let delivered = chunks(&events);
    let (volume_999, volume_1): (Vec<&Chunk>, Vec<&Chunk>) = delivered
        .iter()
        .copied()
        .partition(|chunk| chunk.info.volume_id == 999);
    assert_eq!(
        volume_999
            .iter()
            .map(|c| c.info.object.key.clone())
            .collect::<Vec<_>>(),
        keys_999,
        "every listed chunk of 999, in order"
    );
    for (index, chunk) in volume_999.iter().enumerate() {
        assert_eq!(usize::from(chunk.info.chunk_id), index + 1);
        assert!(chunk.data.is_none());
    }
    assert_eq!(volume_999[0].info.chunk_type, RealtimeChunkType::Start);
    let last = volume_999.last().expect("volume 999 chunks");
    assert_eq!(last.info.chunk_type, RealtimeChunkType::End);

    let first_of_1 = volume_1.first().expect("volume 1 reached");
    assert_eq!(first_of_1.info.chunk_type, RealtimeChunkType::Start);
    assert_eq!(first_of_1.info.chunk_id, 1);
    assert!(first_of_1.info.volume_time > last.info.volume_time);
    assert_eq!(iter.stats().volumes_completed, 1);
    let position = iter.planner().position().expect("position");
    assert_eq!(position.volume_id, 1);
}

/// TMCO volume 710 never got its End chunk; 711 holds one chunk with a
/// bogus 1970 volume time; 712 is the real successor.
#[test]
fn abandons_a_volume_without_end_and_skips_a_bogus_successor() {
    let (cassette, iter, events) = replay_blocking("tmco-710-abandoned");
    let summaries: Vec<Value> = events.iter().map(summarize).collect();
    let abandoned: Vec<&Value> = summaries
        .iter()
        .filter(|s| s["event"] == "abandoned")
        .collect();
    assert_eq!(abandoned.len(), 1);
    assert_eq!(abandoned[0]["volume_id"], 710);
    assert_eq!(abandoned[0]["next_volume_id"], 712);

    let delivered = chunks(&events);
    let from_710: Vec<_> = delivered
        .iter()
        .filter(|c| c.info.volume_id == 710)
        .collect();
    assert_eq!(from_710.len(), listed_keys(cassette.listing_text(0)).len());
    assert_eq!(
        abandoned[0]["last_chunk_id"],
        u64::from(from_710.last().expect("710 chunks").info.chunk_id)
    );
    assert!(delivered.iter().all(|c| c.info.volume_id != 711));
    let first_712 = delivered
        .iter()
        .find(|c| c.info.volume_id == 712)
        .expect("712 reached");
    assert_eq!(first_712.info.chunk_type, RealtimeChunkType::Start);

    // The probe listed 711 (bogus 1970 volume time) before 712.
    let probe_711 = cassette
        .requests
        .iter()
        .position(|r| r.url.ends_with("prefix=TMCO%2F711%2F"))
        .expect("probe of 711");
    assert!(
        listed_keys(cassette.listing_text(probe_711))[0].starts_with("TMCO/711/19700101-000000-")
    );
    assert_eq!(iter.stats().volumes_abandoned, 1);
    assert_eq!(iter.stats().volumes_completed, 0);
}

/// Live join at PHKM while a volume was being collected, through its End
/// chunk and into the next volume.
#[test]
fn joins_mid_volume_and_follows_the_rollover() {
    let (cassette, iter, events) = replay_blocking("phkm-live-join");
    let requests = &cassette.requests;
    assert_eq!(requests[0].kind, FetchKind::VolumeIds);
    assert!(requests[0].url.ends_with("prefix=PHKM%2F&delimiter=%2F"));

    // Newest id from the recorded id listing, computed independently: the id
    // before the largest gap on the 1..=999 ring.
    let ids_xml = cassette.listing_text(0);
    let mut ids: Vec<u32> = ids_xml
        .split("<Prefix>PHKM/")
        .skip(1)
        .filter_map(|rest| rest.split('/').next()?.parse().ok())
        .collect();
    ids.sort_unstable();
    let newest = ids
        .iter()
        .enumerate()
        .max_by_key(|(index, id)| {
            let next = ids.get(index + 1).copied().unwrap_or(ids[0] + 1000);
            next - **id
        })
        .map(|(_, id)| *id)
        .expect("ids");
    assert!(
        requests[1]
            .url
            .ends_with(&format!("prefix=PHKM%2F{newest}%2F")),
        "{}",
        requests[1].url
    );

    // Mid-volume: the join listing already held several chunks, and the
    // first one delivered is still chunk 1.
    let joined = listed_keys(cassette.listing_text(1));
    assert!(
        joined.len() > 1,
        "joined with {} chunks listed",
        joined.len()
    );
    let delivered = chunks(&events);
    assert_eq!(delivered[0].info.object.key, joined[0]);
    assert_eq!(delivered[0].info.chunk_type, RealtimeChunkType::Start);

    // Later polls ask only for keys after the last one taken.
    assert!(
        requests[2..]
            .iter()
            .filter(|r| r.kind == FetchKind::ChunkListing)
            .any(|r| r.url.contains("&start-after=PHKM%2F"))
    );

    let end = delivered
        .iter()
        .position(|c| c.info.chunk_type == RealtimeChunkType::End)
        .expect("End chunk");
    for (index, chunk) in delivered[..=end].iter().enumerate() {
        assert_eq!(usize::from(chunk.info.chunk_id), index + 1);
        assert_eq!(u32::from(chunk.info.volume_id), newest);
    }
    let next = &delivered[end + 1..];
    assert!(!next.is_empty());
    let next_id = if newest == 999 { 1 } else { newest + 1 };
    assert!(next.iter().all(|c| u32::from(c.info.volume_id) == next_id));
    assert_eq!(next[0].info.chunk_type, RealtimeChunkType::Start);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Ok(ChunkEvent::Idle { .. })))
    );
    assert_eq!(iter.stats().volumes_completed, 1);
}

/// KMXX stopped sending volumes: after the last listed chunk the iterator
/// only idles, probes the next ids and periodically relists the site.
#[test]
fn idles_and_probes_while_a_radar_is_offline() {
    let (cassette, iter, events) = replay_blocking("kmxx-offline");
    let config = cassette.config();
    let delivered = chunks(&events);
    let last_chunk = events
        .iter()
        .rposition(|e| matches!(e, Ok(ChunkEvent::Chunk(_))))
        .expect("chunks");
    assert!(events[last_chunk + 1..].iter().all(
        |e| matches!(e, Ok(ChunkEvent::Idle { poll_after }) if *poll_after == config.poll_interval)
    ));
    let volume = delivered[0].info.volume_id;
    assert!(delivered.iter().all(|c| c.info.volume_id == volume));
    assert!(
        delivered
            .iter()
            .all(|c| c.info.chunk_type != RealtimeChunkType::End)
    );

    let urls: Vec<&str> = cassette.requests.iter().map(|r| r.url.as_str()).collect();
    let probe = |id: u16| format!("prefix=KMXX%2F{id}%2F");
    let probes_1 = urls
        .iter()
        .filter(|u| u.ends_with(&probe(volume + 1)))
        .count();
    let probes_2 = urls
        .iter()
        .filter(|u| u.ends_with(&probe(volume + 2)))
        .count();
    assert!(
        probes_1 >= 2 && probes_1 == probes_2,
        "{probes_1} / {probes_2}"
    );
    let id_listings = cassette
        .requests
        .iter()
        .filter(|r| r.kind == FetchKind::VolumeIds)
        .count();
    assert!(
        id_listings >= 2,
        "join listing plus at least one rediscovery"
    );
    assert_eq!(iter.stats().volumes_abandoned, 0);
    assert_eq!(iter.planner().position().map(|p| p.volume_id), Some(volume));
}

/// Live `NextVolume` join at TLAS with downloads: skip the volume in
/// progress, then download the next volume's first three chunks.
#[test]
fn next_volume_join_downloads_real_chunk_bytes() {
    let (cassette, iter, events) = replay_blocking("tlas-next-volume-bytes");
    let delivered = chunks(&events);
    assert_eq!(delivered.len(), 3);
    let joined_id = cassette.requests[1]
        .url
        .rsplit("prefix=TLAS%2F")
        .next()
        .and_then(|rest| rest.split("%2F").next())
        .and_then(|id| id.parse::<u16>().ok())
        .expect("joined id");
    let expected_id = if joined_id == 999 { 1 } else { joined_id + 1 };
    for (index, chunk) in delivered.iter().enumerate() {
        assert_eq!(chunk.info.volume_id, expected_id);
        assert_eq!(usize::from(chunk.info.chunk_id), index + 1);
        let data = chunk.data.as_ref().expect("downloaded");
        let file = Path::new(FIXTURES)
            .join("chunks")
            .join(chunk_file_name(&chunk.info.object.key));
        assert_eq!(data, &fs::read(&file).expect("chunk file"));
        assert_eq!(data.len() as u64, chunk.info.object.size);
    }
    // The Start chunk carries the Archive II volume header, whose extension
    // is the volume id, and the ICAO.
    let start = delivered[0].data.as_ref().expect("start bytes");
    assert_eq!(
        &start[..12],
        format!("AR2V0008.{expected_id:03}").as_bytes()
    );
    assert_eq!(&start[20..24], b"TLAS");
    // Start + two Intermediate chunks decode as a partial volume: the
    // volume header time from the key, and 120 Message 31 radials per
    // Intermediate chunk (counted with an independent parser at capture).
    let bytes: Vec<u8> = delivered
        .iter()
        .flat_map(|c| c.data.clone().expect("downloaded"))
        .collect();
    let volume = recast_radar_io::decode_supported_volume_bytes(&bytes).expect("decode");
    assert_eq!(volume.site.id, "TLAS");
    assert_eq!(volume.volume_time, delivered[0].info.volume_time);
    let radials: usize = volume.cuts.iter().map(|cut| cut.radials.len()).sum();
    assert_eq!(radials, 240);
    assert_eq!(
        iter.stats().chunk_requests,
        3,
        "exactly one GET per delivered chunk"
    );
    assert_eq!(
        iter.stats().chunk_bytes,
        delivered.iter().map(|c| c.info.object.size).sum::<u64>()
    );
}

/// Volume times under one id in a listing, parsed independently of the
/// crate: `YYYYMMDD-HHMMSS` from each key, sorted and deduplicated.
fn listed_volume_times(xml: &str) -> Vec<String> {
    let mut times: Vec<String> = listed_keys(xml)
        .iter()
        .map(|key| key.rsplit('/').next().expect("key name")[..15].to_owned())
        .collect();
    times.sort();
    times.dedup();
    times
}

/// PABC (Air Force WSR-88D, Bethel) restarts its volume numbering every few
/// hours, so ids keep volumes from earlier cycles. Live `Volume(42)` follow:
/// - id 42 held three volumes (2026-09-15, 2026-09-16 and the one in
///   progress); the newest is followed;
/// - after its End chunk, id 43 held only a complete two-day-old volume, which
///   is skipped (it is not newer than volume 42);
/// - the radar's new volume 43 then appears next to the leftover and is
///   followed from its Start chunk.
#[test]
fn skips_an_older_cycle_left_under_the_next_volume_id() {
    let (cassette, iter, events) = replay_blocking("pabc-rollover-leftover");
    let requests = &cassette.requests;
    let times_42 = listed_volume_times(cassette.listing_text(0));
    assert_eq!(times_42.len(), 3, "{times_42:?}");
    let newest_42 = times_42.last().expect("volume times");
    let delivered = chunks(&events);
    let first = delivered.first().expect("chunks");
    assert!(
        first
            .info
            .object
            .key
            .starts_with(&format!("PABC/42/{newest_42}-001-S"))
    );

    let end = delivered
        .iter()
        .position(|c| c.info.chunk_type == RealtimeChunkType::End)
        .expect("End of volume 42");
    for (index, chunk) in delivered[..=end].iter().enumerate() {
        assert_eq!(usize::from(chunk.info.chunk_id), index + 1);
        assert!(
            chunk
                .info
                .object
                .key
                .starts_with(&format!("PABC/42/{newest_42}-"))
        );
    }

    // The first listing of 43 right after the End chunk: one volume, older
    // than volume 42, complete from Start to End.
    let first_43 = requests
        .iter()
        .position(|r| r.url.ends_with("prefix=PABC%2F43%2F"))
        .expect("listing of 43");
    let leftover = listed_volume_times(cassette.listing_text(first_43));
    assert_eq!(leftover.len(), 1);
    assert!(leftover[0].as_str() < newest_42.as_str());
    let leftover_keys = listed_keys(cassette.listing_text(first_43));
    assert!(leftover_keys[0].ends_with("-001-S"));
    assert!(leftover_keys.last().expect("keys").ends_with("-E"));

    // The listing where the new volume 43 shows up next to the leftover.
    let with_new = (first_43..requests.len())
        .find(|index| listed_volume_times(cassette.listing_text(*index)).len() == 2)
        .expect("new volume 43 listed");
    let new_43 = listed_volume_times(cassette.listing_text(with_new))[1].clone();
    assert!(new_43.as_str() > newest_42.as_str());
    // Until then only Idle events: nothing of the leftover was delivered.
    let after_end = &delivered[end + 1..];
    assert_eq!(after_end.len(), 3);
    for (index, chunk) in after_end.iter().enumerate() {
        assert_eq!(chunk.info.volume_id, 43);
        assert_eq!(usize::from(chunk.info.chunk_id), index + 1);
        assert!(
            chunk
                .info
                .object
                .key
                .starts_with(&format!("PABC/43/{new_43}-"))
        );
    }
    assert!(
        delivered
            .iter()
            .all(|c| !c.info.object.key.contains(&leftover[0]))
    );
    assert!(
        with_new > first_43,
        "at least one poll saw only the leftover"
    );
    assert_eq!(iter.stats().volumes_completed, 1);
    assert_eq!(iter.stats().volumes_abandoned, 0);
}

/// Historical TLAS walk from volume 998 with `max_chunk_bytes` (4096) below
/// the size of every Intermediate chunk (recorded live: the HTTPS transport
/// refused each body from its Content-Length). Each volume delivers its
/// Start chunk, then the first Intermediate chunk fails once, without retry,
/// and the volume is abandoned for the next id (998 -> 999 -> 1).
#[test]
fn chunk_larger_than_the_limit_abandons_its_volume() {
    let (cassette, iter, events) = replay_blocking("tlas-chunk-too-large");
    let config = cassette.config();
    let summaries: Vec<Value> = events.iter().map(summarize).collect();
    let kinds: Vec<&str> = summaries
        .iter()
        .map(|s| s["event"].as_str().expect("event"))
        .collect();
    assert_eq!(
        kinds,
        [
            "chunk",
            "error",
            "abandoned",
            "idle",
            "chunk",
            "error",
            "abandoned"
        ]
    );
    for (volume_id, next_volume_id, listing) in [(998, 999, 0), (999, 1, 3)] {
        let keys = listed_keys(cassette.listing_text(listing));
        assert!(keys[0].ends_with("-001-S") && keys[1].ends_with("-002-I"));
        // Sizes from the recorded XML: the Start chunk fits, chunk 2 does not.
        let xml = cassette.listing_text(listing);
        let sizes: Vec<usize> = xml
            .split("<Size>")
            .skip(1)
            .map(|rest| {
                rest.split("</Size>")
                    .next()
                    .expect("Size")
                    .parse()
                    .expect("size")
            })
            .collect();
        assert!(sizes[0] <= config.max_chunk_bytes && sizes[1] > config.max_chunk_bytes);
        let abandoned = summaries
            .iter()
            .find(|s| s["event"] == "abandoned" && s["volume_id"] == volume_id)
            .expect("abandoned");
        assert_eq!(abandoned["last_chunk_id"], 1);
        assert_eq!(abandoned["next_volume_id"], next_volume_id);
    }
    let errors: Vec<(u32, TransportErrorKind, &str)> = events
        .iter()
        .filter_map(|e| match e {
            Err(ChunkIterError::Transport {
                attempts,
                error,
                url,
                kind: FetchKind::Chunk,
            }) => Some((*attempts, error.kind, url.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(errors.len(), 2);
    assert!(errors.iter().all(|(attempts, kind, url)| *attempts == 1
        && *kind == TransportErrorKind::TooLarge
        && url.ends_with("-002-I")));
    // One GET per chunk: nothing repeated.
    let chunk_urls: Vec<&str> = cassette
        .requests
        .iter()
        .filter(|r| r.kind == FetchKind::Chunk)
        .map(|r| r.url.as_str())
        .collect();
    let mut unique = chunk_urls.clone();
    unique.dedup();
    assert_eq!(chunk_urls, unique);
    assert_eq!(iter.stats().volumes_abandoned, 2);
    let position = iter.planner().position().expect("position");
    assert_eq!((position.volume_id, position.next_chunk_id), (1, 1));
}

// ---------------------------------------------------------------------------
// Retries and failures on top of real responses
// ---------------------------------------------------------------------------

fn fault_config(cassette: &Cassette, max_attempts: u32) -> ChunkIteratorConfig {
    ChunkIteratorConfig {
        retry: RetryPolicy {
            max_attempts,
            initial_delay: Duration::from_millis(400),
            max_delay: Duration::from_secs(5),
            multiplier: 2.0,
            jitter: Jitter::Equal,
        },
        jitter_seed: Some(99),
        ..cassette.config()
    }
}

/// Replay with faults and return the events with Retry/error/idle events
/// that the faults added removed, checking what was removed.
fn replay_with_faults(
    name: &str,
    max_attempts: u32,
    faults: Vec<(usize, TransportError)>,
    extra_events: usize,
) -> (Cassette, ChunkIterator<Faulty>, Vec<Event>) {
    let cassette = Cassette::load(name);
    let transport = Faulty {
        inner: cassette.replay(),
        fetches: 0,
        faults: faults.into(),
    };
    let mut iter = ChunkIterator::new(
        cassette.site(),
        fault_config(&cassette, max_attempts),
        transport,
    );
    let events = take_events(&mut iter, cassette.events.len() + extra_events);
    (cassette, iter, events)
}

fn without_fault_events(events: &[Event]) -> Vec<Value> {
    let mut summaries: Vec<Value> = Vec::new();
    for event in events {
        match event {
            Ok(ChunkEvent::Retry { .. }) | Err(ChunkIterError::Transport { .. }) => {}
            Ok(ChunkEvent::Idle { .. })
                if summaries
                    .last()
                    .is_some_and(|s| s["event"] == "error_marker") =>
            {
                summaries.pop();
            }
            _ => summaries.push(summarize(event)),
        }
        if matches!(event, Err(ChunkIterError::Transport { .. })) {
            summaries.push(json!({ "event": "error_marker" }));
        }
    }
    summaries
}

#[test]
fn transient_failure_is_retried_with_backoff() {
    let timeout = TransportError::new(TransportErrorKind::Timeout, "injected timeout");
    let (cassette, iter, events) = replay_with_faults(
        "tlas-999-wrap",
        4,
        vec![(0, timeout.clone()), (1, timeout)],
        2,
    );
    let retries: Vec<(Duration, u32)> = events
        .iter()
        .filter_map(|e| match e {
            Ok(ChunkEvent::Retry {
                after,
                attempt,
                url,
                kind,
                ..
            }) => {
                assert_eq!(url, &cassette.requests[0].url);
                assert_eq!(*kind, FetchKind::ChunkListing);
                Some((*after, *attempt))
            }
            _ => None,
        })
        .collect();
    assert_eq!(retries.len(), 2);
    assert_eq!((retries[0].1, retries[1].1), (2, 3));
    // Equal jitter: within [ceiling / 2, ceiling] of 400 ms, then 800 ms.
    assert!(
        retries[0].0 >= Duration::from_millis(200) && retries[0].0 <= Duration::from_millis(400)
    );
    assert!(
        retries[1].0 >= Duration::from_millis(400) && retries[1].0 <= Duration::from_millis(800)
    );
    assert_eq!(without_fault_events(&events), cassette.events);
    let stats = iter.stats();
    assert_eq!(stats.retries, 2);
    assert_eq!(stats.failed_requests, 2);
    assert_eq!(stats.requests, cassette.requests.len() as u64 + 2);
    assert!(iter.transport().inner.exhausted());
}

#[test]
fn exhausted_retries_report_an_error_then_start_over() {
    let unavailable = TransportError::new(TransportErrorKind::Status(503), "injected SlowDown");
    // Three attempts fail; the fourth (after the error and a pause) succeeds.
    let faults = (0..3).map(|at| (at, unavailable.clone())).collect();
    let (cassette, iter, events) = replay_with_faults("tlas-999-wrap", 3, faults, 4);
    let head: Vec<Value> = events[..4].iter().map(summarize).collect();
    assert_eq!(head[0]["event"], "retry");
    assert_eq!(head[1]["event"], "retry");
    match &events[2] {
        Err(ChunkIterError::Transport {
            attempts,
            error,
            url,
            ..
        }) => {
            assert_eq!(*attempts, 3);
            assert_eq!(error.kind, TransportErrorKind::Status(503));
            assert_eq!(url, &cassette.requests[0].url);
        }
        other => panic!("expected the transport error, got {other:?}"),
    }
    assert!(matches!(events[3], Ok(ChunkEvent::Idle { .. })));
    assert_eq!(without_fault_events(&events), cassette.events);
    assert_eq!(iter.stats().failed_requests, 3);
    assert_eq!(iter.stats().retries, 2);
}

/// Request indices in `tlas-next-volume-bytes`: 17 lists volume 3 (Start
/// chunk only), 18 downloads the Start chunk, 19 lists after it (chunks 2-4),
/// 20 and 21 download chunks 2 and 3.
const TLAS_LIST_3_START: usize = 17;
const TLAS_LIST_3_AFTER_START: usize = 19;
const TLAS_GET_CHUNK_2: usize = 20;
/// Request 16 listed volume 3 before it started: S3's real answer to the
/// same prefix with no keys.
const TLAS_LIST_3_EMPTY: usize = 16;

/// A chunk download that fails for good (a 404, or 503s past the retry
/// budget) is not repeated blindly: the planner lists the volume again from
/// the failed chunk (the recorded answer to that URL still shows it) and
/// then downloads it.
#[test]
fn failed_chunk_download_relists_the_volume_then_fetches_again() {
    let not_found = TransportError::new(TransportErrorKind::Status(404), "injected NoSuchKey");
    let slow_down = TransportError::new(TransportErrorKind::Status(503), "injected SlowDown");
    for (max_attempts, failures) in [
        (4, vec![not_found]),
        (2, vec![slow_down.clone(), slow_down]),
    ] {
        let cassette = Cassette::load("tlas-next-volume-bytes");
        assert_eq!(
            cassette.requests[TLAS_GET_CHUNK_2].url,
            "https://unidata-nexrad-level2-chunks.s3.amazonaws.com/TLAS/3/20260917-015242-002-I"
        );
        let status = failures[0].kind;
        let mut script: Vec<(usize, Injection)> = failures
            .iter()
            .enumerate()
            .map(|(offset, error)| (TLAS_GET_CHUNK_2 + offset, Injection::Fail(error.clone())))
            .collect();
        script.push((
            TLAS_GET_CHUNK_2 + failures.len(),
            Injection::Serve {
                recorded: TLAS_LIST_3_AFTER_START,
                same_url: true,
            },
        ));
        let transport = Scripted::new(&cassette, script);
        let mut iter = ChunkIterator::new(
            cassette.site(),
            fault_config(&cassette, max_attempts),
            transport,
        );
        let extra = failures.len() + 1; // Retry events, the error and its Idle
        let events = take_events(&mut iter, cassette.events.len() + extra);
        let retries = events
            .iter()
            .filter(|e| matches!(e, Ok(ChunkEvent::Retry { .. })))
            .count();
        assert_eq!(retries, failures.len() - 1, "{status:?}");
        let error = events
            .iter()
            .find_map(|e| match e {
                Err(ChunkIterError::Transport {
                    attempts,
                    kind,
                    error,
                    ..
                }) => Some((*attempts, *kind, error.kind)),
                _ => None,
            })
            .expect("error");
        assert_eq!(error, (failures.len() as u32, FetchKind::Chunk, status));
        assert_eq!(without_fault_events(&events), cassette.events, "{status:?}");
        let transport = iter.transport();
        let relist = transport.injected.last().expect("relisting");
        assert_eq!(relist.kind, FetchKind::ChunkListing);
        assert!(
            relist
                .url
                .ends_with("&start-after=TLAS%2F3%2F20260917-015242-001-S")
        );
        assert!(transport.inner.exhausted());
        let stats = iter.stats();
        assert_eq!(
            stats.requests,
            cassette.requests.len() as u64 + extra as u64
        );
        assert_eq!(stats.volumes_abandoned, 0);
    }
}

/// Drive a planner until it asks for a request `stop` accepts, answering
/// the others with `answer`. Returns the events and the stopping request.
fn drive_until(
    planner: &mut ChunkPlanner,
    mut answer: impl FnMut(&FetchRequest) -> Option<Result<Vec<u8>, TransportError>>,
    max_steps: usize,
) -> (Vec<Event>, Vec<FetchRequest>, Option<FetchRequest>) {
    let mut events = Vec::new();
    let mut fetched = Vec::new();
    for _ in 0..max_steps {
        match planner.next_step() {
            PlannerStep::Event(event) => events.push(event),
            PlannerStep::Fetch(request) => match answer(&request) {
                Some(result) => {
                    fetched.push(request);
                    planner.complete(result);
                }
                None => return (events, fetched, Some(request)),
            },
        }
    }
    (events, fetched, None)
}

/// The chunk is gone when the volume is listed again (the relisting gets
/// S3's real empty answer for volume 3's prefix): the volume is abandoned
/// right away and the next request lists volume 4.
#[test]
fn purged_chunk_abandons_the_volume() {
    let cassette = Cassette::load("tlas-next-volume-bytes");
    let not_found = TransportError::new(TransportErrorKind::Status(404), "injected NoSuchKey");
    let mut transport = Scripted::new(
        &cassette,
        vec![
            (TLAS_GET_CHUNK_2, Injection::Fail(not_found)),
            (
                TLAS_GET_CHUNK_2 + 1,
                Injection::Serve {
                    recorded: TLAS_LIST_3_EMPTY,
                    same_url: false,
                },
            ),
        ],
    );
    let mut planner = ChunkPlanner::new(cassette.site(), fault_config(&cassette, 4));
    let (events, _, stop) = drive_until(
        &mut planner,
        |request| (!request.url.contains("prefix=TLAS%2F4%2F")).then(|| transport.fetch(request)),
        500,
    );
    let stop = stop.expect("listing of volume 4");
    assert_eq!(
        stop.url,
        "https://unidata-nexrad-level2-chunks.s3.amazonaws.com/?list-type=2&prefix=TLAS%2F4%2F"
    );
    let summaries: Vec<Value> = events.iter().map(summarize).collect();
    // Start chunk, the pause before polling for chunk 2, the 404 and its
    // pause, then the abandonment right after the relisting.
    let tail: Vec<&str> = summaries[summaries.len() - 5..]
        .iter()
        .map(|s| s["event"].as_str().expect("event"))
        .collect();
    assert_eq!(tail, ["chunk", "idle", "error", "idle", "abandoned"]);
    let abandoned = summaries.last().expect("abandoned");
    assert_eq!(abandoned["volume_id"], 3);
    assert_eq!(abandoned["volume_time"], "2026-09-17T01:52:42+00:00");
    assert_eq!(abandoned["last_chunk_id"], 1);
    assert_eq!(abandoned["next_volume_id"], 4);
    // The relisting started after the delivered Start chunk.
    assert!(
        transport.injected[1]
            .url
            .ends_with("&start-after=TLAS%2F3%2F20260917-015242-001-S")
    );
    assert_eq!(planner.stats().volumes_abandoned, 1);
    let position = planner.position().expect("position");
    assert_eq!((position.volume_id, position.next_chunk_id), (4, 1));
}

/// The verifier's reproduction of a stuck iterator: `Volume(3)` with
/// downloads, every listing of volume 3 answered with its real recorded
/// listing (Start chunk listed), every chunk GET failing with 404. The
/// planner gives up after `max_chunk_failures` rounds and moves to volume 4.
#[test]
fn chunk_that_keeps_failing_is_given_up_after_max_chunk_failures() {
    let cassette = Cassette::load("tlas-next-volume-bytes");
    let listing_3 = cassette.requests[TLAS_LIST_3_START].url.clone();
    let body_3 = cassette.listing_text(TLAS_LIST_3_START).as_bytes().to_vec();
    for max_chunk_failures in [0, 1, 3] {
        let config = ChunkIteratorConfig {
            join: JoinMode::Volume(3),
            download: true,
            stall_polls: 1,
            max_chunk_failures,
            ..fault_config(&cassette, 4)
        };
        let mut planner = ChunkPlanner::new("TLAS", config);
        let (events, fetched, stop) = drive_until(
            &mut planner,
            |request| {
                if request.url == listing_3 {
                    Some(Ok(body_3.clone()))
                } else if request.kind == FetchKind::Chunk {
                    Some(Err(TransportError::new(
                        TransportErrorKind::Status(404),
                        "injected NoSuchKey",
                    )))
                } else {
                    None
                }
            },
            2000,
        );
        let rounds = max_chunk_failures.max(1) as usize;
        let stop = stop.expect("the planner moved on");
        assert!(stop.url.ends_with("prefix=TLAS%2F4%2F"), "{}", stop.url);
        let gets = fetched
            .iter()
            .filter(|r| r.kind == FetchKind::Chunk)
            .count();
        let listings = fetched.len() - gets;
        assert_eq!((gets, listings), (rounds, rounds), "{max_chunk_failures}");
        let errors = events.iter().filter(|e| e.is_err()).count();
        assert_eq!(errors, rounds);
        // Nothing was delivered, so there is no VolumeAbandoned event.
        assert!(events.iter().all(|e| !matches!(
            e,
            Ok(ChunkEvent::VolumeAbandoned { .. } | ChunkEvent::Chunk(_))
        )));
        assert_eq!(planner.position().map(|p| p.volume_id), Some(4));
    }
}

#[test]
fn truncated_chunk_body_is_rejected_and_refetched() {
    let cassette = Cassette::load("tlas-next-volume-bytes");
    let chunk_fetch = cassette
        .requests
        .iter()
        .rposition(|r| r.kind == FetchKind::Chunk)
        .expect("chunk request");
    let transport = Mutating {
        inner: cassette.replay(),
        fetches: 0,
        at: chunk_fetch,
        mutate: |mut body: Vec<u8>| {
            body.truncate(body.len() / 2);
            body
        },
    };
    let mut iter = ChunkIterator::new(cassette.site(), fault_config(&cassette, 4), transport);
    let events = take_events(&mut iter, cassette.events.len() + 1);
    let retry = events
        .iter()
        .find_map(|e| match e {
            Ok(ChunkEvent::Retry { error, kind, .. }) => Some((error.kind, *kind)),
            _ => None,
        })
        .expect("retry");
    assert_eq!(retry, (TransportErrorKind::Body, FetchKind::Chunk));
    let summaries: Vec<Value> = events
        .iter()
        .filter(|e| !matches!(e, Ok(ChunkEvent::Retry { .. })))
        .map(summarize)
        .collect();
    assert_eq!(summaries, cassette.events);
    assert!(iter.transport().inner.exhausted());
}

#[test]
fn oversized_listing_is_a_final_error() {
    let cassette = Cassette::load("tlas-999-wrap");
    let listing_len = cassette.listing_text(0).len();
    let config = ChunkIteratorConfig {
        max_listing_bytes: listing_len - 1,
        ..fault_config(&cassette, 4)
    };
    let mut iter = ChunkIterator::new(cassette.site(), config, cassette.replay());
    match iter.next() {
        Some(Err(ChunkIterError::Transport {
            attempts, error, ..
        })) => {
            assert_eq!(attempts, 1);
            assert_eq!(error.kind, TransportErrorKind::TooLarge);
        }
        other => panic!("expected TooLarge, got {other:?}"),
    }
    assert!(matches!(iter.next(), Some(Ok(ChunkEvent::Idle { .. }))));
    assert_eq!(iter.stats().listing_bytes, listing_len as u64);
}

#[test]
fn a_chunk_body_is_not_a_listing() {
    let cassette = Cassette::load("tlas-next-volume-bytes");
    let chunk_fetch = cassette
        .requests
        .iter()
        .position(|r| r.kind == FetchKind::Chunk)
        .expect("chunk request");
    let chunk_bytes = cassette.body(
        cassette.requests[chunk_fetch]
            .response
            .as_ref()
            .expect("ok"),
    );
    let transport = Mutating {
        inner: cassette.replay(),
        fetches: 0,
        at: 0,
        mutate: move |_| chunk_bytes.clone(),
    };
    let mut iter = ChunkIterator::new(cassette.site(), cassette.config(), transport);
    assert!(matches!(
        iter.next(),
        Some(Err(ChunkIterError::Listing { .. }))
    ));
    assert!(matches!(iter.next(), Some(Ok(ChunkEvent::Idle { .. }))));
    // The join listing is requested again afterwards, and the replay goes on.
    assert!(iter.next().is_some());
    assert_eq!(
        iter.transport().inner.served[0].url,
        cassette.requests[0].url
    );
}

#[test]
fn chunks_adapter_sleeps_through_idle_and_retry() {
    let cassette = Cassette::load("kmxx-offline");
    let transport = Faulty {
        inner: cassette.replay(),
        fetches: 0,
        faults: VecDeque::from([(
            1,
            TransportError::new(TransportErrorKind::Connect, "injected reset"),
        )]),
    };
    let mut iter = ChunkIterator::new(cassette.site(), fault_config(&cassette, 4), transport);
    let expected_chunks = cassette
        .events
        .iter()
        .filter(|e| e["event"] == "chunk")
        .count();
    let mut sleeps = Vec::new();
    let delivered: Vec<Chunk> = iter
        .chunks(|delay| sleeps.push(delay))
        .take(expected_chunks)
        .map(|chunk| chunk.expect("no final errors"))
        .collect();
    assert_eq!(delivered.len(), expected_chunks);
    assert_eq!(sleeps.len(), 1, "one retry before the chunks: {sleeps:?}");
    assert!(sleeps[0] <= Duration::from_millis(400));
}

// ---------------------------------------------------------------------------
// Async stream
// ---------------------------------------------------------------------------

#[cfg(feature = "async")]
mod stream {
    use std::future::{Ready, ready};
    use std::pin::Pin;
    use std::task::{Context, Poll, Waker};

    use futures_core::Stream;
    use recast_radar_data::realtime::stream::{AsyncChunkTransport, ChunkStream};

    use super::*;

    impl AsyncChunkTransport for Replay {
        type Fetch = Ready<Result<Vec<u8>, TransportError>>;

        fn fetch(&mut self, request: &FetchRequest) -> Self::Fetch {
            ready(ChunkTransport::fetch(self, request))
        }
    }

    /// Answers every other poll with `Pending`, to exercise re-polling.
    struct Slow {
        inner: Replay,
    }

    struct SlowFetch {
        result: Option<Result<Vec<u8>, TransportError>>,
        polled: bool,
    }

    impl std::future::Future for SlowFetch {
        type Output = Result<Vec<u8>, TransportError>;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            if !self.polled {
                self.polled = true;
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            Poll::Ready(self.result.take().expect("polled after completion"))
        }
    }

    impl AsyncChunkTransport for Slow {
        type Fetch = SlowFetch;

        fn fetch(&mut self, request: &FetchRequest) -> Self::Fetch {
            SlowFetch {
                result: Some(ChunkTransport::fetch(&mut self.inner, request)),
                polled: false,
            }
        }
    }

    fn collect<S: Stream<Item = Event> + Unpin>(stream: &mut S, count: usize) -> Vec<Event> {
        let mut cx = Context::from_waker(Waker::noop());
        let mut events = Vec::new();
        let mut pending = 0;
        while events.len() < count {
            match Pin::new(&mut *stream).poll_next(&mut cx) {
                Poll::Ready(Some(event)) => events.push(event),
                Poll::Ready(None) => panic!("the chunk stream never ends"),
                Poll::Pending => {
                    pending += 1;
                    assert!(pending < 1_000_000, "stuck");
                }
            }
        }
        events
    }

    #[test]
    fn stream_replays_every_cassette_like_the_iterator() {
        for name in CASSETTES {
            let cassette = Cassette::load(name);
            let mut stream =
                ChunkStream::new(cassette.site(), cassette.config(), cassette.replay());
            let events = collect(&mut stream, cassette.events.len());
            let summaries: Vec<Value> = events.iter().map(summarize).collect();
            assert_eq!(summaries, cassette.events, "{name}");
            assert!(stream.transport().exhausted(), "{name}");
            assert_eq!(
                &stats_json(&stream.stats()),
                cassette.recorded_stats(),
                "{name}"
            );
        }
    }

    #[test]
    fn stream_handles_pending_fetches() {
        let cassette = Cassette::load("tmco-710-abandoned");
        let transport = Slow {
            inner: cassette.replay(),
        };
        let mut stream = ChunkStream::new(cassette.site(), cassette.config(), transport);
        let events = collect(&mut stream, cassette.events.len());
        let summaries: Vec<Value> = events.iter().map(summarize).collect();
        assert_eq!(summaries, cassette.events);
    }
}

// ---------------------------------------------------------------------------
// HTTP clients over loopback, serving recorded responses
// ---------------------------------------------------------------------------

/// The crate's real HTTP transports (blocking `ReqwestTransport`, feature
/// `net`; async `AsyncReqwestTransport`, feature `async-client`) against a
/// loopback HTTP/1.1 server that answers with a cassette's recorded
/// responses: listing XML and chunk bytes as 200 bodies. A recorded
/// `TooLarge` refusal is answered with the headers S3 sent, a
/// `Content-Length` of the chunk's listed size, and no body.
#[cfg(any(feature = "net", feature = "async-client"))]
mod http {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use recast_radar_data::realtime::iterator::CHUNKS_BUCKET_URL;

    use super::*;

    #[derive(Clone, Debug)]
    pub enum Answer {
        Body(Vec<u8>),
        Status(u16),
        /// Headers announcing this many bytes, then the connection closes.
        HeadersOnly(u64),
    }

    /// A loopback server that expects `script` (path and query, answer) in
    /// order, one connection per request.
    pub struct Server {
        pub base: String,
        pub requests: Arc<Mutex<Vec<String>>>,
        expected: Vec<String>,
    }

    impl Server {
        pub fn start(script: Vec<(String, Answer)>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let base = format!("http://{}", listener.local_addr().expect("addr"));
            let requests = Arc::new(Mutex::new(Vec::new()));
            let log = Arc::clone(&requests);
            let expected = script.iter().map(|(path, _)| path.clone()).collect();
            std::thread::spawn(move || {
                for (_, answer) in script {
                    let Ok((stream, _)) = listener.accept() else {
                        return;
                    };
                    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                    let mut request_line = String::new();
                    reader.read_line(&mut request_line).expect("request line");
                    let mut header = String::new();
                    while reader.read_line(&mut header).expect("header") > 2 {
                        header.clear();
                    }
                    let path = request_line
                        .split(' ')
                        .nth(1)
                        .unwrap_or_default()
                        .to_owned();
                    log.lock().expect("log").push(path);
                    let mut stream = stream;
                    let (status, length, body) = match &answer {
                        Answer::Body(body) => (200, body.len() as u64, body.as_slice()),
                        Answer::Status(status) => (*status, 0, &[][..]),
                        Answer::HeadersOnly(length) => (200, *length, &[][..]),
                    };
                    let head = format!(
                        "HTTP/1.1 {status} Recorded\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(body);
                    let _ = stream.flush();
                }
            });
            Self {
                base,
                requests,
                expected,
            }
        }

        /// Every expected request arrived, in order, and nothing else.
        pub fn assert_served(&self, name: &str) {
            let requests = self.requests.lock().expect("log").clone();
            assert_eq!(requests, self.expected, "{name}: requests over HTTP");
        }
    }

    /// The listed size of `key` in the cassette's recorded listings.
    fn listed_size(cassette: &Cassette, key: &str) -> u64 {
        cassette
            .requests
            .iter()
            .filter_map(|request| match &request.response {
                Ok(RecordedBody::Text(xml)) => Some(xml),
                _ => None,
            })
            .find_map(|xml| {
                let rest = xml.split(&format!("<Key>{key}</Key>")).nth(1)?;
                rest.split("<Size>")
                    .nth(1)?
                    .split("</Size>")
                    .next()?
                    .parse()
                    .ok()
            })
            .unwrap_or_else(|| panic!("{key} not listed"))
    }

    /// Path and query of recorded request `index`, and its recorded answer.
    pub fn recorded(cassette: &Cassette, index: usize) -> (String, Answer) {
        let request = &cassette.requests[index];
        let path = request
            .url
            .strip_prefix(CHUNKS_BUCKET_URL)
            .expect("bucket url")
            .to_owned();
        let answer = match &request.response {
            Ok(body) => Answer::Body(cassette.body(body)),
            Err(error) => match error.kind {
                TransportErrorKind::Status(status) => Answer::Status(status),
                TransportErrorKind::TooLarge => {
                    Answer::HeadersOnly(listed_size(cassette, path.trim_start_matches('/')))
                }
                other => panic!("cannot serve a recorded {other:?}"),
            },
        };
        (path, answer)
    }

    pub fn script(cassette: &Cassette) -> Vec<(String, Answer)> {
        (0..cassette.requests.len())
            .map(|index| recorded(cassette, index))
            .collect()
    }

    pub fn config(cassette: &Cassette, server: &Server) -> ChunkIteratorConfig {
        ChunkIteratorConfig {
            bucket_url: server.base.clone(),
            ..cassette.config()
        }
    }

    /// A summary with the loopback base URL put back to the bucket URL, and
    /// error messages cut after the error kind (the transports word their
    /// detail differently).
    pub fn normalized(event: &Event, base: &str) -> Value {
        let mut summary = summarize(event);
        if let Some(url) = summary["url"].as_str() {
            summary["url"] = json!(url.replace(base, CHUNKS_BUCKET_URL));
        }
        if let Some(message) = summary["message"].as_str() {
            let message = message.replace(base, CHUNKS_BUCKET_URL);
            let cut = message
                .find("attempt(s): ")
                .map(|at| at + "attempt(s): ".len())
                .and_then(|start| {
                    message[start..]
                        .find(':')
                        .map(|colon| message[..start + colon].to_owned())
                })
                .unwrap_or(message);
            summary["message"] = json!(cut);
        }
        summary
    }

    pub fn normalized_recording(cassette: &Cassette) -> Vec<Value> {
        cassette
            .events
            .iter()
            .map(|summary| {
                let mut summary = summary.clone();
                if let Some(message) = summary["message"].as_str() {
                    let cut = message
                        .find("attempt(s): ")
                        .map(|at| at + "attempt(s): ".len())
                        .and_then(|start| {
                            message[start..]
                                .find(':')
                                .map(|colon| message[..start + colon].to_owned())
                        })
                        .unwrap_or_else(|| message.to_owned());
                    summary["message"] = json!(cut);
                }
                summary
            })
            .collect()
    }

    /// Cassettes whose every recorded answer can be served: listings, chunk
    /// bytes and a Content-Length refusal.
    pub const SERVED: &[&str] = &[
        "tlas-next-volume-bytes",
        "tlas-chunk-too-large",
        "tmco-710-abandoned",
    ];

    /// `tlas-next-volume-bytes` with a 404 for the first download of chunk 2,
    /// then the relisting (request 19 again) and the recorded rest.
    pub fn not_found_script(cassette: &Cassette) -> Vec<(String, Answer)> {
        let mut script = script(cassette);
        let (chunk_2, _) = recorded(cassette, 20);
        script.insert(20, recorded(cassette, 19));
        script.insert(20, (chunk_2, Answer::Status(404)));
        script
    }

    #[cfg(feature = "net")]
    #[test]
    fn blocking_https_transport_over_http() {
        use recast_radar_data::realtime::iterator::ReqwestTransport;

        for name in SERVED {
            let cassette = Cassette::load(name);
            let server = Server::start(script(&cassette));
            let transport = ReqwestTransport::try_new().expect("client");
            let mut iter =
                ChunkIterator::new(cassette.site(), config(&cassette, &server), transport);
            let events = take_events(&mut iter, cassette.events.len());
            let summaries: Vec<Value> = events
                .iter()
                .map(|event| normalized(event, &server.base))
                .collect();
            assert_eq!(summaries, normalized_recording(&cassette), "{name}");
            assert_eq!(
                &stats_json(&iter.stats()),
                cassette.recorded_stats(),
                "{name}"
            );
            server.assert_served(name);
        }

        let cassette = Cassette::load("tlas-next-volume-bytes");
        let server = Server::start(not_found_script(&cassette));
        let transport = ReqwestTransport::try_new().expect("client");
        let mut iter = ChunkIterator::new(cassette.site(), config(&cassette, &server), transport);
        let events = take_events(&mut iter, cassette.events.len() + 2);
        assert!(events.iter().any(|event| matches!(
            event,
            Err(ChunkIterError::Transport { error, .. })
                if error.kind == TransportErrorKind::Status(404)
        )));
        let rest: Vec<Value> = without_fault_events(&events)
            .into_iter()
            .map(|mut summary| {
                if let Some(url) = summary["url"].as_str() {
                    summary["url"] = json!(url.replace(&server.base, CHUNKS_BUCKET_URL));
                }
                summary
            })
            .collect();
        assert_eq!(rest, cassette.events);
        server.assert_served("404");
    }

    #[cfg(feature = "async-client")]
    #[test]
    fn async_https_transport_over_http() {
        use std::pin::Pin;

        use futures_core::Stream;
        use recast_radar_data::realtime::stream::{AsyncReqwestTransport, ChunkStream};

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let collect = |stream: &mut ChunkStream<AsyncReqwestTransport>, count: usize| {
            runtime.block_on(async {
                let mut events = Vec::new();
                while events.len() < count {
                    let event =
                        std::future::poll_fn(|cx| Pin::new(&mut *stream).poll_next(cx)).await;
                    events.push(event.expect("the chunk stream never ends"));
                }
                events
            })
        };

        for name in SERVED {
            let cassette = Cassette::load(name);
            let server = Server::start(script(&cassette));
            let transport = AsyncReqwestTransport::new().expect("client");
            let mut stream =
                ChunkStream::new(cassette.site(), config(&cassette, &server), transport);
            let events = collect(&mut stream, cassette.events.len());
            let summaries: Vec<Value> = events
                .iter()
                .map(|event| normalized(event, &server.base))
                .collect();
            assert_eq!(summaries, normalized_recording(&cassette), "{name}");
            assert_eq!(
                &stats_json(&stream.stats()),
                cassette.recorded_stats(),
                "{name}"
            );
            server.assert_served(name);
        }

        let cassette = Cassette::load("tlas-next-volume-bytes");
        let server = Server::start(not_found_script(&cassette));
        let transport = AsyncReqwestTransport::new().expect("client");
        let mut stream = ChunkStream::new(cassette.site(), config(&cassette, &server), transport);
        let events = collect(&mut stream, cassette.events.len() + 2);
        assert!(events.iter().any(|event| matches!(
            event,
            Err(ChunkIterError::Transport { error, .. })
                if error.kind == TransportErrorKind::Status(404)
        )));
        assert_eq!(without_fault_events(&events), cassette.events);
        server.assert_served("404");
    }
}

// ---------------------------------------------------------------------------
// Capture (live; run through tests/fixtures/listings/capture.sh)
// ---------------------------------------------------------------------------

#[cfg(feature = "net")]
struct Recorder {
    inner: recast_radar_data::realtime::iterator::ReqwestTransport,
    lines: Vec<Value>,
    chunk_dir: std::path::PathBuf,
    chunk_files: Vec<String>,
    max_chunk_files: usize,
}

#[cfg(feature = "net")]
impl ChunkTransport for Recorder {
    fn fetch(&mut self, request: &FetchRequest) -> Result<Vec<u8>, TransportError> {
        let seq = self
            .lines
            .iter()
            .filter(|line| line["type"] == "request")
            .count();
        let time = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let result = self.inner.fetch(request);
        let mut line = json!({
            "type": "request",
            "seq": seq,
            "time": time,
            "kind": kind_name(request.kind),
            "url": request.url,
        });
        match &result {
            Ok(body) if request.kind == FetchKind::Chunk => {
                let key = request
                    .url
                    .split(".amazonaws.com/")
                    .nth(1)
                    .expect("object url");
                let file = chunk_file_name(key);
                if !self.chunk_files.contains(&file) {
                    assert!(
                        self.chunk_files.len() < self.max_chunk_files,
                        "capture would write more than {} chunk files; tighten RECAST_CAPTURE_STOP",
                        self.max_chunk_files
                    );
                    fs::create_dir_all(&self.chunk_dir).expect("chunk dir");
                    fs::write(self.chunk_dir.join(&file), body).expect("write chunk");
                    self.chunk_files.push(file.clone());
                }
                line["ok"] = json!({ "file": file, "len": body.len() });
            }
            Ok(body) => {
                let text = String::from_utf8(body.clone()).expect("UTF-8 listing");
                line["ok"] = json!({ "text": text });
            }
            Err(error) => {
                line["err"] = json!({
                    "kind": error_kind_name(error.kind),
                    "message": error.message,
                });
            }
        }
        self.lines.push(line);
        result
    }
}

#[cfg(feature = "net")]
#[test]
#[ignore = "live capture against the real-time bucket; run tests/fixtures/listings/capture.sh"]
fn capture_cassette() {
    use std::env::var;

    let Ok(out) = var("RECAST_CAPTURE_OUT") else {
        eprintln!("RECAST_CAPTURE_OUT not set; nothing to capture");
        return;
    };
    let out = std::path::PathBuf::from(out);
    let env_or = |key: &str, default: &str| var(key).unwrap_or_else(|_| default.to_owned());
    let parse = |key: &str, default: &str| -> u64 {
        env_or(key, default)
            .parse()
            .unwrap_or_else(|err| panic!("{key}: {err}"))
    };
    let site = env_or("RECAST_CAPTURE_SITE", "KTLX");
    let config = ChunkIteratorConfig {
        join: join_from_name(&env_or("RECAST_CAPTURE_JOIN", "current")),
        download: parse("RECAST_CAPTURE_DOWNLOAD", "0") != 0,
        poll_interval: Duration::from_millis(parse("RECAST_CAPTURE_POLL_MS", "5000")),
        stall_polls: parse("RECAST_CAPTURE_STALL_POLLS", "12") as u32,
        probe_ahead: parse("RECAST_CAPTURE_PROBE_AHEAD", "2") as u16,
        rediscover_every: parse("RECAST_CAPTURE_REDISCOVER_EVERY", "5") as u32,
        jitter_seed: Some(parse("RECAST_CAPTURE_SEED", "1")),
        max_chunk_bytes: parse(
            "RECAST_CAPTURE_MAX_CHUNK_BYTES",
            &ChunkIteratorConfig::default().max_chunk_bytes.to_string(),
        ) as usize,
        max_chunk_failures: parse(
            "RECAST_CAPTURE_MAX_CHUNK_FAILURES",
            &ChunkIteratorConfig::default()
                .max_chunk_failures
                .to_string(),
        ) as u32,
        ..ChunkIteratorConfig::default()
    };
    let stop = env_or("RECAST_CAPTURE_STOP", "events=20");
    let stop: Vec<(String, u64)> = stop
        .split(',')
        .map(|term| {
            let (key, value) = term.split_once('=').expect("stop term key=value");
            (key.to_owned(), value.parse().expect("stop value"))
        })
        .collect();
    let deadline =
        std::time::Instant::now() + Duration::from_secs(60 * parse("RECAST_CAPTURE_MINUTES", "15"));

    let recorder = Recorder {
        inner: recast_radar_data::realtime::iterator::ReqwestTransport::new(),
        lines: vec![json!({
            "type": "header",
            "format": 1,
            "site": site,
            "started": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "stop": env_or("RECAST_CAPTURE_STOP", "events=20"),
            "note": env_or("RECAST_CAPTURE_NOTE", ""),
            "config": config_json(&config),
        })],
        chunk_dir: out.parent().expect("output dir").join("chunks"),
        chunk_files: Vec::new(),
        max_chunk_files: 3,
    };
    let mut iter = ChunkIterator::new(&site, config, recorder);
    let (mut events, mut chunk_count, mut completed, mut idles, mut after_rollover) =
        (0, 0, 0, 0, 0);
    let (mut abandoned, mut errors) = (0, 0);
    let mut first_volume = None;
    let reason = loop {
        let event = iter.next().expect("never ends");
        let summary = summarize(&event);
        eprintln!("{summary}");
        iter.transport_mut().lines.push(json!({
            "type": "event",
            "time": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "summary": summary,
        }));
        events += 1;
        match &event {
            Ok(ChunkEvent::Chunk(chunk)) => {
                chunk_count += 1;
                let volume = *first_volume.get_or_insert(chunk.info.volume_id);
                if chunk.info.volume_id != volume {
                    after_rollover += 1;
                }
                if chunk.info.chunk_type == RealtimeChunkType::End {
                    completed += 1;
                }
            }
            Ok(ChunkEvent::Idle { poll_after }) => {
                idles += 1;
                std::thread::sleep(*poll_after);
            }
            Ok(ChunkEvent::Retry { after, .. }) => std::thread::sleep(*after),
            Ok(ChunkEvent::VolumeAbandoned { .. }) => abandoned += 1,
            Err(_) => errors += 1,
            _ => {}
        }
        let reached = stop.iter().find(|(key, limit)| {
            let value = match key.as_str() {
                "events" => events,
                "chunks" => chunk_count,
                "completed" => completed,
                "idles" => idles,
                "chunks_after_rollover" => after_rollover,
                "abandoned" => abandoned,
                "errors" => errors,
                other => panic!("unknown stop condition {other}"),
            };
            value >= *limit
        });
        if let Some((key, limit)) = reached {
            break format!("{key}={limit}");
        }
        assert!(std::time::Instant::now() < deadline, "capture timed out");
    };
    let stats = iter.stats();
    let mut recorder = iter.into_transport();
    recorder.lines.push(json!({
        "type": "end",
        "time": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "reason": reason,
        "stats": stats_json(&stats),
    }));
    let mut text = String::new();
    for line in &recorder.lines {
        text.push_str(&serde_json::to_string(line).expect("json"));
        text.push('\n');
    }
    fs::write(&out, text).expect("write cassette");
    eprintln!("wrote {} ({} lines)", out.display(), recorder.lines.len());
}
