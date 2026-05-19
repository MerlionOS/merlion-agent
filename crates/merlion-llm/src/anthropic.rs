//! Anthropic `/v1/messages` adapter.
//!
//! Maps merlion-core's OpenAI-shaped [`Message`] sequence into Anthropic's:
//! - top-level `system` string (concatenated from all Role::System turns)
//! - alternating user/assistant messages with content blocks
//! - tool_use / tool_result content blocks instead of `tool_calls`
//!
//! Streaming follows Anthropic's SSE protocol with named events
//! (`content_block_start`, `content_block_delta`, `message_delta`,
//! `message_stop`).

use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use merlion_core::{
    Error, LlmClient, LlmRequest, LlmStreamEvent, Message, Result, Role, ToolCall, Usage,
};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::warn;

const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct AnthropicClient {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    extra_headers: HeaderMap,
    /// Required by Anthropic; defaults to 4096 if `LlmRequest.max_tokens` is None.
    default_max_tokens: u32,
}

impl AnthropicClient {
    pub fn new(base_url: impl Into<String>, api_key: Option<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(600))
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| Error::Llm(format!("http build: {e}")))?;
        Ok(Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key,
            extra_headers: HeaderMap::new(),
            default_max_tokens: 4096,
        })
    }

    pub fn with_default_max_tokens(mut self, n: u32) -> Self {
        self.default_max_tokens = n;
        self
    }

    pub fn with_header(mut self, name: &'static str, value: &str) -> Result<Self> {
        let v = HeaderValue::from_str(value)
            .map_err(|e| Error::Llm(format!("invalid header value: {e}")))?;
        self.extra_headers.insert(name, v);
        Ok(self)
    }

    fn build_headers(&self) -> Result<HeaderMap> {
        let mut h = self.extra_headers.clone();
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        h.insert(
            "anthropic-version",
            HeaderValue::from_static(ANTHROPIC_VERSION),
        );
        if let Some(key) = &self.api_key {
            let v = HeaderValue::from_str(key)
                .map_err(|e| Error::Llm(format!("invalid api key: {e}")))?;
            h.insert("x-api-key", v);
        }
        Ok(h)
    }

    pub(crate) fn build_body(&self, req: &LlmRequest, stream: bool) -> Value {
        let (system, messages) = convert_messages(&req.messages);
        let mut body = json!({
            "model": req.model,
            "max_tokens": req.max_tokens.unwrap_or(self.default_max_tokens),
            "messages": messages,
            "stream": stream,
        });
        if !system.is_empty() {
            body["system"] = json!(system);
        }
        if !req.tools.is_empty() {
            let tools: Vec<Value> = req
                .tools
                .iter()
                .map(|s| {
                    json!({
                        "name": s.name,
                        "description": s.description,
                        "input_schema": s.parameters,
                    })
                })
                .collect();
            body["tools"] = Value::Array(tools);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        body
    }
}

/// Walk the merlion message list and produce (system_prompt, anthropic_messages).
/// Consecutive Tool messages collapse into one user turn with multiple
/// `tool_result` content blocks, as Anthropic requires.
pub(crate) fn convert_messages(messages: &[Message]) -> (String, Vec<Value>) {
    let mut system_parts: Vec<String> = Vec::new();
    let mut out: Vec<Value> = Vec::new();
    let mut pending_tool_results: Vec<Value> = Vec::new();

    fn flush_tool_results(out: &mut Vec<Value>, buf: &mut Vec<Value>) {
        if !buf.is_empty() {
            out.push(json!({
                "role": "user",
                "content": std::mem::take(buf),
            }));
        }
    }

    for m in messages {
        match m.role {
            Role::System => {
                if let Some(text) = &m.content {
                    system_parts.push(text.clone());
                }
            }
            Role::User => {
                flush_tool_results(&mut out, &mut pending_tool_results);
                if let Some(text) = &m.content {
                    out.push(json!({
                        "role": "user",
                        "content": [{ "type": "text", "text": text }],
                    }));
                }
            }
            Role::Assistant => {
                flush_tool_results(&mut out, &mut pending_tool_results);
                let mut blocks: Vec<Value> = Vec::new();
                if let Some(text) = m.content.as_deref().filter(|s| !s.is_empty()) {
                    blocks.push(json!({ "type": "text", "text": text }));
                }
                for tc in &m.tool_calls {
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.name,
                        "input": tc.arguments,
                    }));
                }
                if !blocks.is_empty() {
                    out.push(json!({
                        "role": "assistant",
                        "content": blocks,
                    }));
                }
            }
            Role::Tool => {
                let id = m.tool_call_id.clone().unwrap_or_default();
                let content = m.content.clone().unwrap_or_default();
                pending_tool_results.push(json!({
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": content,
                }));
            }
        }
    }
    flush_tool_results(&mut out, &mut pending_tool_results);
    (system_parts.join("\n\n"), out)
}

