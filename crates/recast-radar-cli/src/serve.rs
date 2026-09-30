//! `serve`: a small HTTP/1.1 file server for a directory, such as a
//! GR2Analyst polling directory.
//!
//! GET and HEAD only, one request per connection (`Connection: close`),
//! directory listings in the style of nginx's autoindex (what many polling
//! servers run), no range requests. Paths are confined to the
//! served directory: `..` segments, backslashes, drive prefixes and names
//! starting with a dot are refused, and the resolved path must stay under
//! the directory after following links. Built on `std::net`; no unsafe code.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Utc};

use crate::{CliError, ServeArgs};

/// Longest request head read (request line and headers).
const MAX_REQUEST_HEAD: usize = 16 * 1024;
/// How long a connection may go without sending anything.
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a connection may take to send its whole request head, so a
/// client sending one byte at a time cannot hold a connection slot.
const HEAD_DEADLINE: Duration = Duration::from_secs(20);
/// How long a response may take to be accepted by the client.
const WRITE_TIMEOUT: Duration = Duration::from_secs(60);

/// Settings of a running server.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ServeConfig {
    /// The served directory (canonical path).
    pub root: PathBuf,
    /// Connections handled at once.
    pub max_connections: usize,
    /// Stop after this many connections, when set.
    pub max_requests: Option<u64>,
    /// Log each request to standard error.
    pub log: bool,
    /// How long a connection may take to send its request head (request
    /// line and headers); it is answered 408 after that.
    pub head_deadline: Duration,
}

impl ServeConfig {
    /// Serve `root` (which must be a directory) with at most
    /// `max_connections` connections at a time.
    pub fn new(root: &Path, max_connections: usize) -> io::Result<Self> {
        let root = fs::canonicalize(root)?;
        if !root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a directory", root.display()),
            ));
        }
        Ok(Self {
            root,
            max_connections: max_connections.max(1),
            max_requests: None,
            log: true,
            head_deadline: HEAD_DEADLINE,
        })
    }
}

pub(crate) fn run_command(args: &ServeArgs, out: &mut dyn Write) -> Result<(), CliError> {
    let mut config = ServeConfig::new(&args.dir, args.max_connections as usize)
        .map_err(|err| CliError::io(&args.dir, err))?;
    config.max_requests = args.max_requests;
    let listener = TcpListener::bind(&args.bind)
        .map_err(|err| CliError::Failed(format!("cannot listen on {}: {err}", args.bind)))?;
    let address = listener.local_addr()?;
    writeln!(
        out,
        "serving {} at http://{address}/ (Ctrl+C to stop)",
        display_path(&config.root)
    )?;
    out.flush()?;
    serve(listener, &config)?;
    Ok(())
}

/// `path` for people to read: without the `\\?\` prefix of Windows
/// canonical paths (`\\?\UNC\server\share` is shown as `\\server\share`).
fn display_path(path: &Path) -> String {
    let text = path.display().to_string();
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else if let Some(local) = text.strip_prefix(r"\\?\") {
        local.to_owned()
    } else {
        text
    }
}

/// Accept connections on `listener` and answer them until
/// [`ServeConfig::max_requests`] connections have been handled (forever
/// when it is `None`).
pub fn serve(listener: TcpListener, config: &ServeConfig) -> io::Result<()> {
    let slots = Arc::new((Mutex::new(0usize), Condvar::new()));
    let mut handled = 0u64;
    let mut workers = Vec::new();
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(err) => {
                if config.log {
                    eprintln!("serve: accept failed: {err}");
                }
                continue;
            }
        };
        {
            let (lock, ready) = &*slots;
            let mut active = lock.lock().unwrap_or_else(|p| p.into_inner());
            while *active >= config.max_connections {
                active = ready.wait(active).unwrap_or_else(|p| p.into_inner());
            }
            *active += 1;
        }
        let slots = Arc::clone(&slots);
        let root = config.root.clone();
        let log = config.log;
        let head_deadline = config.head_deadline;
        workers.push(thread::spawn(move || {
            if let Err(err) = handle_connection(stream, &root, log, head_deadline)
                && log
            {
                eprintln!("serve: {err}");
            }
            let (lock, ready) = &*slots;
            let mut active = lock.lock().unwrap_or_else(|p| p.into_inner());
            *active -= 1;
            ready.notify_one();
        }));
        workers.retain(|worker| !worker.is_finished());
        handled += 1;
        if config.max_requests.is_some_and(|max| handled >= max) {
            break;
        }
    }
    for worker in workers {
        let _ = worker.join();
    }
    Ok(())
}

