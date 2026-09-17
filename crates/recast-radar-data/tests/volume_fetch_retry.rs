//! `fetch_volume_bytes_with_retry` over a loopback HTTP server that serves a
//! real chunk (`TLAS/3/20260917-015242-003-I`, recorded by the iterator
//! cassettes): the first response is cut off halfway through the body, the
//! second is complete.
#![cfg(feature = "net")]
// Tests may unwrap and expect (spec section 2).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use recast_radar_data::realtime::retry::{Jitter, RetryPolicy};
use recast_radar_data::{VOLUME_FETCH_RETRY, fetch_volume_bytes_with_retry};

const CHUNK: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/listings/chunks/TLAS-3-20260917-015242-003-I"
);

/// Serves `responses` connections in order: each writes the full headers
/// and the first `body_bytes` bytes of `body`, then closes.
fn serve(body: Vec<u8>, responses: Vec<usize>) -> (String, Arc<Mutex<usize>>) {
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
    (url, served)
}

#[test]
fn truncated_volume_body_is_retried_with_the_callers_policy_and_sleep() {
    let body = std::fs::read(Path::new(CHUNK)).expect("chunk fixture");
    assert_eq!(body.len(), 24018);
    let (url, served) = serve(body.clone(), vec![body.len() / 2, body.len()]);
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
    let body = std::fs::read(Path::new(CHUNK)).expect("chunk fixture");
    let (url, served) = serve(body.clone(), vec![body.len() / 2]);
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