#[async_trait]
impl LlmClient for AnthropicClient {
    async fn stream(&self, req: LlmRequest) -> Result<BoxStream<'static, Result<LlmStreamEvent>>> {
        let url = format!("{}/messages", self.base_url);
        let body = self.build_body(&req, true);
        let headers = self.build_headers()?;

        let http = self.http.clone();
        let resp =
            crate::retry::send_with_retry(|| http.post(&url).headers(headers.clone()).json(&body))
                .await?;

        let stream = anthropic_sse_to_events(resp.bytes_stream()).boxed();
        Ok(stream)
    }
}

/// Parse Anthropic's named-event SSE protocol.
///
/// Per the API docs, content blocks are addressed by `index`; text blocks emit
/// `text_delta` events, tool_use blocks emit `input_json_delta` events whose
/// `partial_json` strings concatenate into a complete JSON object. The full
/// argument value is only known once `content_block_stop` arrives.
fn anthropic_sse_to_events<S>(
    bytes: S,
) -> impl futures::Stream<Item = Result<LlmStreamEvent>> + Send + 'static
where
    S: futures::Stream<Item = std::result::Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
{
    let mut buf = String::new();
    let mut blocks: Vec<BlockState> = Vec::new();
    let mut tool_calls_out: Vec<ToolCall> = Vec::new();
    let mut prompt_tokens: Option<u32> = None;
    let mut completion_tokens: Option<u32> = None;
    let mut finished = false;

    async_stream::stream! {
        futures::pin_mut!(bytes);
        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.map_err(|e| Error::Llm(format!("stream: {e}")))?;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(idx) = buf.find("\n\n") {
                let event: String = buf.drain(..idx + 2).collect();
                // We don't need the `event:` line — every `data:` JSON payload
                // carries a `type` field that's sufficient to dispatch.
                for line in event.lines() {
                    let Some(data) = line.strip_prefix("data:") else { continue };
                    let data = data.trim();
                    if data.is_empty() { continue; }
                    let parsed: Value = match serde_json::from_str(data) {
                        Ok(v) => v,
                        Err(e) => {
                            warn!(error = %e, data = %data, "failed to parse Anthropic SSE chunk");
                            continue;
                        }
                    };
                    let kind = parsed.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    match kind {
                        "message_start" => {
                            if let Some(input) = parsed.get("message")
                                .and_then(|m| m.get("usage"))
                                .and_then(|u| u.get("input_tokens"))
                                .and_then(|v| v.as_u64())
                            {
                                prompt_tokens = Some(input as u32);
                            }
                        }
                        "ping" => {}
                        "content_block_start" => {
                            let index = parsed.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                            let block = parsed.get("content_block").cloned().unwrap_or(Value::Null);
                            while blocks.len() <= index { blocks.push(BlockState::Skip); }
                            let btype = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            blocks[index] = match btype {
                                "text" => BlockState::Text,
                                "tool_use" => BlockState::ToolUse {
                                    id: block.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                    name: block.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                    args: String::new(),
                                },
                                _ => BlockState::Skip,
                            };
                        }
                        "content_block_delta" => {
                            let index = parsed.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                            let Some(delta) = parsed.get("delta") else { continue };
                            let dtype = delta.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            match (dtype, blocks.get_mut(index)) {
                                ("text_delta", _) => {
                                    if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                                        if !text.is_empty() {
                                            yield Ok(LlmStreamEvent::Delta(text.to_string()));
                                        }
                                    }
                                }
                                ("input_json_delta", Some(BlockState::ToolUse { args, .. })) => {
                                    if let Some(part) = delta.get("partial_json").and_then(|t| t.as_str()) {
                                        args.push_str(part);
                                    }
                                }
                                _ => {}
                            }
                        }
                        "content_block_stop" => {
                            let index = parsed.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                            if let Some(state) = blocks.get_mut(index) {
                                if let BlockState::ToolUse { id, name, args } = std::mem::replace(state, BlockState::Skip) {
                                    let arguments: Value = if args.trim().is_empty() {
                                        Value::Object(Default::default())
                                    } else {
                                        serde_json::from_str(&args).unwrap_or(Value::String(args))
                                    };
                                    tool_calls_out.push(ToolCall { id, name, arguments });
                                }
                            }
                        }
                        "message_delta" => {
                            if let Some(out) = parsed.get("usage")
                                .and_then(|u| u.get("output_tokens"))
                                .and_then(|v| v.as_u64())
                            {
                                completion_tokens = Some(out as u32);
                            }
                            if let Some(stop) = parsed.get("delta")
                                .and_then(|d| d.get("stop_reason"))
                                .and_then(|s| s.as_str())
                            {
                                if !tool_calls_out.is_empty() {
                                    let calls = std::mem::take(&mut tool_calls_out);
                                    yield Ok(LlmStreamEvent::ToolCalls(calls));
                                }
                                if prompt_tokens.is_some() || completion_tokens.is_some() {
                                    let total = match (prompt_tokens, completion_tokens) {
                                        (Some(p), Some(c)) => Some(p + c),
                                        _ => None,
                                    };
                                    yield Ok(LlmStreamEvent::Usage(Usage {
                                        prompt_tokens,
                                        completion_tokens,
                                        total_tokens: total,
                                    }));
                                }
                                yield Ok(LlmStreamEvent::Done(Some(stop.to_string())));
                                finished = true;
                            }
                        }
                        "message_stop" if !finished => {
                            if !tool_calls_out.is_empty() {
                                let calls = std::mem::take(&mut tool_calls_out);
                                yield Ok(LlmStreamEvent::ToolCalls(calls));
                            }
                            yield Ok(LlmStreamEvent::Done(None));
                            finished = true;
                        }
                        "error" => {
                            let msg = parsed.get("error")
                                .and_then(|e| e.get("message"))
                                .and_then(|m| m.as_str())
                                .unwrap_or("anthropic stream error")
                                .to_string();
                            yield Err(Error::Llm(msg));
                            return;
                        }
                        _ => {}
                    }
                }
            }
            if finished { break; }
        }
        if !finished {
            if !tool_calls_out.is_empty() {
                let calls = std::mem::take(&mut tool_calls_out);
                yield Ok(LlmStreamEvent::ToolCalls(calls));
            }
            yield Ok(LlmStreamEvent::Done(None));
        }
    }
}

