//! Captions read from the browser's DOM instead of from pixels.
//!
//! Netflix and YouTube both render subtitles as ordinary HTML rather than
//! burning them into the video, so the text is sitting there exactly as the
//! author wrote it. Taking it from the DOM gives perfect text with no OCR, no
//! Screen Recording permission, and nothing for macOS to re-prompt about.
//!
//! The cost is reach: this only works on sites a content script runs on. The
//! screen-capture source still covers everything else, including the Netflix
//! desktop app, local players, and anything outside a browser. The two are
//! alternatives, not replacements.
//!
//! A browser extension does the watching and posts each caption here. This
//! module is just the receiving end: a loopback HTTP listener that turns those
//! posts into the same `(text, when)` pairs the OCR path produces, so
//! everything downstream is shared.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Default loopback port. Picked from the IANA dynamic range to avoid the
/// usual development ports.
pub const DEFAULT_PORT: u16 = 53_117;

/// How long a single client is allowed to take. A caption post is a few hundred
/// bytes over loopback, so anything slower is a stuck or hostile connection and
/// must not be allowed to wedge the listener.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(2);

/// Refuse to read an unreasonable body. Nothing legitimate comes close.
const MAX_BODY: usize = 16 * 1024;

#[derive(Debug, Clone)]
pub struct Caption {
    pub text: String,
    pub seen_at: Instant,
}

#[derive(Default)]
pub struct Stats {
    pub accepted: AtomicU64,
    pub rejected: AtomicU64,
}

/// A running loopback listener. Dropping it stops the server.
pub struct CaptionServer {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub port: u16,
    pub stats: Arc<Stats>,
}

impl CaptionServer {
    /// Begin listening on loopback for captions from the extension.
    ///
    /// `token` is a shared secret written into the extension at setup time. It
    /// is not protecting anything precious, but without it any page the user
    /// visits could post a "simple" cross-origin request here and make the app
    /// say whatever it liked, which for a tool a blind person relies on to know
    /// what is happening is worse than it sounds.
    pub fn start(port: u16, token: String) -> std::io::Result<(Self, Receiver<Caption>)> {
        // Loopback only. This must never be reachable from the network.
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let listener = TcpListener::bind(addr).map_err(|e| {
            // "Address already in use" on its own sends people hunting for a
            // conflicting service, when it is nearly always a second copy of
            // this app still running.
            if e.kind() == std::io::ErrorKind::AddrInUse {
                std::io::Error::new(
                    e.kind(),
                    format!(
                        "port {port} is already in use, most likely by another copy of \
                         ReadSubtitles that is still running"
                    ),
                )
            } else {
                e
            }
        })?;
        listener.set_nonblocking(true)?;
        // Port 0 asks the OS to choose, so report what was actually bound
        // rather than what was asked for.
        let port = listener.local_addr()?.port();

        let (tx, rx) = channel::<Caption>();
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Stats::default());

        let thread_stop = stop.clone();
        let thread_stats = stats.clone();
        let thread = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        handle(stream, &token, &tx, &thread_stats);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        // Polling rather than blocking, so stopping does not
                        // need a self-connection to wake the accept call.
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
        });

        Ok((
            Self {
                stop,
                thread: Some(thread),
                port,
                stats,
            },
            rx,
        ))
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}

impl Drop for CaptionServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn handle(mut stream: TcpStream, token: &str, tx: &Sender<Caption>, stats: &Stats) {
    let seen_at = Instant::now();

    // The listener polls rather than blocking, so that stopping does not need a
    // self-connection to wake it. On macOS and the BSDs an accepted socket
    // inherits that non-blocking flag, and the inherited flag is a trap: the
    // first read returns WouldBlock whenever the client's bytes have not landed
    // yet, read_request reads that as a malformed request, and a perfectly good
    // caption comes back 400 and is never spoken. It survives light testing
    // because over loopback the data usually arrives first; it shows up on a
    // loaded machine, which is exactly when someone is watching a film.
    let _ = stream.set_nonblocking(false);

    let _ = stream.set_read_timeout(Some(CLIENT_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CLIENT_TIMEOUT));

    let Some(request) = read_request(&mut stream) else {
        stats.rejected.fetch_add(1, Ordering::Relaxed);
        let _ = respond(&mut stream, 400, "bad request");
        return;
    };

    // The extension sends a preflight before its first post.
    if request.method == "OPTIONS" {
        let _ = respond(&mut stream, 204, "");
        return;
    }

    if request.token.as_deref() != Some(token) {
        stats.rejected.fetch_add(1, Ordering::Relaxed);
        let _ = respond(&mut stream, 403, "forbidden");
        return;
    }

