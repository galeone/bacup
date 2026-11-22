// Copyright 2026 Paolo Galeone
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Heartbeat pings to a healthchecks.io-style URL.
//!
//! A backup with a `ping_url` signals `<ping_url>/start` when a run begins
//! and `<ping_url>` when it succeeds. The monitoring service alerts when the
//! success ping is late, so it also catches the cases where bacup can't say
//! anything itself (process dead, server down, config not loading).
//! A failed run sends `POST <ping_url>/fail` with a plain-text report.
//!
//! A ping never fails a backup: errors are logged and the run goes on.

use std::fmt;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Method, Request};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use log::{debug, warn};
use url::Url;

/// What a ping tells the monitoring service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    /// The run has started: `GET <ping_url>/start`.
    Start,
    /// The run succeeded: `GET <ping_url>`.
    Success,
    /// The run failed: `POST <ping_url>/fail` with this report as a
    /// plain-text body (redacted and truncated, see [fail_body]).
    Fail(String),
}

impl fmt::Display for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Signal::Start => write!(f, "start"),
            Signal::Success => write!(f, "success"),
            Signal::Fail(_) => write!(f, "fail"),
        }
    }
}

/// Bodies above this are cut: healthchecks.io stores up to 100 kB, but a
/// report should stay readable in an alert.
pub const MAX_BODY: usize = 10_000;

/// The body sent with [Signal::Fail]: AWS access key IDs are masked
/// (`AKIA...WXYZ`), since the report leaves the machine, and the text is
/// cut at [MAX_BODY] bytes on a character boundary.
pub fn fail_body(report: &str) -> String {
    let access_key = regex::Regex::new(r"\b(AKIA|ASIA)[A-Z0-9]{12,}([A-Z0-9]{4})\b").unwrap();
    let mut body = access_key.replace_all(report, "$1...$2").into_owned();
    if body.len() > MAX_BODY {
        const MARK: &str = "\n[truncated]";
        let mut cut = MAX_BODY - MARK.len();
        while !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body.truncate(cut);
        body.push_str(MARK);
    }
    body
}

/// Whether a ping reached the monitoring service. Callers only log it.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Delivered,
    Failed(String),
}

#[derive(Clone)]
pub struct Pinger {
    url: Url,
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
    timeout: Duration,
    attempts: u32,
    backoff: Duration,
}

impl fmt::Debug for Pinger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The URL is a credential (anyone holding it can ping): keep it out
        // of debug output.
        f.debug_struct("Pinger").finish_non_exhaustive()
    }
}

impl Pinger {
    /// Per-attempt timeout.
    pub const TIMEOUT: Duration = Duration::from_secs(10);
    /// Attempts per ping, retried on network errors, timeouts and 5xx.
    pub const ATTEMPTS: u32 = 3;
    /// Wait before the 2nd attempt; doubled for each later one.
    pub const BACKOFF: Duration = Duration::from_secs(1);

    pub fn new(url: Url) -> Result<Pinger, String> {
        match url.scheme() {
            "http" | "https" => {}
            scheme => return Err(format!("ping_url: unsupported scheme {scheme}")),
        }
        if url.cannot_be_a_base() {
            return Err("ping_url: not a valid http(s) URL".to_string());
        }
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .map_err(|error| format!("ping_url: unable to load the system CA roots: {error}"))?
            .https_or_http()
            .enable_http1()
            .build();
        let client = Client::builder(TokioExecutor::new()).build(https);
        Ok(Pinger {
            url,
            client,
            timeout: Self::TIMEOUT,
            attempts: Self::ATTEMPTS,
            backoff: Self::BACKOFF,
        })
    }

    /// Overrides timeout, attempts and backoff (used by tests).
    pub fn with_timing(mut self, timeout: Duration, attempts: u32, backoff: Duration) -> Pinger {
        self.timeout = timeout;
        self.attempts = attempts.max(1);
        self.backoff = backoff;
        self
    }

