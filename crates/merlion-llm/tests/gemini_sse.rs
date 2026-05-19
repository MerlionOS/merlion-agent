//! End-to-end test of the Gemini SSE adapter against a fixture stream.
//!
//! Gemini's stream is conceptually simpler than Anthropic's: every `data:`
//! line is a full `GenerateContentResponse` fragment whose `candidates[0]
//! .content.parts[]` carries concrete deltas (text snippets and complete
//! `functionCall` objects).

use std::sync::Arc;

use futures::StreamExt;
use merlion_core::{LlmClient, LlmRequest, LlmStreamEvent, Message};
use merlion_llm::GeminiClient;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

const FIXTURE: &str = concat!(
    "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"Hello \"}]}}]}\n\n",
    "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"world\"}]}}]}\n\n",
    "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"functionCall\":{\"name\":\"bash\",\"args\":{\"command\":\"ls\"}}}]}}]}\n\n",
    "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[]},\"finishReason\":\"STOP\"}]}\n\n",
);

async fn spawn_fixture_server(body: &'static str, expected_path_suffix: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = [0u8; 8192];
            let n = tokio::io::AsyncReadExt::read(&mut sock, &mut buf)
                .await
                .unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(
                req.contains(expected_path_suffix),
                "fixture server saw unexpected request path:\n{req}"
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
async fn gemini_stream_emits_text_then_function_call_then_done() {
    let base = spawn_fixture_server(
        FIXTURE,
        "/models/gemini-2.0-flash:streamGenerateContent?alt=sse",
    )
    .await;
    let client = Arc::new(GeminiClient::new(base, Some("k".into())).unwrap());

    let req = LlmRequest {
        model: "gemini-2.0-flash".into(),
        messages: vec![Message::user("hi")],
        tools: vec![],
        temperature: None,
        max_tokens: None,
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
                assert_eq!(calls[0].name, "bash");
                assert_eq!(calls[0].arguments["command"], "ls");
                assert!(calls[0].id.starts_with("gem_bash_"));
                got_tool_call = true;
            }
            LlmStreamEvent::Done(reason) => {
                assert_eq!(reason.as_deref(), Some("STOP"));
                got_done = true;
            }
            LlmStreamEvent::Usage(_) => {}
        }
    }

    assert_eq!(text, "Hello world");
    assert!(got_tool_call, "expected a ToolCalls event");
    assert!(got_done, "expected a Done event");
}