    match request.path.as_str() {
        // Lets the extension confirm the app is listening before it starts
        // posting, so a misconfigured setup fails visibly rather than silently.
        "/health" => {
            let _ = respond(&mut stream, 200, "ok");
        }
        "/cue" => {
            let text = json_string_field(&request.body, "text").unwrap_or_default();
            let text = text.trim().to_string();
            if text.is_empty() {
                // An empty cue means the caption cleared. That is normal and
                // carries no text to speak, so acknowledge and drop it.
                let _ = respond(&mut stream, 204, "");
                return;
            }
            stats.accepted.fetch_add(1, Ordering::Relaxed);
            let _ = tx.send(Caption { text, seen_at });
            let _ = respond(&mut stream, 204, "");
        }
        _ => {
            let _ = respond(&mut stream, 404, "not found");
        }
    }
}

struct Request {
    method: String,
    path: String,
    token: Option<String>,
    body: String,
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream);

    let mut start = String::new();
    reader.read_line(&mut start).ok()?;
    let mut parts = start.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

    let mut content_length = 0usize;
    let mut token = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let value = value.trim();
            match name.to_ascii_lowercase().as_str() {
                "content-length" => content_length = value.parse().unwrap_or(0),
                "x-readsubtitles-token" => token = Some(value.to_string()),
                _ => {}
            }
        }
    }

    let mut body = String::new();
    if content_length > 0 {
        let capped = content_length.min(MAX_BODY);
        let mut buf = vec![0u8; capped];
        reader.read_exact(&mut buf).ok()?;
        body = String::from_utf8_lossy(&buf).into_owned();
    }

    Some(Request {
        method,
        path,
        token,
        body,
    })
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        _ => "Not Found",
    };
    // The extension posts from its service worker with host permissions, so
    // these headers are belt and braces rather than load-bearing.
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Headers: content-type, x-readsubtitles-token\r\n\
         Access-Control-Allow-Methods: POST, GET, OPTIONS\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

