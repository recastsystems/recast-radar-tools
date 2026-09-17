//! `fetch_volume_bytes_with_retry` over a loopback HTTP server that serves a
//! real chunk (`TLAS/3/20260917-015242-003-I`, recorded by the iterator
//! cassettes): the first response is cut off halfway through the body, the
//! second is complete.
#![cfg(feature = "net")]
// Tests may unwrap and expect (spec section 2).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use recast_radar_data::realtime::retry::{Jitter, RetryPolicy};
use recast_radar_data::{VOLUME_FETCH_RETRY, fetch_volume_bytes_with_retry};

/// The 24,018-byte TLAS chunk the `tlas-next-volume-bytes` cassette
/// downloaded (`testdata/level2/manifest.toml`).
const CHUNK: &str = "l2chunk-tlas-3-20260917-015242-003-i";

/// Serves the real chunk [`CHUNK`] over `responses` connections in order:
/// each writes the full headers and the first `body_bytes(chunk length)`
/// bytes of the chunk, then closes. Returns the URL, the count of served
/// connections and the chunk bytes.
fn serve(responses: Vec<fn(usize) -> usize>) -> (String, Arc<Mutex<usize>>, Vec<u8>) {
    let body = recast_radar_testdata::bytes(CHUNK).expect("chunk fixture");
    assert_eq!(body.len(), 24018);
    let responses: Vec<usize> = responses.iter().map(|part| part(body.len())).collect();
    let served_body = body.clone();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!(
        "http://{}/TLAS/3/20260917-015242-003-I",
        listener.local_addr().expect("addr")
    );
    let served = Arc::new(Mutex::new(0));
    let counter = Arc::clone(&served);
    thread::spawn(move || {
        for body_bytes in responses {
            let (stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut line = String::new();
            while reader.read_line(&mut line).expect("request") > 2 {
                line.clear();
            }
            let mut stream = stream;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: binary/octet-stream\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).expect("head");
            stream.write_all(&body[..body_bytes]).expect("body");
            stream.flush().expect("flush");
            *counter.lock().expect("lock") += 1;
        }
    });
    (url, served, served_body)
}

#[test]
fn truncated_volume_body_is_retried_with_the_callers_policy_and_sleep() {
    let (url, served, body) = serve(vec![|len| len / 2, |len| len]);
    let mut slept = Vec::new();
    let bytes = fetch_volume_bytes_with_retry(&url, &VOLUME_FETCH_RETRY, |delay| slept.push(delay))
        .expect("second attempt succeeds");
    assert_eq!(bytes, body);
    assert_eq!(*served.lock().expect("lock"), 2);
    // The default schedule's one delay, handed to the caller instead of
    // sleeping inside the library.
    assert_eq!(slept, [Duration::from_secs(2)]);
}

#[test]
fn a_policy_without_retries_returns_the_body_error() {
    let (url, served, _body) = serve(vec![|len| len / 2]);
    let policy = RetryPolicy {
        jitter: Jitter::None,
        ..RetryPolicy::no_retry()
    };
    let result = fetch_volume_bytes_with_retry(&url, &policy, |delay| {
        panic!("no retry allowed, asked to sleep {delay:?}")
    });
    assert!(result.is_err(), "truncated body accepted");
    assert_eq!(*served.lock().expect("lock"), 1);
}
