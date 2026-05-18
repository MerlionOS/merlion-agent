//! Google Gemini `generateContent` adapter.
//!
//! Differences from OpenAI / Anthropic worth knowing:
//! - Model name lives in the URL path (`models/<model>:streamGenerateContent`),
//!   not the request body.
//! - Roles are `user` and `model` — there is no `assistant`, and no `system`
//!   role; system prompts go to a top-level `system_instruction` field.
//! - Tool calls are inline `functionCall` parts inside `model` content; tool
//!   results are `functionResponse` parts inside `user` content. Gemini has
//!   **no concept of a `tool_call_id`** — it correlates calls and responses
//!   by function name. We synthesize ids on the way out so the merlion-core
//!   loop stays uniform, and ignore them on the way in.
//! - SSE streaming is unusually simple: every `data:` line is a full
//!   `GenerateContentResponse` fragment with concrete `parts[]` deltas, not
//!   incremental JSON like Anthropic's `input_json_delta`.

use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use merlion_core::{
    Error, LlmClient, LlmRequest, LlmStreamEvent, Message, Result, Role, ToolCall, Usage,
};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde_json::{json, Value};
use tracing::warn;

pub struct GeminiClient {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    extra_headers: HeaderMap,
}

impl GeminiClient {
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
        })
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
        if let Some(key) = &self.api_key {
            let v = HeaderValue::from_str(key)
                .map_err(|e| Error::Llm(format!("invalid api key: {e}")))?;
            h.insert("x-goog-api-key", v);
        }
        Ok(h)
    }

    pub(crate) fn build_url(&self, model: &str) -> String {
        format!("{}/models/{}:streamGenerateContent?alt=sse", self.base_url, model)
    }

    pub(crate) fn build_body(&self, req: &LlmRequest) -> Value {
        let (system, contents) = convert_messages(&req.messages);
        let mut body = json!({ "contents": contents });
        if !system.is_empty() {
            body["system_instruction"] = json!({ "parts": [{ "text": system }] });
        }
        if !req.tools.is_empty() {
            let decls: Vec<Value> = req
                .tools
                .iter()
                .map(|s| {
                    json!({
                        "name": s.name,
                        "description": s.description,
                        "parameters": sanitize_schema(s.parameters.clone()),
                    })
                })
                .collect();
            body["tools"] = json!([{ "functionDeclarations": decls }]);
        }
        let mut gen_cfg = serde_json::Map::new();
        if let Some(t) = req.temperature {
            gen_cfg.insert("temperature".into(), json!(t));
        }
        if let Some(m) = req.max_tokens {
            gen_cfg.insert("maxOutputTokens".into(), json!(m));
        }
        if !gen_cfg.is_empty() {
            body["generationConfig"] = Value::Object(gen_cfg);
        }
        body
    }
}

/// Strip JSON-schema keywords Gemini rejects. The list mirrors the errors
/// we've seen in practice — `default` and `$schema` are the common offenders
/// from our own tool schemas (e.g. `edit.replace_all` has `default: false`).
pub(crate) fn sanitize_schema(mut v: Value) -> Value {
    const BANNED: &[&str] =
        &["default", "$schema", "examples", "$ref", "definitions", "additionalProperties"];
    fn walk(v: &mut Value, banned: &[&str]) {
        match v {
            Value::Object(map) => {
                for b in banned {
                    map.remove(*b);
                }
                for (_, child) in map.iter_mut() {
                    walk(child, banned);
                }
            }
            Value::Array(arr) => {
                for child in arr.iter_mut() {
                    walk(child, banned);
                }
            }
            _ => {}
        }
    }
    walk(&mut v, BANNED);
    v
}

