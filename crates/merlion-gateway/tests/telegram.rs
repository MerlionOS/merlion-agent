//! End-to-end test of the Telegram adapter against a fixture HTTP server.
//!
//! Mirrors the pattern in `merlion-llm/tests/anthropic_sse.rs`: a tiny TCP
//! listener serves canned JSON responses for `/bot<token>/getUpdates` and
//! `/bot<token>/sendMessage`. We drive `TelegramGateway::run` for a short
//! window and assert on what flowed through the channels.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use merlion_gateway::telegram::{
    message_from_update, TelegramGateway, TgChat, TgMessage, TgUpdate, TgUser,
};
use merlion_gateway::{Gateway, OutgoingMessage};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

#[test]
fn message_from_update_extracts_fields() {
    let update = TgUpdate {
        update_id: 7,
        message: Some(TgMessage {
            message_id: 42,
            from: Some(TgUser {
                id: 99,
                first_name: Some("Ada".into()),
                last_name: Some("Lovelace".into()),
                username: Some("ada".into()),
            }),
            chat: TgChat { id: -100123 },
            text: Some("hello world".into()),
            voice: None,
        }),
    };

    let im = message_from_update(update).expect("expected an IncomingMessage");
    assert_eq!(im.user.platform, "telegram");
    assert_eq!(im.user.id, "99");
    assert_eq!(im.user.display_name, "Ada Lovelace");
    assert_eq!(im.conversation_id, "-100123");
    assert_eq!(im.message_id, "42");
    assert_eq!(im.text, "hello world");
}

#[test]
fn message_from_update_skips_non_text() {
    let update = TgUpdate {
        update_id: 8,
        message: Some(TgMessage {
            message_id: 43,
            from: Some(TgUser {
                id: 1,
                first_name: Some("X".into()),
                last_name: None,
                username: None,
            }),
            chat: TgChat { id: 1 },
            text: None,
            voice: None,
        }),
    };
    assert!(message_from_update(update).is_none());
}

#[test]
fn message_from_update_first_name_only() {
    let update = TgUpdate {
        update_id: 9,
        message: Some(TgMessage {
            message_id: 44,
            from: Some(TgUser {
                id: 2,
                first_name: Some("Solo".into()),
                last_name: None,
                username: None,
            }),
            chat: TgChat { id: 2 },
            text: Some("hi".into()),
            voice: None,
        }),
    };
    let im = message_from_update(update).unwrap();
    assert_eq!(im.user.display_name, "Solo");
}

#[test]
fn tg_message_with_voice_and_no_text_deserializes() {
    let raw = r#"{
        "message_id": 51,
        "from": {"id": 7, "first_name": "Vox"},
        "chat": {"id": 7},
        "voice": {
            "file_id": "AwACAgI-voice-id",
            "duration": 3,
            "mime_type": "audio/ogg"
        }
    }"#;

    let m: TgMessage = serde_json::from_str(raw).expect("voice-only TgMessage should parse");
    assert!(m.text.is_none());
    let voice = m.voice.expect("voice field present");
    assert_eq!(voice.file_id, "AwACAgI-voice-id");
    assert_eq!(voice.duration, Some(3));
    assert_eq!(voice.mime_type.as_deref(), Some("audio/ogg"));
}

#[test]
fn voice_transcript_prefix_is_prepended() {
    let transcript = "hello from a voice memo";
    let prefixed = format!("[voice 🎙️] {transcript}");
    assert!(prefixed.starts_with("[voice 🎙️] "));
    assert!(prefixed.ends_with(transcript));
}