/// Read one string field out of a flat JSON object.
///
/// The payload is one field this crate defines and the extension produces, so a
/// full JSON parser would be a dependency bought for nothing. Anything it
/// cannot read is treated as absent, which the caller handles as an empty cue.
#[must_use]
pub fn json_string_field(body: &str, field: &str) -> Option<String> {
    let key = format!("\"{field}\"");
    let after_key = &body[body.find(&key)? + key.len()..];
    let after_colon = &after_key[after_key.find(':')? + 1..];
    let start = after_colon.find('"')? + 1;
    let mut out = String::new();
    let mut chars = after_colon[start..].chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'u' => {
                    // \uXXXX: take the four hex digits and rebuild the char.
                    let hex: String = chars.by_ref().take(4).collect();
                    let code = u32::from_str_radix(&hex, 16).ok()?;
                    out.push(char::from_u32(code)?);
                }
                other => out.push(other),
            },
            _ => out.push(c),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_caption_is_read_out_of_the_payload() {
        let body = r#"{"text":"Yeah. It was a six-hour surgery."}"#;
        assert_eq!(
            json_string_field(body, "text").as_deref(),
            Some("Yeah. It was a six-hour surgery.")
        );
    }

    #[test]
    fn a_two_line_caption_keeps_its_line_break() {
        let body = r#"{"text":"I don't know what\nyou're talking about."}"#;
        assert_eq!(
            json_string_field(body, "text").as_deref(),
            Some("I don't know what\nyou're talking about.")
        );
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_caption_early() {
        let body = r#"{"text":"He said \"no\" twice."}"#;
        assert_eq!(
            json_string_field(body, "text").as_deref(),
            Some(r#"He said "no" twice."#)
        );
    }

    #[test]
    fn a_unicode_escape_is_decoded() {
        // Subtitles are full of typographic quotes and dashes, which browsers
        // escape on the way out.
        let body = r#"{"text":"café — open"}"#;
        assert_eq!(
            json_string_field(body, "text").as_deref(),
            Some("café — open")
        );
    }

    #[test]
    fn a_missing_field_is_absent_rather_than_an_error() {
        assert_eq!(json_string_field(r#"{"other":"x"}"#, "text"), None);
        assert_eq!(json_string_field("not json at all", "text"), None);
        assert_eq!(json_string_field("", "text"), None);
    }

    #[test]
    fn a_truncated_payload_does_not_panic() {
        assert_eq!(json_string_field(r#"{"text":"unterminated"#, "text"), None);
        assert_eq!(json_string_field(r#"{"text":"\u00"#, "text"), None);
    }

    use std::net::TcpStream;

    /// Send one request to the test server and return its status line.
    fn post(port: u16, path: &str, token: &str, body: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let request = format!(
            "POST {path} HTTP/1.1\r\nHost: localhost\r\n\
             X-ReadSubtitles-Token: {token}\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        s.write_all(request.as_bytes()).unwrap();
        s.flush().unwrap();
        let mut reply = String::new();
        s.read_to_string(&mut reply).ok();
        reply.lines().next().unwrap_or_default().to_string()
    }

    #[test]
    fn a_posted_caption_arrives_as_a_cue() {
        // Port 0 asks the OS for a free port, so tests never collide with each
        // other or with a running app.
        let (server, rx) = CaptionServer::start(0, "secret".into()).unwrap();
        let status = post(
            server.port,
            "/cue",
            "secret",
            r#"{"text":"Did the Director penalize you?"}"#,
        );
        assert!(status.contains("204"), "unexpected status: {status}");

        let cue = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(cue.text, "Did the Director penalize you?");
        assert_eq!(server.stats.accepted.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_caption_with_the_wrong_token_is_refused_and_never_spoken() {
        let (server, rx) = CaptionServer::start(0, "secret".into()).unwrap();
        let status = post(server.port, "/cue", "guessed", r#"{"text":"intruder"}"#);
        assert!(status.contains("403"), "unexpected status: {status}");
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
        assert_eq!(server.stats.rejected.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_cleared_caption_is_acknowledged_without_producing_a_cue() {
        let (server, rx) = CaptionServer::start(0, "secret".into()).unwrap();
        let status = post(server.port, "/cue", "secret", r#"{"text":"   "}"#);
        assert!(status.contains("204"), "unexpected status: {status}");
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    }

    #[test]
    fn the_health_check_answers_so_setup_fails_visibly() {
        let (server, _rx) = CaptionServer::start(0, "secret".into()).unwrap();
        assert!(post(server.port, "/health", "secret", "").contains("200"));
    }

    #[test]
    fn a_request_that_arrives_late_is_still_accepted() {
        // The accepted socket used to inherit the listener's non-blocking flag,
        // so a client that connected before it sent anything was answered 400
        // and its caption dropped. Connecting first and writing afterwards
        // reproduces that deterministically, where the ordinary tests only hit
        // it under load.
        let (server, rx) = CaptionServer::start(0, "secret".into()).unwrap();
        let mut s = TcpStream::connect(("127.0.0.1", server.port)).unwrap();

        // Long enough that the server has certainly reached its first read.
        std::thread::sleep(Duration::from_millis(250));

        let body = r#"{"text":"spoken despite the pause"}"#;
        let request = format!(
            "POST /cue HTTP/1.1\r\nHost: localhost\r\n\
             X-ReadSubtitles-Token: secret\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        s.write_all(request.as_bytes()).unwrap();
        s.flush().unwrap();

        let mut reply = String::new();
        s.read_to_string(&mut reply).ok();
        let status = reply.lines().next().unwrap_or_default();
        assert!(status.contains("204"), "late request refused: {status}");

        let cue = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(cue.text, "spoken despite the pause");
    }

    #[test]
    fn a_body_split_across_packets_is_read_whole() {
        // The same failure in its other form: headers in one write, body in
        // another, which is what a real client does when a caption is long.
        let (server, rx) = CaptionServer::start(0, "secret".into()).unwrap();
        let mut s = TcpStream::connect(("127.0.0.1", server.port)).unwrap();

        let body = r#"{"text":"split across two writes"}"#;
        s.write_all(
            format!(
                "POST /cue HTTP/1.1\r\nHost: localhost\r\n\
                 X-ReadSubtitles-Token: secret\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .as_bytes(),
        )
        .unwrap();
        s.flush().unwrap();
        std::thread::sleep(Duration::from_millis(150));
        s.write_all(body.as_bytes()).unwrap();
        s.flush().unwrap();

        let mut reply = String::new();
        s.read_to_string(&mut reply).ok();
        assert!(
            reply.lines().next().unwrap_or_default().contains("204"),
            "split request refused: {reply}"
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap().text,
            "split across two writes"
        );
    }

    #[test]
    fn an_unknown_path_is_not_found() {
        let (server, _rx) = CaptionServer::start(0, "secret".into()).unwrap();
        assert!(post(server.port, "/elsewhere", "secret", "{}").contains("404"));
    }

    #[test]
    fn stopping_the_server_is_idempotent() {
        let (mut server, _rx) = CaptionServer::start(0, "secret".into()).unwrap();
        server.stop();
        server.stop();
    }
}
