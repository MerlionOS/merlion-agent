use merlion_core::Tool;
use merlion_tools::bash::Bash;
use serde_json::json;

#[tokio::test]
async fn bash_echo_roundtrip() {
    let r = Bash
        .call("call_1", json!({ "command": "echo hello-merlion" }))
        .await;
    assert!(!r.is_error, "tool reported error: {}", r.content);
    assert!(
        r.content.contains("hello-merlion"),
        "stdout missing: {}",
        r.content
    );
    assert_eq!(r.name, "bash");
    assert_eq!(r.tool_call_id, "call_1");
}

#[tokio::test]
async fn bash_nonzero_exit_is_marked_error() {
    let r = Bash.call("call_2", json!({ "command": "exit 7" })).await;
    assert!(r.is_error, "expected non-zero exit to be reported as error");
    assert!(r.content.contains("[exit: 7]"));
}

#[tokio::test]
async fn bash_timeout_fires() {
    let r = Bash
        .call("call_3", json!({ "command": "sleep 5", "timeout_ms": 100 }))
        .await;
    assert!(r.is_error);
    assert!(r.content.contains("timed out"), "got: {}", r.content);
}