/// Fixture Telegram API server. Serves a canned getUpdates response on the
/// first request and an empty result on subsequent polls so the long-poll
/// loop keeps running quietly. Any `/sendMessage` POST is answered with a
/// success body and forwarded to `sends_tx` for assertions.
async fn spawn_fixture_telegram(
    token: &'static str,
    sends_tx: mpsc::Sender<String>,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let token = token.to_string();

    tokio::spawn(async move {
        let updates_served = Arc::new(AtomicU32::new(0));
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(p) => p,
                Err(_) => return,
            };
            let sends_tx = sends_tx.clone();
            let token = token.clone();
            let updates_served = updates_served.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16 * 1024];
                let mut filled = 0usize;
                let (path, body_bytes): (String, Vec<u8>) = loop {
                    let n = match sock.read(&mut buf[filled..]).await {
                        Ok(0) => return,
                        Ok(n) => n,
                        Err(_) => return,
                    };
                    filled += n;
                    let Some(header_end) = find_double_crlf(&buf[..filled]) else {
                        if filled == buf.len() {
                            buf.resize(buf.len() * 2, 0);
                        }
                        continue;
                    };
                    let header_str =
                        std::str::from_utf8(&buf[..header_end]).unwrap_or("").to_string();
                    let content_length = header_str
                        .lines()
                        .find_map(|l| {
                            let mut parts = l.splitn(2, ':');
                            let k = parts.next()?.trim().to_ascii_lowercase();
                            let v = parts.next()?.trim();
                            if k == "content-length" { v.parse::<usize>().ok() } else { None }
                        })
                        .unwrap_or(0);
                    let body_start = header_end + 4;
                    let need = body_start + content_length;
                    if buf.len() < need {
                        buf.resize(need, 0);
                    }
                    while filled < need {
                        let n = match sock.read(&mut buf[filled..]).await {
                            Ok(0) => return,
                            Ok(n) => n,
                            Err(_) => return,
                        };
                        filled += n;
                    }

                    let request_line = header_str.lines().next().unwrap_or("").to_string();
                    let path = request_line
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or("")
                        .to_string();

                    let body = buf[body_start..body_start + content_length].to_vec();
                    break (path, body);
                };
                let body_bytes = body_bytes.as_slice();

                let prefix = format!("/bot{token}/");
                let resp_body: String = if let Some(rest) = path.strip_prefix(&prefix) {
                    let method = rest.split('?').next().unwrap_or("");
                    match method {
                        "getUpdates" => {
                            let prev = updates_served.fetch_add(1, Ordering::SeqCst);
                            if prev == 0 {
                                r#"{"ok":true,"result":[{"update_id":100,"message":{"message_id":7,"from":{"id":555,"first_name":"Test","last_name":"User"},"chat":{"id":555},"text":"ping"}}]}"#.into()
                            } else {
                                tokio::time::sleep(Duration::from_millis(50)).await;
                                r#"{"ok":true,"result":[]}"#.into()
                            }
                        }
                        "sendMessage" => {
                            let s = String::from_utf8_lossy(body_bytes).to_string();
                            let _ = sends_tx.send(s).await;
                            r#"{"ok":true,"result":{}}"#.into()
                        }
                        _ => r#"{"ok":false,"description":"unknown method"}"#.into(),
                    }
                } else {
                    r#"{"ok":false,"description":"bad path"}"#.into()
                };

                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    resp_body.len(),
                    resp_body,
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });

    format!("http://{addr}")
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

#[tokio::test]
async fn telegram_gateway_polls_and_sends() {
    let token = "test-token";
    let (sends_tx, mut sends_rx) = mpsc::channel::<String>(8);
    let base = spawn_fixture_telegram(token, sends_tx).await;

    let gw = Arc::new(TelegramGateway::new(token).with_base_url(base));
    let (incoming_tx, mut incoming_rx) = mpsc::channel(8);
    let (outgoing_tx, outgoing_rx) = mpsc::channel(8);

    let run_handle = tokio::spawn(gw.clone().run(incoming_tx, outgoing_rx));

    // Receiver should pick up the canned update.
    let im = tokio::time::timeout(Duration::from_secs(2), incoming_rx.recv())
        .await
        .expect("timed out waiting for IncomingMessage")
        .expect("incoming channel closed");
    assert_eq!(im.user.platform, "telegram");
    assert_eq!(im.user.id, "555");
    assert_eq!(im.user.display_name, "Test User");
    assert_eq!(im.conversation_id, "555");
    assert_eq!(im.message_id, "7");
    assert_eq!(im.text, "ping");

    // Send an outgoing message and confirm the fixture sees it.
    outgoing_tx
        .send(OutgoingMessage {
            conversation_id: "555".into(),
            reply_to: Some("7".into()),
            text: "pong".into(),
        })
        .await
        .unwrap();

    let sent_body = tokio::time::timeout(Duration::from_secs(2), sends_rx.recv())
        .await
        .expect("timed out waiting for sendMessage")
        .expect("sends channel closed");
    assert!(sent_body.contains("\"chat_id\":555"), "body was: {sent_body}");
    assert!(sent_body.contains("\"text\":\"pong\""), "body was: {sent_body}");
    assert!(
        sent_body.contains("\"reply_to_message_id\":7"),
        "body was: {sent_body}"
    );

    // Tear down: close outgoing first then abort the run task.
    drop(outgoing_tx);
    drop(incoming_rx);
    run_handle.abort();
    let _ = run_handle.await;
}