fn handle_connection(
    stream: TcpStream,
    root: &Path,
    log: bool,
    head_deadline: Duration,
) -> io::Result<()> {
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let peer = stream
        .peer_addr()
        .map(|addr| addr.to_string())
        .unwrap_or_default();
    let mut reader = BufReader::new(DeadlineReader {
        stream: stream.try_clone()?,
        deadline: Instant::now() + head_deadline,
    });
    let response = match read_request_head(&mut reader) {
        Ok(head) => {
            let (method, target) = parse_request_line(&head);
            let response = respond(root, method, target);
            if log {
                eprintln!(
                    "{peer} {method} {target} {} {}",
                    response.status,
                    response.body.len()
                );
            }
            response
        }
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ) =>
        {
            Response::error(408, "the request head was not sent in time")
        }
        Err(err) => Response::error(400, &format!("bad request: {err}")),
    };
    let mut stream = stream;
    response.send(&mut stream)
}

/// The status `serve` answers a request with, when the request's bytes are
/// `request` and the served directory is `root` (a canonical path, as
/// [`ServeConfig::root`]): the request head is read, parsed and resolved
/// under `root` exactly as for a connection, but nothing is sent. For tests
/// and the `serve-request` fuzz target.
pub fn status_for_request(root: &Path, request: &[u8]) -> u16 {
    let mut reader = BufReader::new(request);
    match read_request_head(&mut reader) {
        Ok(head) => {
            let (method, target) = parse_request_line(&head);
            respond(root, method, target).status
        }
        Err(_) => 400,
    }
}

/// A connection read with a deadline for the whole request head: each read
/// waits at most [`READ_TIMEOUT`] and never past the deadline.
struct DeadlineReader {
    stream: TcpStream,
    deadline: Instant,
}

impl Read for DeadlineReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the request head took too long",
            ));
        }
        self.stream.set_read_timeout(Some(left.min(READ_TIMEOUT)))?;
        self.stream.read(buf)
    }
}

/// Read a request head (request line and headers, up to the blank line) of
/// at most 16 KiB.
fn read_request_head(reader: &mut impl BufRead) -> io::Result<String> {
    let mut head = Vec::new();
    loop {
        let mut line = Vec::new();
        let read = reader
            .by_ref()
            .take((MAX_REQUEST_HEAD - head.len()) as u64)
            .read_until(b'\n', &mut line)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before the request ended",
            ));
        }
        let blank = line == b"\r\n" || line == b"\n";
        head.extend_from_slice(&line);
        if blank {
            break;
        }
        if head.len() >= MAX_REQUEST_HEAD {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request head too long",
            ));
        }
    }
    String::from_utf8(head)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "request head is not UTF-8"))
}

fn parse_request_line(head: &str) -> (&str, &str) {
    let line = head.lines().next().unwrap_or_default();
    let mut parts = line.split_whitespace();
    (parts.next().unwrap_or(""), parts.next().unwrap_or(""))
}

/// A response body.
#[derive(Debug)]
enum Body {
    Bytes(Vec<u8>),
    File { path: PathBuf, len: u64 },
}

impl Body {
    fn len(&self) -> u64 {
        match self {
            Self::Bytes(bytes) => bytes.len() as u64,
            Self::File { len, .. } => *len,
        }
    }
}

/// A response: status, headers and body. `head_only` drops the body (HEAD).
#[derive(Debug)]
struct Response {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: Body,
    head_only: bool,
}

impl Response {
    fn error(status: u16, message: &str) -> Self {
        Self {
            status,
            headers: vec![("Content-Type", "text/plain; charset=utf-8".to_owned())],
            body: Body::Bytes(format!("{status} {}: {message}\n", reason(status)).into_bytes()),
            head_only: false,
        }
    }

    fn send(self, stream: &mut TcpStream) -> io::Result<()> {
        let mut head = format!(
            "HTTP/1.1 {} {}\r\nServer: recast-radar\r\nConnection: close\r\nCache-Control: no-cache\r\nContent-Length: {}\r\n",
            self.status,
            reason(self.status),
            self.body.len()
        );
        for (name, value) in &self.headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes())?;
        if !self.head_only {
            match self.body {
                Body::Bytes(bytes) => stream.write_all(&bytes)?,
                Body::File { path, len } => {
                    let copied = io::copy(&mut File::open(&path)?.take(len), stream)?;
                    if copied != len {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            format!("{} shrank while being sent", path.display()),
                        ));
                    }
                }
            }
        }
        stream.flush()
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        301 => "Moved Permanently",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        _ => "Internal Server Error",
    }
}

