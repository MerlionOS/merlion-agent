use std::time::Duration;

use merlion_mcp::client::Transport;
use merlion_mcp::{Error, StdioTransport};

/// A mock MCP server that reads each line of stdin and writes a canned reply.
const ECHO_SERVER: &str = r#"
while IFS= read -r line; do
    echo '{"jsonrpc":"2.0","id":1,"result":{"ok":true}}'
done
"#;

/// Mock server that floods stderr with many lines before answering on stdout.
/// Exercises the stderr-drain task to make sure it doesn't deadlock.
const STDERR_FLOOD_SERVER: &str = r#"
for i in $(seq 1 200); do
    echo "noisy stderr line $i" 1>&2
done
while IFS= read -r line; do
    echo '{"jsonrpc":"2.0","id":1,"result":{"noisy":true}}'
done
"#;

/// Mock server that returns a JSON-RPC error response.
const ERROR_SERVER: &str = r#"
while IFS= read -r line; do
    echo '{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"unknown method"}}'
done
"#;

/// Mock server that reads input but never replies.
const SILENT_SERVER: &str = r#"
while IFS= read -r line; do
    :
done
"#;

#[tokio::test]
async fn echo_server_round_trips_a_request() {
    let transport = StdioTransport::spawn_simple("bash", &["-c".into(), ECHO_SERVER.into()])
        .await
        .expect("spawn echo server");

    let result = transport
        .request("ping", None)
        .await
        .expect("ping request");

    assert_eq!(result, serde_json::json!({"ok": true}));

    transport.close().await.unwrap();
}

#[tokio::test]
async fn stderr_flood_does_not_block_the_response() {
    let transport =
        StdioTransport::spawn_simple("bash", &["-c".into(), STDERR_FLOOD_SERVER.into()])
            .await
            .expect("spawn stderr-flood server");

    let result = tokio::time::timeout(Duration::from_secs(5), transport.request("ping", None))
        .await
        .expect("request did not deadlock")
        .expect("request succeeded");

    assert_eq!(result, serde_json::json!({"noisy": true}));

    transport.close().await.unwrap();
}

#[tokio::test]
async fn rpc_error_response_surfaces_as_error() {
    let transport = StdioTransport::spawn_simple("bash", &["-c".into(), ERROR_SERVER.into()])
        .await
        .expect("spawn error server");

    let err = transport
        .request("bogus", None)
        .await
        .expect_err("expected error");

    match err {
        Error::Rpc(msg) => assert!(msg.contains("unknown method"), "got: {msg}"),
        other => panic!("expected Error::Rpc, got {other:?}"),
    }

    transport.close().await.unwrap();
}

#[tokio::test]
async fn request_times_out_when_server_never_replies() {
    let transport = StdioTransport::spawn_simple("bash", &["-c".into(), SILENT_SERVER.into()])
        .await
        .expect("spawn silent server")
        .with_timeout(Duration::from_millis(200));

    let err = transport
        .request("ping", None)
        .await
        .expect_err("expected timeout");

    match err {
        Error::Transport(msg) => assert!(msg.contains("timed out"), "got: {msg}"),
        other => panic!("expected Error::Transport(timeout), got {other:?}"),
    }

    transport.close().await.unwrap();
}

#[tokio::test]
async fn notify_does_not_wait_for_a_response() {
    let transport = StdioTransport::spawn_simple("bash", &["-c".into(), SILENT_SERVER.into()])
        .await
        .expect("spawn silent server");

    tokio::time::timeout(
        Duration::from_secs(1),
        transport.notify("notifications/initialized", None),
    )
    .await
    .expect("notify returned promptly")
    .expect("notify succeeded");

    transport.close().await.unwrap();
}