#[derive(Debug)]
enum BlockState {
    Text,
    ToolUse {
        id: String,
        name: String,
        args: String,
    },
    Skip,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct AnthropicErrorBody {
    #[serde(rename = "type")]
    kind: String,
    error: AnthropicError,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct AnthropicError {
    #[serde(rename = "type")]
    kind: String,
    message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use merlion_core::{Message, ToolCall, ToolResult};
    use serde_json::json;

    #[test]
    fn system_messages_become_top_level_string() {
        let msgs = vec![
            Message::system("be brief"),
            Message::system("be kind"),
            Message::user("hi"),
        ];
        let (sys, out) = convert_messages(&msgs);
        assert_eq!(sys, "be brief\n\nbe kind");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(out[0]["content"][0]["text"], "hi");
    }

    #[test]
    fn assistant_with_tool_calls_emits_inline_tool_use_blocks() {
        let msgs = vec![
            Message::user("run ls"),
            Message::assistant_tool_calls(vec![ToolCall {
                id: "toolu_1".into(),
                name: "bash".into(),
                arguments: json!({"command": "ls"}),
            }]),
        ];
        let (_, out) = convert_messages(&msgs);
        let asst = &out[1];
        assert_eq!(asst["role"], "assistant");
        assert_eq!(asst["content"][0]["type"], "tool_use");
        assert_eq!(asst["content"][0]["id"], "toolu_1");
        assert_eq!(asst["content"][0]["input"]["command"], "ls");
    }

    #[test]
    fn consecutive_tool_results_collapse_to_one_user_message() {
        let msgs = vec![
            Message::user("run two"),
            Message::assistant_tool_calls(vec![
                ToolCall {
                    id: "a".into(),
                    name: "bash".into(),
                    arguments: json!({"command":"ls"}),
                },
                ToolCall {
                    id: "b".into(),
                    name: "bash".into(),
                    arguments: json!({"command":"pwd"}),
                },
            ]),
            Message::tool_response(ToolResult {
                tool_call_id: "a".into(),
                name: "bash".into(),
                content: "x".into(),
                is_error: false,
            }),
            Message::tool_response(ToolResult {
                tool_call_id: "b".into(),
                name: "bash".into(),
                content: "y".into(),
                is_error: false,
            }),
        ];
        let (_, out) = convert_messages(&msgs);
        let tool_turn = out.last().unwrap();
        assert_eq!(tool_turn["role"], "user");
        let blocks = tool_turn["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "tool_result");
        assert_eq!(blocks[0]["tool_use_id"], "a");
        assert_eq!(blocks[1]["tool_use_id"], "b");
    }

    #[test]
    fn body_carries_required_max_tokens_and_drops_authorization_header() {
        let client =
            AnthropicClient::new("https://api.anthropic.com/v1", Some("sk-test".into())).unwrap();
        let req = LlmRequest {
            model: "claude-opus-4-7".into(),
            messages: vec![Message::user("hi")],
            tools: vec![],
            temperature: Some(0.2),
            max_tokens: None,
        };
        let body = client.build_body(&req, true);
        assert_eq!(body["model"], "claude-opus-4-7");
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["stream"], true);
        let temp = body["temperature"].as_f64().unwrap();
        assert!((temp - 0.2).abs() < 1e-6, "got temperature = {temp}");

        let headers = client.build_headers().unwrap();
        assert_eq!(headers.get("x-api-key").unwrap(), "sk-test");
        assert_eq!(headers.get("anthropic-version").unwrap(), "2023-06-01");
        assert!(headers.get("authorization").is_none());
    }
}
