//! HTTP + SSE ("Streamable HTTP") transport for MCP.
//!
//! The server exposes a single endpoint. Each `request` is a POST whose
//! body is a JSON-RPC `Request`; the server may answer with either a
//! `application/json` body (single Response) or a `text/event-stream`
//! that ends with a `data:` event carrying the Response. Notifications
//! POST the same way and expect 202 + empty body.
//!
//! We don't open the server-initiated GET SSE stream — POST-only is
//! sufficient for the servers we target today.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;
use tokio::sync::{oneshot, Mutex};

use crate::client::Transport;
use crate::proto::{Request, Response};
use crate::{Error, Result};

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

pub struct HttpTransport {
    url: String,
    http: reqwest::Client,
    bearer: Option<String>,
    extra_headers: HeaderMap,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Response>>>>,
    next_id: AtomicU64,
    request_timeout: Duration,
}

impl HttpTransport {
    pub fn new(url: impl Into<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| Error::Transport(format!("build http client: {e}")))?;
        Ok(Self {
            url: url.into(),
            http,
            bearer: None,
            extra_headers: HeaderMap::new(),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        })
    }

    pub fn with_bearer(mut self, token: impl Into<String>) -> Self {
        self.bearer = Some(token.into());
        self
    }

    pub fn with_header(mut self, name: &'static str, value: &str) -> Result<Self> {
        let header_name = HeaderName::from_static(name);
        let header_value = HeaderValue::from_str(value)
            .map_err(|e| Error::Transport(format!("invalid header value for {name}: {e}")))?;
        self.extra_headers.insert(header_name, header_value);
        Ok(self)
    }

    /// Override the per-request timeout (default 60s). Used by tests.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    fn build_headers(&self, include_sse_accept: bool) -> HeaderMap {
        let mut headers = self.extra_headers.clone();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if include_sse_accept {
            headers.insert(
                ACCEPT,
                HeaderValue::from_static("application/json, text/event-stream"),
            );
        }
        if let Some(token) = &self.bearer {
            if let Ok(v) = HeaderValue::from_str(&format!("Bearer {token}")) {
                headers.insert(AUTHORIZATION, v);
            }
        }
        headers
    }

    async fn do_request(&self, id: u64, body: String) -> Result<Response> {
        let resp = self
            .http
            .post(&self.url)
            .headers(self.build_headers(true))
            .body(body)
            .send()
            .await
            .map_err(|e| Error::Transport(format!("post: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Transport(format!("http {status}: {text}")));
        }

        let content_type = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();

        if content_type.contains("text/event-stream") {
            read_sse_response(resp, id).await
        } else {
            let bytes = resp
                .bytes()
                .await
                .map_err(|e| Error::Transport(format!("read body: {e}")))?;
            let parsed: Response = serde_json::from_slice(&bytes).map_err(|e| {
                Error::Protocol(format!(
                    "parse json response: {e}; body={}",
                    String::from_utf8_lossy(&bytes)
                ))
            })?;
            Ok(parsed)
        }
    }
}

async fn read_sse_response(resp: reqwest::Response, want_id: u64) -> Result<Response> {
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| Error::Transport(format!("sse chunk: {e}")))?;
        buf.extend_from_slice(&chunk);

        while let Some(boundary) = find_event_boundary(&buf) {
            let raw_event = buf.drain(..boundary.end).collect::<Vec<u8>>();
            let event_text = &raw_event[..boundary.event_len];
            let data = extract_data(event_text);
            if data.is_empty() {
                continue;
            }
            match serde_json::from_str::<Response>(&data) {
                Ok(resp) => {
                    if resp.id.as_u64() == Some(want_id) {
                        return Ok(resp);
                    } else {
                        tracing::debug!(
                            event_id = ?resp.id,
                            want_id = want_id,
                            "ignoring sse response with non-matching id"
                        );
                    }
                }
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        data = %data,
                        "ignoring non-response sse data event"
                    );
                }
            }
        }
    }

    Err(Error::Transport(
        "sse stream ended before a matching response arrived".into(),
    ))
}

struct EventBoundary {
    event_len: usize,
    end: usize,
}

fn find_event_boundary(buf: &[u8]) -> Option<EventBoundary> {
    if let Some(pos) = find_subslice(buf, b"\n\n") {
        return Some(EventBoundary {
            event_len: pos,
            end: pos + 2,
        });
    }
    if let Some(pos) = find_subslice(buf, b"\r\n\r\n") {
        return Some(EventBoundary {
            event_len: pos,
            end: pos + 4,
        });
    }
    None
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn extract_data(event_bytes: &[u8]) -> String {
    let text = match std::str::from_utf8(event_bytes) {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    let mut parts: Vec<&str> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(rest) = line.strip_prefix("data:") {
            parts.push(rest.strip_prefix(' ').unwrap_or(rest));
        } else if let Some(rest) = line.strip_prefix("data") {
            // tolerate "data\n" with no payload
            parts.push(rest);
        }
    }
    parts.join("\n")
}

#[async_trait]
impl Transport for HttpTransport {
    async fn request(&self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = Request::call(id, method, params);
        let body = serde_json::to_string(&req)?;

        let (tx, rx) = oneshot::channel::<Response>();
        self.pending.lock().await.insert(id, tx);

        let result = tokio::time::timeout(self.request_timeout, self.do_request(id, body)).await;

        // Regardless of outcome, drop the pending slot — HTTP is request-scoped
        // so nothing else will satisfy it.
        let _ = self.pending.lock().await.remove(&id);
        // The receiver is unused for HTTP (we don't have a separate reader
        // task) but we keep the pending map shape for parity / future use.
        drop(rx);

        match result {
            Ok(Ok(resp)) => {
                if let Some(err) = resp.error {
                    Err(Error::Rpc(err.message))
                } else {
                    Ok(resp.result.unwrap_or(Value::Null))
                }
            }
            Ok(Err(e)) => Err(e),
            Err(_) => Err(Error::Transport(format!(
                "request '{method}' timed out after {:?}",
                self.request_timeout
            ))),
        }
    }

    async fn notify(&self, method: &str, params: Option<Value>) -> Result<()> {
        let req = Request::notify(method, params);
        let body = serde_json::to_string(&req)?;

        let resp = self
            .http
            .post(&self.url)
            .headers(self.build_headers(false))
            .body(body)
            .send()
            .await
            .map_err(|e| Error::Transport(format!("post notify: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Transport(format!("http {status} on notify: {text}")));
        }
        // 202 with empty body is the spec-blessed shape; drain & discard.
        let _ = resp.bytes().await;
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        self.pending.lock().await.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_data_joins_multiline_payloads() {
        let event = b"event: message\ndata: {\"a\":1,\ndata: \"b\":2}";
        let s = extract_data(event);
        assert_eq!(s, "{\"a\":1,\n\"b\":2}");
    }

    #[test]
    fn event_boundary_on_lf_lf() {
        let buf = b"data: hi\n\nrest";
        let b = find_event_boundary(buf).unwrap();
        assert_eq!(b.event_len, 8);
        assert_eq!(b.end, 10);
    }

    #[test]
    fn event_boundary_on_crlf_crlf() {
        let buf = b"data: hi\r\n\r\nrest";
        let b = find_event_boundary(buf).unwrap();
        assert_eq!(b.event_len, 8);
        assert_eq!(b.end, 12);
    }
}
