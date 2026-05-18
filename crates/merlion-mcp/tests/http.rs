//! End-to-end tests for the MCP HTTP+SSE transport.
//!
//! Uses a hand-rolled TcpListener fixture so we can return exact headers
//! (`Content-Type: application/json` vs `text/event-stream`) and exact
//! framing per test case — see `crates/merlion-llm/tests/anthropic_sse.rs`
//! for the same pattern applied to a different protocol.

use std::time::Duration;

use merlion_mcp::{Error, HttpTransport, Transport};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Fixture that replies to every POST with the same canned response.
async fn spawn_fixture(response_headers: &'static str, body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            // Drain request bytes until we see end-of-headers; we don't
            // care about the body content (only that it parses).
            let mut req_buf = Vec::with_capacity(2048);
            let mut tmp = [0u8; 1024];
            loop {
                match tokio::time::timeout(Duration::from_millis(200), sock.read(&mut tmp)).await {
                    Ok(Ok(0)) => break,
                    Ok(Ok(n)) => {
                        req_buf.extend_from_slice(&tmp[..n]);
                        if req_buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    _ => break,
                }
            }
            let status_line = "HTTP/1.1 200 OK\r\n";
            let full = format!("{status_line}{response_headers}\r\n{body}");
            let _ = sock.write_all(full.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    format!("http://{addr}/mcp")
}

#[tokio::test]
async fn http_request_with_json_response_parses() {
    // jsonrpc id is u64-assigned starting at 1 inside the transport.
    let body = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"echo","inputSchema":{}}]}}"#;
    let headers = format!(
        "Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    // Leak the formatted string to get a 'static slice for the fixture.
    let headers_static: &'static str = Box::leak(headers.into_boxed_str());
    let url = spawn_fixture(headers_static, body).await;

    let transport = HttpTransport::new(url)
        .unwrap()
        .with_timeout(Duration::from_secs(5));
    let raw = transport
        .request("tools/list", None)
        .await
        .expect("request should succeed");
    let tools = raw.get("tools").and_then(|v| v.as_array()).unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "echo");
}

#[tokio::test]
async fn http_request_with_sse_response_parses() {
    // Two non-matching events arrive first (server-initiated notifications),
    // then the matching response. We must skip the first two and surface
    // the matching response's `result`.
    let body = concat!(
        "event: message\n",
        "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{\"x\":1}}\n",
        "\n",
        "event: message\n",
        "data: {\"jsonrpc\":\"2.0\",\"id\":999,\"result\":{\"unrelated\":true}}\n",
        "\n",
        "event: message\n",
        "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true,\"value\":42}}\n",
        "\n",
    );
    let headers = "Content-Type: text/event-stream\r\nConnection: close\r\n";
    let url = spawn_fixture(headers, body).await;

    let transport = HttpTransport::new(url)
        .unwrap()
        .with_timeout(Duration::from_secs(5));
    let raw = transport
        .request("ping", None)
        .await
        .expect("sse request should succeed");
    assert_eq!(raw["ok"], true);
    assert_eq!(raw["value"], 42);
}

#[tokio::test]
async fn http_request_with_rpc_error_envelope_surfaces_as_rpc_error() {
    let body =
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"method not found"}}"#;
    let headers = format!(
        "Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    let headers_static: &'static str = Box::leak(headers.into_boxed_str());
    let url = spawn_fixture(headers_static, body).await;

    let transport = HttpTransport::new(url)
        .unwrap()
        .with_timeout(Duration::from_secs(5));
    let err = transport
        .request("does/not/exist", None)
        .await
        .expect_err("rpc error envelope must surface as Err");
    match err {
        Error::Rpc(msg) => assert!(msg.contains("method not found"), "got: {msg}"),
        other => panic!("expected Error::Rpc, got {other:?}"),
    }
}
