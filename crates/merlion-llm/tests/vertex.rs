//! Vertex AI adapter tests.
//!
//! Vertex shares its wire format with Gemini Studio, so the SSE fixture
//! mirrors `gemini_sse.rs`. The two adapter-specific concerns these tests
//! lock down are:
//!
//! 1. URL construction — the project/region/model path is non-trivial.
//! 2. Auth — bypass `gcloud` via `with_token` so the test can run without
//!    a Google Cloud login.

use std::sync::Arc;

use futures::StreamExt;
use merlion_core::{LlmClient, LlmRequest, LlmStreamEvent, Message, ToolSchema};
use merlion_llm::vertex::{build_body, build_url, VertexClient};
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

#[test]
fn build_url_matches_vertex_template() {
    let url = build_url("my-proj", "us-central1", "gemini-2.0-flash-001");
    assert_eq!(
        url,
        "https://us-central1-aiplatform.googleapis.com/v1/projects/my-proj/locations/us-central1/publishers/google/models/gemini-2.0-flash-001:streamGenerateContent?alt=sse"
    );
}

#[test]
fn build_body_carries_contents_system_instruction_and_tools() {
    let tool = ToolSchema {
        name: "bash".into(),
        description: "run".into(),
        parameters: json!({
            "type": "object",
            "properties": { "command": { "type": "string", "default": "ls" } }
        }),
    };
    let req = LlmRequest {
        model: "gemini-2.0-flash-001".into(),
        messages: vec![Message::system("be brief"), Message::user("hi")],
        tools: vec![tool],
        temperature: Some(0.3),
        max_tokens: Some(128),
    };
    let body = build_body(&req);
    assert!(body.get("contents").is_some());
    assert_eq!(body["system_instruction"]["parts"][0]["text"], "be brief");
    let decl = &body["tools"][0]["functionDeclarations"][0];
    assert_eq!(decl["name"], "bash");
    // sanitize_schema must have stripped `default`.
    assert!(decl["parameters"]["properties"]["command"]
        .get("default")
        .is_none());
    let temp = body["generationConfig"]["temperature"].as_f64().unwrap();
    assert!((temp - 0.3).abs() < 1e-6);
    assert_eq!(body["generationConfig"]["maxOutputTokens"], 128);
}

#[test]
fn build_body_omits_optional_blocks_when_unused() {
    let req = LlmRequest {
        model: "gemini-2.0-flash-001".into(),
        messages: vec![Message::user("hi")],
        tools: vec![],
        temperature: None,
        max_tokens: None,
    };
    let body = build_body(&req);
    assert!(body.get("system_instruction").is_none());
    assert!(body.get("tools").is_none());
    assert!(body.get("generationConfig").is_none());
}

const FIXTURE: &str = concat!(
    "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"Hello \"}]}}]}\n\n",
    "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"world\"}]}}]}\n\n",
    "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"functionCall\":{\"name\":\"bash\",\"args\":{\"command\":\"ls\"}}}]}}]}\n\n",
    "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":12,\"candidatesTokenCount\":7,\"totalTokenCount\":19}}\n\n",
);

async fn spawn_fixture_server(
    body: &'static str,
    expected_path_suffix: &'static str,
    expect_bearer: &'static str,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = [0u8; 8192];
            let n = tokio::io::AsyncReadExt::read(&mut sock, &mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(
                req.contains(expected_path_suffix),
                "fixture server saw unexpected request path:\n{req}"
            );
            assert!(
                req.to_lowercase().contains(&format!("authorization: bearer {expect_bearer}").to_lowercase()),
                "fixture server did not see expected Authorization header:\n{req}"
            );
            let resp = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n".to_string();
            sock.write_all(resp.as_bytes()).await.unwrap();
            for chunk in body.split_inclusive("\n\n") {
                let len = format!("{:x}\r\n", chunk.len());
                sock.write_all(len.as_bytes()).await.unwrap();
                sock.write_all(chunk.as_bytes()).await.unwrap();
                sock.write_all(b"\r\n").await.unwrap();
            }
            sock.write_all(b"0\r\n\r\n").await.unwrap();
            let _ = sock.shutdown().await;
        }
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn vertex_stream_emits_text_then_function_call_then_usage_then_done() {
    let base = spawn_fixture_server(
        FIXTURE,
        "/v1/projects/my-proj/locations/us-central1/publishers/google/models/gemini-2.0-flash-001:streamGenerateContent?alt=sse",
        "fake-test-token",
    )
    .await;

    let client = Arc::new(
        VertexClient::new("my-proj", "us-central1")
            .unwrap()
            .with_base_url(base)
            .with_token("fake-test-token"),
    );

    let req = LlmRequest {
        model: "gemini-2.0-flash-001".into(),
        messages: vec![Message::user("hi")],
        tools: vec![],
        temperature: None,
        max_tokens: None,
    };

    let mut stream = client.stream(req).await.unwrap();
    let mut text = String::new();
    let mut got_tool_call = false;
    let mut got_usage = false;
    let mut got_done = false;

    while let Some(ev) = stream.next().await {
        match ev.unwrap() {
            LlmStreamEvent::Delta(s) => text.push_str(&s),
            LlmStreamEvent::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "bash");
                assert_eq!(calls[0].arguments["command"], "ls");
                assert!(calls[0].id.starts_with("vtx_bash_"));
                got_tool_call = true;
            }
            LlmStreamEvent::Usage(u) => {
                assert_eq!(u.prompt_tokens, Some(12));
                assert_eq!(u.completion_tokens, Some(7));
                assert_eq!(u.total_tokens, Some(19));
                got_usage = true;
            }
            LlmStreamEvent::Done(reason) => {
                assert_eq!(reason.as_deref(), Some("STOP"));
                got_done = true;
            }
        }
    }

    assert_eq!(text, "Hello world");
    assert!(got_tool_call, "expected a ToolCalls event");
    assert!(got_usage, "expected a Usage event");
    assert!(got_done, "expected a Done event");
}
