//! End-to-end test of the Anthropic SSE adapter against a fixture stream.
//!
//! We stand up a tiny HTTP server that returns a hand-rolled SSE response
//! mimicking a real Anthropic exchange (text deltas + a tool_use block), then
//! drive the client and assert on the emitted [`LlmStreamEvent`]s.

use std::sync::Arc;

use futures::StreamExt;
use merlion_core::{LlmClient, LlmRequest, LlmStreamEvent, Message};
use merlion_llm::AnthropicClient;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

const FIXTURE: &str = concat!(
    "event: message_start\n",
    "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n\n",
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello \"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"world\"}}\n\n",
    "event: content_block_stop\n",
    "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_42\",\"name\":\"bash\",\"input\":{}}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"comm\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"and\\\":\\\"ls\\\"}\"}}\n\n",
    "event: content_block_stop\n",
    "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
    "event: message_delta\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
    "event: message_stop\n",
    "data: {\"type\":\"message_stop\"}\n\n",
);

async fn spawn_fixture_server(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = [0u8; 8192];
            let _ = tokio::io::AsyncReadExt::read(&mut sock, &mut buf).await;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n"
            );
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
async fn anthropic_stream_emits_text_then_tool_call_then_done() {
    let base = spawn_fixture_server(FIXTURE).await;
    let client = Arc::new(AnthropicClient::new(base, Some("sk-test".into())).unwrap());

    let req = LlmRequest {
        model: "claude-opus-4-7".into(),
        messages: vec![Message::user("hi")],
        tools: vec![],
        temperature: None,
        max_tokens: Some(1024),
    };

    let mut stream = client.stream(req).await.unwrap();
    let mut text = String::new();
    let mut got_tool_call = false;
    let mut got_done = false;

    while let Some(ev) = stream.next().await {
        match ev.unwrap() {
            LlmStreamEvent::Delta(s) => text.push_str(&s),
            LlmStreamEvent::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].id, "toolu_42");
                assert_eq!(calls[0].name, "bash");
                assert_eq!(calls[0].arguments["command"], "ls");
                got_tool_call = true;
            }
            LlmStreamEvent::Done(reason) => {
                assert_eq!(reason.as_deref(), Some("tool_use"));
                got_done = true;
            }
        }
    }

    assert_eq!(text, "Hello world");
    assert!(got_tool_call, "expected a ToolCalls event");
    assert!(got_done, "expected a Done event");
}