    /// The URL a signal is sent to. Suffixes are appended as path segments,
    /// so a trailing slash or a query string (e.g. `?rid=...`) is preserved.
    pub fn target(&self, signal: &Signal) -> Url {
        let mut url = self.url.clone();
        let suffix = match signal {
            Signal::Start => Some("start"),
            Signal::Success => None,
            Signal::Fail(_) => Some("fail"),
        };
        if let Some(suffix) = suffix {
            // Checked in new(): the URL can be a base, so this is Ok.
            if let Ok(mut segments) = url.path_segments_mut() {
                segments.pop_if_empty().push(suffix);
            }
        }
        url
    }

    /// Sends `signal`, retrying transient failures. Never panics or errors:
    /// the outcome is logged and returned for tests.
    pub async fn send(&self, signal: Signal, backup: &str) -> Outcome {
        let url = self.target(&signal);
        let body = match &signal {
            Signal::Fail(report) => Some(Bytes::from(fail_body(report))),
            _ => None,
        };
        let mut wait = self.backoff;
        let mut last_error = String::new();
        for attempt in 1..=self.attempts {
            if attempt > 1 {
                tokio::time::sleep(wait).await;
                wait *= 2;
            }
            match self.attempt(&url, body.clone()).await {
                Ok(()) => {
                    debug!("[{backup}] Ping {signal} delivered");
                    return Outcome::Delivered;
                }
                Err((error, retry)) => {
                    last_error = error;
                    if !retry {
                        break;
                    }
                }
            }
        }
        // The URL is not logged: it is a credential.
        warn!("[{backup}] Ping {signal} failed: {last_error}. The backup is not affected.");
        Outcome::Failed(last_error)
    }