fn content_type(path: &Path) -> &'static str {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let extension = name.rsplit('.').next().unwrap_or("");
    match extension {
        "list" | "cfg" | "txt" | "pal" => "text/plain; charset=utf-8",
        "html" | "htm" => "text/html; charset=utf-8",
        "json" => "application/json",
        "png" => "image/png",
        "gz" => "application/gzip",
        _ => "application/octet-stream",
    }
}

fn http_date(time: SystemTime) -> String {
    DateTime::<Utc>::from(time)
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string()
}

/// Decode `%XX` escapes; `None` for a malformed escape.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = text.get(index + 1..index + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The path under `root` that `target` names, or the status to refuse it
/// with.
fn resolve(root: &Path, target: &str) -> Result<PathBuf, u16> {
    let path = target.split(['?', '#']).next().unwrap_or("");
    if !path.starts_with('/') {
        return Err(400);
    }
    let mut resolved = root.to_path_buf();
    for raw in path.split('/') {
        let segment = percent_decode(raw).ok_or(400u16)?;
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." || segment.starts_with('.') || segment.contains(['/', '\\', ':', '\0']) {
            return Err(403);
        }
        resolved.push(segment);
    }
    let canonical = fs::canonicalize(&resolved).map_err(|_| 404u16)?;
    if !canonical.starts_with(root) {
        return Err(403);
    }
    Ok(canonical)
}