/// Convert merlion's OpenAI-shaped message list into Gemini's
/// `(system_instruction_text, contents[])` form. Consecutive Tool messages
/// collapse into one `user` turn carrying multiple `functionResponse` parts —
/// the same collapsing rule as Anthropic, since Gemini also requires
/// alternating user/model turns.
pub(crate) fn convert_messages(messages: &[Message]) -> (String, Vec<Value>) {
    let mut system_parts: Vec<String> = Vec::new();
    let mut out: Vec<Value> = Vec::new();
    let mut pending_responses: Vec<Value> = Vec::new();

    fn flush_responses(out: &mut Vec<Value>, buf: &mut Vec<Value>) {
        if !buf.is_empty() {
            out.push(json!({
                "role": "user",
                "parts": std::mem::take(buf),
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
                flush_responses(&mut out, &mut pending_responses);
                if let Some(text) = &m.content {
                    out.push(json!({
                        "role": "user",
                        "parts": [{ "text": text }],
                    }));
                }
            }
            Role::Assistant => {
                flush_responses(&mut out, &mut pending_responses);
                let mut parts: Vec<Value> = Vec::new();
                if let Some(text) = m.content.as_deref().filter(|s| !s.is_empty()) {
                    parts.push(json!({ "text": text }));
                }
                for tc in &m.tool_calls {
                    parts.push(json!({
                        "functionCall": { "name": tc.name, "args": tc.arguments }
                    }));
                }
                if !parts.is_empty() {
                    out.push(json!({
                        "role": "model",
                        "parts": parts,
                    }));
                }
            }
            Role::Tool => {
                // Gemini correlates by function name, not by id — the merlion
                // tool message already carries `name`, so we use it directly.
                let name = m.name.clone().unwrap_or_default();
                let content = m.content.clone().unwrap_or_default();
                let response: Value = serde_json::from_str(&content)
                    .unwrap_or_else(|_| json!({ "content": content }));
                pending_responses.push(json!({
                    "functionResponse": { "name": name, "response": response }
                }));
            }
        }
    }
    flush_responses(&mut out, &mut pending_responses);
    (system_parts.join("\n\n"), out)
}

#[async_trait]
impl LlmClient for GeminiClient {
    async fn stream(
        &self,
        req: LlmRequest,
    ) -> Result<BoxStream<'static, Result<LlmStreamEvent>>> {
        let url = self.build_url(&req.model);
        let body = self.build_body(&req);
        let headers = self.build_headers()?;

        let http = self.http.clone();
        let resp = crate::retry::send_with_retry(|| {
            http.post(&url).headers(headers.clone()).json(&body)
        })
        .await?;

        let stream = gemini_sse_to_events(resp.bytes_stream()).boxed();
        Ok(stream)
    }
}

fn gemini_sse_to_events<S>(
    bytes: S,
) -> impl futures::Stream<Item = Result<LlmStreamEvent>> + Send + 'static
where
    S: futures::Stream<Item = std::result::Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
{
    let mut buf = String::new();
    let mut pending_calls: Vec<ToolCall> = Vec::new();
    let mut call_counter: u32 = 0;
    let mut finished = false;

    async_stream::stream! {
        futures::pin_mut!(bytes);
        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.map_err(|e| Error::Llm(format!("stream: {e}")))?;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(idx) = buf.find("\n\n") {
                let event: String = buf.drain(..idx + 2).collect();
                for line in event.lines() {
                    let Some(data) = line.strip_prefix("data:") else { continue };
                    let data = data.trim();
                    if data.is_empty() { continue; }
                    let parsed: Value = match serde_json::from_str(data) {
                        Ok(v) => v,
                        Err(e) => {
                            warn!(error = %e, data = %data, "failed to parse Gemini SSE chunk");
                            continue;
                        }
                    };

                    let Some(candidate) = parsed
                        .get("candidates")
                        .and_then(|c| c.as_array())
                        .and_then(|a| a.first())
                    else {
                        // Error envelope or empty chunk.
                        if let Some(msg) = parsed.get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(|m| m.as_str())
                        {
                            yield Err(Error::Llm(msg.to_string()));
                            return;
                        }
                        continue;
                    };

                    if let Some(parts) = candidate
                        .get("content")
                        .and_then(|c| c.get("parts"))
                        .and_then(|p| p.as_array())
                    {
                        for part in parts {
                            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                if !text.is_empty() {
                                    yield Ok(LlmStreamEvent::Delta(text.to_string()));
                                }
                            } else if let Some(fc) = part.get("functionCall") {
                                let name = fc.get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let args = fc.get("args").cloned().unwrap_or(Value::Object(Default::default()));
                                let id = format!("gem_{}_{}", name, call_counter);
                                call_counter += 1;
                                pending_calls.push(ToolCall { id, name, arguments: args });
                            }
                        }
                    }

                    if let Some(reason) = candidate.get("finishReason").and_then(|r| r.as_str()) {
                        if !pending_calls.is_empty() {
                            let calls = std::mem::take(&mut pending_calls);
                            yield Ok(LlmStreamEvent::ToolCalls(calls));
                        }
                        if let Some(meta) = parsed.get("usageMetadata") {
                            let prompt = meta.get("promptTokenCount").and_then(|v| v.as_u64()).map(|n| n as u32);
                            let completion = meta.get("candidatesTokenCount").and_then(|v| v.as_u64()).map(|n| n as u32);
                            let total = meta.get("totalTokenCount").and_then(|v| v.as_u64()).map(|n| n as u32);
                            if prompt.is_some() || completion.is_some() || total.is_some() {
                                yield Ok(LlmStreamEvent::Usage(Usage {
                                    prompt_tokens: prompt,
                                    completion_tokens: completion,
                                    total_tokens: total,
                                }));
                            }
                        }
                        yield Ok(LlmStreamEvent::Done(Some(reason.to_string())));
                        finished = true;
                    }
                }
            }
            if finished { break; }
        }
        if !finished {
            if !pending_calls.is_empty() {
                let calls = std::mem::take(&mut pending_calls);
                yield Ok(LlmStreamEvent::ToolCalls(calls));
            }
            yield Ok(LlmStreamEvent::Done(None));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use merlion_core::{Message, ToolCall, ToolResult};
    use serde_json::json;

    #[test]
    fn system_messages_become_system_instruction() {
        let msgs =
            vec![Message::system("be brief"), Message::system("be kind"), Message::user("hi")];
        let (sys, out) = convert_messages(&msgs);
        assert_eq!(sys, "be brief\n\nbe kind");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(out[0]["parts"][0]["text"], "hi");
    }

    #[test]
    fn assistant_with_tool_calls_emits_function_call_parts_under_role_model() {
        let msgs = vec![
            Message::user("run ls"),
            Message::assistant_tool_calls(vec![ToolCall {
                id: "ignored".into(),
                name: "bash".into(),
                arguments: json!({"command": "ls"}),
            }]),
        ];
        let (_, out) = convert_messages(&msgs);
        let asst = &out[1];
        assert_eq!(asst["role"], "model");
        let fc = &asst["parts"][0]["functionCall"];
        assert_eq!(fc["name"], "bash");
        assert_eq!(fc["args"]["command"], "ls");
    }

    #[test]
    fn consecutive_tool_responses_collapse_into_one_user_turn() {
        let msgs = vec![
            Message::user("run two"),
            Message::assistant_tool_calls(vec![
                ToolCall { id: "a".into(), name: "bash".into(), arguments: json!({"command": "ls"}) },
                ToolCall { id: "b".into(), name: "bash".into(), arguments: json!({"command": "pwd"}) },
            ]),
            Message::tool_response(ToolResult { tool_call_id: "a".into(), name: "bash".into(), content: "x".into(), is_error: false }),
            Message::tool_response(ToolResult { tool_call_id: "b".into(), name: "bash".into(), content: "y".into(), is_error: false }),
        ];
        let (_, out) = convert_messages(&msgs);
        let turn = out.last().unwrap();
        assert_eq!(turn["role"], "user");
        let parts = turn["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["functionResponse"]["name"], "bash");
    }

    #[test]
    fn sanitize_schema_strips_default_recursively() {
        let s = json!({
            "type": "object",
            "properties": {
                "x": { "type": "boolean", "default": false },
                "nested": {
                    "type": "object",
                    "properties": { "y": { "type": "integer", "default": 5 } }
                }
            },
            "$schema": "https://json-schema.org/draft/2020-12/schema"
        });
        let cleaned = sanitize_schema(s);
        assert!(cleaned.get("$schema").is_none());
        assert!(cleaned["properties"]["x"].get("default").is_none());
        assert!(cleaned["properties"]["nested"]["properties"]["y"]
            .get("default")
            .is_none());
    }

    #[test]
    fn build_url_puts_model_in_path_with_sse_query() {
        let client = GeminiClient::new("https://generativelanguage.googleapis.com/v1beta", None).unwrap();
        assert_eq!(
            client.build_url("gemini-2.0-flash"),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-flash:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn build_body_carries_generation_config_and_drops_authorization_header() {
        let client = GeminiClient::new("https://generativelanguage.googleapis.com/v1beta", Some("k".into())).unwrap();
        let req = LlmRequest {
            model: "gemini-2.0-flash".into(),
            messages: vec![Message::user("hi")],
            tools: vec![],
            temperature: Some(0.7),
            max_tokens: Some(1024),
        };
        let body = client.build_body(&req);
        let temp = body["generationConfig"]["temperature"].as_f64().unwrap();
        assert!((temp - 0.7).abs() < 1e-6);
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 1024);
        let headers = client.build_headers().unwrap();
        assert_eq!(headers.get("x-goog-api-key").unwrap(), "k");
        assert!(headers.get("authorization").is_none());
    }
}