    /// One request. On error, returns the message and whether to retry.
    async fn attempt(&self, url: &Url, body: Option<Bytes>) -> Result<(), (String, bool)> {
        let builder = Request::builder().uri(url.as_str()).header(
            hyper::header::USER_AGENT,
            concat!("bacup/", env!("CARGO_PKG_VERSION")),
        );
        let request = match body {
            Some(body) => builder
                .method(Method::POST)
                .header(hyper::header::CONTENT_TYPE, "text/plain; charset=utf-8")
                .body(Full::new(body)),
            None => builder.method(Method::GET).body(Full::new(Bytes::new())),
        }
        .map_err(|error| (format!("invalid request: {error}"), false))?;

        let response = tokio::time::timeout(self.timeout, self.client.request(request))
            .await
            .map_err(|_| (format!("timed out after {:?}", self.timeout), true))?
            .map_err(|error| (format!("request error: {error}"), true))?;

        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            // 4xx (e.g. unknown check) won't fix itself on retry; 5xx might.
            Err((format!("HTTP {status}"), status.is_server_error()))
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// How the fake endpoint answers.
    #[derive(Clone, Copy)]
    pub enum Reply {
        Status(u16),
        /// Accept the connection and never answer.
        Hang,
    }

    /// A request the fake endpoint received.
    #[derive(Debug, Clone, PartialEq)]
    pub struct Received {
        pub method: String,
        pub path: String,
        pub body: String,
    }

    /// A minimal HTTP/1.1 server on 127.0.0.1 recording every request.
    pub struct FakeEndpoint {
        pub base: Url,
        pub received: Arc<Mutex<Vec<Received>>>,
    }

    impl FakeEndpoint {
        pub async fn start(reply: Reply) -> FakeEndpoint {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let received = Arc::new(Mutex::new(Vec::new()));
            let log = received.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        return;
                    };
                    let log = log.clone();
                    tokio::spawn(async move {
                        // Serve requests on this connection until it closes
                        // (the client keeps connections alive).
                        let mut buffer = Vec::new();
                        loop {
                            let Some(request) = read_request(&mut socket, &mut buffer).await else {
                                return;
                            };
                            log.lock().unwrap().push(request);
                            match reply {
                                Reply::Hang => {
                                    tokio::time::sleep(Duration::from_secs(3600)).await;
                                    return;
                                }
                                Reply::Status(code) => {
                                    let response =
                                        format!("HTTP/1.1 {code} X\r\ncontent-length: 2\r\n\r\nOK");
                                    if socket.write_all(response.as_bytes()).await.is_err() {
                                        return;
                                    }
                                }
                            }
                        }
                    });
                }
            });
            FakeEndpoint {
                base: Url::parse(&format!("http://{address}/ping/abc")).unwrap(),
                received,
            }
        }

        pub fn requests(&self) -> Vec<Received> {
            self.received.lock().unwrap().clone()
        }
    }

    async fn read_request(
        socket: &mut tokio::net::TcpStream,
        buffer: &mut Vec<u8>,
    ) -> Option<Received> {
        let mut chunk = [0u8; 4096];
        let header_end = loop {
            if let Some(position) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                break position + 4;
            }
            let read = socket.read(&mut chunk).await.ok()?;
            if read == 0 {
                return None;
            }
            buffer.extend_from_slice(&chunk[..read]);
        };
        let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
        let mut request_line = head.lines().next()?.split_whitespace();
        let method = request_line.next()?.to_string();
        let path = request_line.next()?.to_string();
        let length = head
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        while buffer.len() < header_end + length {
            let read = socket.read(&mut chunk).await.ok()?;
            if read == 0 {
                return None;
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
        let body = String::from_utf8_lossy(&buffer[header_end..header_end + length]).to_string();
        buffer.drain(..header_end + length);
        Some(Received { method, path, body })
    }

    pub fn fast(pinger: Pinger) -> Pinger {
        pinger.with_timing(Duration::from_millis(300), 3, Duration::from_millis(10))
    }

    #[test]
    fn target_appends_suffixes() {
        let p = |url: &str| Pinger::new(Url::parse(url).unwrap()).unwrap();
        let hc = p("https://hc-ping.com/0b7c1e1a-uuid");
        assert_eq!(
            hc.target(&Signal::Success).as_str(),
            "https://hc-ping.com/0b7c1e1a-uuid"
        );
        assert_eq!(
            hc.target(&Signal::Start).as_str(),
            "https://hc-ping.com/0b7c1e1a-uuid/start"
        );
        // A trailing slash doesn't produce an empty segment, and the query
        // string survives.
        let slash = p("https://hc-ping.com/key/slug/?create=1");
        assert_eq!(
            slash.target(&Signal::Start).as_str(),
            "https://hc-ping.com/key/slug/start?create=1"
        );
    }

    #[test]
    fn fail_targets_the_fail_endpoint() {
        let hc = Pinger::new(Url::parse("https://hc-ping.com/uuid?rid=1").unwrap()).unwrap();
        assert_eq!(
            hc.target(&Signal::Fail(String::new())).as_str(),
            "https://hc-ping.com/uuid/fail?rid=1"
        );
    }

    #[test]
    fn fail_body_masks_access_keys() {
        let body = fail_body(
            "InvalidAccessKeyId: key AKIAIOSFODNN7EXAMPLE not found; ASIAABCDEFGHIJKL1234",
        );
        assert_eq!(
            body,
            "InvalidAccessKeyId: key AKIA...MPLE not found; ASIA...1234"
        );
        // Words that merely start with AKIA are left alone.
        assert_eq!(fail_body("AKIAshort"), "AKIAshort");
    }

    #[test]
    fn fail_body_is_truncated_on_a_char_boundary() {
        let long = "è".repeat(MAX_BODY);
        let body = fail_body(&long);
        assert!(body.len() <= MAX_BODY, "{}", body.len());
        assert!(body.ends_with("[truncated]"));
        let short = "step: upload";
        assert_eq!(fail_body(short), short);
    }

    #[tokio::test]
    async fn fail_posts_the_report() {
        let endpoint = FakeEndpoint::start(Reply::Status(200)).await;
        let pinger = fast(Pinger::new(endpoint.base.clone()).unwrap());
        let report = "backup: db\nstep: dump\nerror: boom".to_string();
        assert_eq!(
            pinger.send(Signal::Fail(report.clone()), "t").await,
            Outcome::Delivered
        );
        let requests = endpoint.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/ping/abc/fail");
        assert_eq!(requests[0].body, report);
    }

    #[test]
    fn rejects_non_http_urls() {
        assert!(Pinger::new(Url::parse("ftp://example.com/x").unwrap()).is_err());
        assert!(Pinger::new(Url::parse("data:text/plain,hi").unwrap()).is_err());
    }

    #[tokio::test]
    async fn success_and_start_hit_the_right_paths() {
        let endpoint = FakeEndpoint::start(Reply::Status(200)).await;
        let pinger = fast(Pinger::new(endpoint.base.clone()).unwrap());
        assert_eq!(pinger.send(Signal::Start, "t").await, Outcome::Delivered);
        assert_eq!(pinger.send(Signal::Success, "t").await, Outcome::Delivered);
        let requests = endpoint.requests();
        let paths: Vec<_> = requests
            .iter()
            .map(|r| (r.method.as_str(), r.path.as_str()))
            .collect();
        assert_eq!(
            paths,
            vec![("GET", "/ping/abc/start"), ("GET", "/ping/abc")]
        );
    }

    #[tokio::test]
    async fn server_errors_are_retried_then_given_up() {
        let endpoint = FakeEndpoint::start(Reply::Status(503)).await;
        let pinger = fast(Pinger::new(endpoint.base.clone()).unwrap());
        let outcome = pinger.send(Signal::Success, "t").await;
        assert!(
            matches!(outcome, Outcome::Failed(ref e) if e.contains("503")),
            "{:?}",
            outcome
        );
        assert_eq!(endpoint.requests().len(), 3);
    }

    #[tokio::test]
    async fn client_errors_are_not_retried() {
        let endpoint = FakeEndpoint::start(Reply::Status(404)).await;
        let pinger = fast(Pinger::new(endpoint.base.clone()).unwrap());
        let outcome = pinger.send(Signal::Success, "t").await;
        assert!(
            matches!(outcome, Outcome::Failed(ref e) if e.contains("404")),
            "{:?}",
            outcome
        );
        assert_eq!(endpoint.requests().len(), 1);
    }

    #[tokio::test]
    async fn a_hanging_endpoint_is_bounded_by_the_timeout() {
        let endpoint = FakeEndpoint::start(Reply::Hang).await;
        let pinger = Pinger::new(endpoint.base.clone()).unwrap().with_timing(
            Duration::from_millis(200),
            2,
            Duration::from_millis(10),
        );
        let started = Instant::now();
        let outcome = pinger.send(Signal::Success, "t").await;
        assert!(
            matches!(outcome, Outcome::Failed(ref e) if e.contains("timed out")),
            "{:?}",
            outcome
        );
        // 2 attempts x 200 ms + backoff, with generous slack for slow CI.
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(endpoint.requests().len(), 2);
    }

    /// Real HTTPS round trip (network): proves the TLS stack and the
    /// system CA roots work. Any HTTP status counts as success here.
    /// Run with `cargo test https_smoke -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn https_smoke() {
        let pinger = Pinger::new(Url::parse("https://hc-ping.com/").unwrap()).unwrap();
        match pinger.send(Signal::Success, "smoke").await {
            Outcome::Delivered => {}
            Outcome::Failed(error) => assert!(error.starts_with("HTTP "), "{}", error),
        }
    }

    #[tokio::test]
    async fn an_unreachable_endpoint_fails_quickly() {
        // Bind and drop: nothing listens on this port any more.
        let address = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap();
        let url = Url::parse(&format!("http://{address}/abc")).unwrap();
        let pinger = fast(Pinger::new(url).unwrap());
        let started = Instant::now();
        let outcome = pinger.send(Signal::Success, "t").await;
        assert!(matches!(outcome, Outcome::Failed(_)), "{:?}", outcome);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }
}