fn respond(root: &Path, method: &str, target: &str) -> Response {
    let head_only = match method {
        "GET" => false,
        "HEAD" => true,
        _ => {
            let mut response = Response::error(405, "only GET and HEAD are served");
            response.headers.push(("Allow", "GET, HEAD".to_owned()));
            return response;
        }
    };
    let mut response = match resolve(root, target) {
        Err(status) => Response::error(status, target),
        Ok(path) if path.is_dir() => {
            let clean = target.split(['?', '#']).next().unwrap_or("/");
            if clean.ends_with('/') {
                match directory_listing(&path, clean) {
                    Ok(html) => Response {
                        status: 200,
                        headers: vec![("Content-Type", "text/html; charset=utf-8".to_owned())],
                        body: Body::Bytes(html.into_bytes()),
                        head_only: false,
                    },
                    Err(err) => Response::error(500, &err.to_string()),
                }
            } else {
                let mut response = Response::error(301, "moved");
                response.headers.push(("Location", format!("{clean}/")));
                response
            }
        }
        Ok(path) => match fs::metadata(&path) {
            Ok(metadata) => {
                let mut headers = vec![("Content-Type", content_type(&path).to_owned())];
                if let Ok(modified) = metadata.modified() {
                    headers.push(("Last-Modified", http_date(modified)));
                }
                Response {
                    status: 200,
                    headers,
                    body: Body::File {
                        path,
                        len: metadata.len(),
                    },
                    head_only: false,
                }
            }
            Err(err) => Response::error(404, &err.to_string()),
        },
    };
    response.head_only = head_only;
    response
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn href_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// An nginx-autoindex-style listing: directories first, then files, each
/// sorted without regard to case; dot files left out.
fn directory_listing(dir: &Path, url_path: &str) -> io::Result<String> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            dirs.push((name, metadata));
        } else {
            files.push((name, metadata));
        }
    }
    dirs.sort_by_key(|(name, _)| name.to_lowercase());
    files.sort_by_key(|(name, _)| name.to_lowercase());
    let title = html_escape(url_path);
    let mut html = format!(
        "<html>\r\n<head><title>Index of {title}</title></head>\r\n<body>\r\n<h1>Index of {title}</h1><hr><pre><a href=\"../\">../</a>\r\n"
    );
    for (name, metadata) in dirs.iter().chain(files.iter()) {
        let slash = if metadata.is_dir() { "/" } else { "" };
        let modified = metadata
            .modified()
            .map(|time| {
                DateTime::<Utc>::from(time)
                    .format("%d-%b-%Y %H:%M")
                    .to_string()
            })
            .unwrap_or_default();
        let size = if metadata.is_dir() {
            "-".to_owned()
        } else {
            metadata.len().to_string()
        };
        html.push_str(&format!(
            "<a href=\"{}{slash}\">{}{slash}</a> {modified} {size}\r\n",
            href_escape(name),
            html_escape(name)
        ));
    }
    html.push_str("</pre><hr></body>\r\n</html>\r\n");
    Ok(html)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// North Dakota SWC's KXWA `dir.list`, captured 2026-09-25.
    const LISTING: &str = "polling-ndswc-kxwa-dir-list-20260925";

    /// A served directory holding the real North Dakota SWC KXWA `dir.list`
    /// capture, or `None` when the capture (not redistributed) is not in the
    /// testdata cache.
    fn served_dir(name: &str) -> Option<PathBuf> {
        let listing = recast_radar_testdata::path_if_available(LISTING)?;
        let dir =
            std::env::temp_dir().join(format!("recast-radar-serve-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let site = dir.join("KXWA");
        fs::create_dir_all(&site).unwrap();
        fs::copy(listing, site.join("dir.list")).unwrap();
        Some(fs::canonicalize(dir).unwrap())
    }

    fn body(response: &Response) -> Vec<u8> {
        match &response.body {
            Body::Bytes(bytes) => bytes.clone(),
            Body::File { path, .. } => fs::read(path).unwrap(),
        }
    }

    #[test]
    fn banner_paths_have_no_verbatim_prefix() {
        assert_eq!(
            display_path(Path::new(r"\\?\C:\radar\polling")),
            r"C:\radar\polling"
        );
        assert_eq!(
            display_path(Path::new(r"\\?\UNC\server\share\polling")),
            r"\\server\share\polling"
        );
        assert_eq!(display_path(Path::new("/srv/polling")), "/srv/polling");
    }

    #[test]
    fn files_and_listings_are_served() {
        let Some(root) = served_dir("files") else {
            return;
        };
        let response = respond(&root, "GET", "/KXWA/dir.list");
        assert_eq!(response.status, 200);
        let listing = recast_radar_testdata::bytes(LISTING).unwrap();
        assert_eq!(body(&response), listing);
        assert!(
            response
                .headers
                .iter()
                .any(|(name, value)| *name == "Content-Type" && value.starts_with("text/plain"))
        );

        let index = respond(&root, "GET", "/");
        assert_eq!(index.status, 200);
        let html = String::from_utf8(body(&index)).unwrap();
        assert!(html.contains("<a href=\"KXWA/\">KXWA/</a>"), "{html}");

        let redirect = respond(&root, "GET", "/KXWA");
        assert_eq!(redirect.status, 301);
        let head = respond(&root, "HEAD", "/KXWA/dir.list");
        assert!(head.head_only);
        assert_eq!(head.body.len(), listing.len() as u64);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn paths_outside_the_directory_are_refused() {
        let Some(root) = served_dir("escape") else {
            return;
        };
        for target in [
            "/../Cargo.toml",
            "/KXWA/../../x",
            "/%2e%2e/x",
            "/KXWA%2F..%2F..%2Fx",
            "/.hidden",
            "/C:/Windows",
            "/KXWA\\..\\..\\x",
        ] {
            let status = respond(&root, "GET", target).status;
            assert!(matches!(status, 400 | 403 | 404), "{target}: {status}");
            assert_ne!(status, 200, "{target}");
        }
        assert_eq!(respond(&root, "GET", "/missing").status, 404);
        assert_eq!(respond(&root, "POST", "/KXWA/dir.list").status, 405);
        assert_eq!(respond(&root, "GET", "relative").status, 400);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn real_client_requests_are_answered() {
        let Some(root) = served_dir("clients") else {
            return;
        };
        for id in [
            "http-request-head-curl-8.21.0",
            "http-request-head-python-urllib-3.13",
            "http-request-head-recast-radar-fetch",
        ] {
            let head = recast_radar_testdata::bytes(id).unwrap();
            assert_eq!(status_for_request(&root, &head), 200, "{id}");
            // Cut anywhere before the blank line, the head is incomplete.
            assert_eq!(
                status_for_request(&root, &head[..head.len() - 2]),
                400,
                "{id}"
            );
        }
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_slow_request_head_is_cut_off_at_the_deadline() {
        let Some(root) = served_dir("slow") else {
            return;
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let mut config = ServeConfig::new(&root, 1).unwrap();
        config.max_requests = Some(1);
        config.log = false;
        config.head_deadline = Duration::from_millis(400);
        let server = thread::spawn(move || serve(listener, &config));

        let start = Instant::now();
        let mut client = TcpStream::connect(address).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        // One byte every 50 ms, each well inside the per-read timeout, and
        // then nothing: the head is still unfinished at the deadline. (No byte
        // is sent after it, so the server closes with nothing left unread.)
        for byte in b"GET /BJ" {
            client.write_all(&[*byte]).unwrap();
            thread::sleep(Duration::from_millis(50));
        }
        let mut response = String::new();
        let _ = client.read_to_string(&mut response);
        assert!(response.starts_with("HTTP/1.1 408"), "{response}");
        assert!(start.elapsed() < Duration::from_secs(5));
        server.join().unwrap().unwrap();
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn percent_escapes_decode_and_malformed_ones_are_rejected() {
        assert_eq!(percent_decode("a%20b").as_deref(), Some("a b"));
        assert_eq!(percent_decode("%zz"), None);
        assert_eq!(percent_decode("%4"), None);
    }
}
