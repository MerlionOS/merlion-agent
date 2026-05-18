use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use merlion_core::{Tool, ToolResult, ToolSchema};
use serde::Deserialize;
use serde_json::json;

const DEFAULT_MAX_BYTES: usize = 256 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const OUTER_TIMEOUT: Duration = Duration::from_secs(35);
const USER_AGENT: &str = "merlion-agent/0.1";

#[derive(Default)]
pub struct WebFetch;

#[derive(Debug, Deserialize)]
struct Args {
    url: String,
    #[serde(default)]
    max_bytes: Option<usize>,
    #[serde(default)]
    as_html: Option<bool>,
}

#[async_trait]
impl Tool for WebFetch {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "web_fetch".into(),
            description:
                "HTTP GET a URL and return the response body as readable plain text. \
                 By default, HTML responses are converted to plain text (tags stripped). \
                 Set `as_html: true` to return raw HTML. Body is capped at `max_bytes` \
                 (default 262144 = 256 KiB)."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "URL to fetch. Must start with http:// or https://."
                    },
                    "max_bytes": {
                        "type": "integer",
                        "description": "Maximum bytes of the response body to read. Default 262144 (256 KiB)."
                    },
                    "as_html": {
                        "type": "boolean",
                        "description": "If true, return raw HTML. If false (default), convert HTML to plain text."
                    }
                },
                "required": ["url"]
            }),
        }
    }

    async fn call(&self, call_id: &str, args: serde_json::Value) -> ToolResult {
        let parsed: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return err(call_id, format!("invalid arguments: {e}")),
        };

        let fut = do_fetch(parsed);
        match tokio::time::timeout(OUTER_TIMEOUT, fut).await {
            Ok(Ok(content)) => ToolResult {
                tool_call_id: call_id.into(),
                name: "web_fetch".into(),
                content,
                is_error: false,
            },
            Ok(Err(msg)) => err(call_id, msg),
            Err(_) => err(call_id, format!("timed out after {OUTER_TIMEOUT:?}")),
        }
    }
}

async fn do_fetch(args: Args) -> Result<String, String> {
    let url = args.url;
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!(
            "invalid URL scheme (must be http:// or https://): {url}"
        ));
    }
    let max_bytes = args.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
    let as_html = args.as_html.unwrap_or(false);

    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))?;

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(format!("HTTP {} from {}", status.as_u16(), url));
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    let mut body: Vec<u8> = Vec::new();
    let mut truncated = false;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("read body failed: {e}"))?;
        if body.len() + chunk.len() > max_bytes {
            let take = max_bytes.saturating_sub(body.len());
            body.extend_from_slice(&chunk[..take]);
            truncated = true;
            break;
        }
        body.extend_from_slice(&chunk);
    }

    let is_html = content_type.starts_with("text/html");
    let mut content = if is_html && !as_html {
        html2text::from_read(&body[..], 80)
    } else {
        String::from_utf8_lossy(&body).into_owned()
    };

    if truncated {
        content.push_str(&format!("\n…[truncated at {max_bytes} bytes]"));
    }

    Ok(content)
}

fn err(call_id: &str, msg: String) -> ToolResult {
    ToolResult {
        tool_call_id: call_id.into(),
        name: "web_fetch".into(),
        content: msg,
        is_error: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn spawn_http_server(response: &'static [u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                // Read the request (best-effort; ignore the contents).
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let _ = sock.write_all(response).await;
                let _ = sock.shutdown().await;
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn rejects_non_http_url() {
        let tool = WebFetch::default();
        let res = tool
            .call(
                "c1",
                json!({
                    "url": "ftp://foo.example.com/bar"
                }),
            )
            .await;
        assert!(res.is_error, "expected an error result, got: {res:?}");
        assert!(
            res.content.contains("scheme"),
            "unexpected error message: {}",
            res.content
        );
    }

    #[tokio::test]
    async fn html_is_converted_to_plain_text() {
        let body = "<html><body><h1>Hi</h1><p>World</p></body></html>";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        // Leak the response so we get a 'static slice for the spawned task.
        let leaked: &'static [u8] = Box::leak(response.into_bytes().into_boxed_slice());
        let base = spawn_http_server(leaked).await;

        let tool = WebFetch::default();
        let res = tool.call("c2", json!({ "url": base })).await;
        assert!(!res.is_error, "unexpected error: {}", res.content);
        assert!(
            res.content.contains("Hi"),
            "missing 'Hi' in extracted output: {}",
            res.content
        );
        assert!(
            res.content.contains("World"),
            "missing 'World' in extracted output: {}",
            res.content
        );
        assert!(
            !res.content.contains("<h1>"),
            "expected tags stripped, got: {}",
            res.content
        );
    }

    #[tokio::test]
    async fn truncates_at_max_bytes() {
        let body = "A".repeat(2048);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let leaked: &'static [u8] = Box::leak(response.into_bytes().into_boxed_slice());
        let base = spawn_http_server(leaked).await;

        let tool = WebFetch::default();
        let res = tool
            .call(
                "c3",
                json!({
                    "url": base,
                    "max_bytes": 256
                }),
            )
            .await;
        assert!(!res.is_error, "unexpected error: {}", res.content);
        assert!(
            res.content.contains("[truncated at 256 bytes]"),
            "missing truncation marker, got: {}",
            res.content
        );
        // First 256 bytes of body + truncation suffix. Body portion should be 256 'A's.
        let a_count = res.content.chars().filter(|c| *c == 'A').count();
        assert_eq!(a_count, 256, "expected 256 'A' chars, got {a_count}");
    }
}
