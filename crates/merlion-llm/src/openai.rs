use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use merlion_core::{
    Error, LlmClient, LlmRequest, LlmStreamEvent, Message, Result, Role, ToolCall, Usage,
};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::warn;

/// An OpenAI-compatible chat-completions client.
pub struct OpenAiClient {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    extra_headers: HeaderMap,
}

impl OpenAiClient {
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
            let v = HeaderValue::from_str(&format!("Bearer {key}"))
                .map_err(|e| Error::Llm(format!("invalid api key: {e}")))?;
            h.insert(AUTHORIZATION, v);
        }
        Ok(h)
    }

    fn build_body(&self, req: &LlmRequest, stream: bool) -> Value {
        let messages: Vec<Value> = req.messages.iter().map(message_to_json).collect();
        let mut body = json!({
            "model": req.model,
            "messages": messages,
            "stream": stream,
        });
        if stream {
            body["stream_options"] = json!({ "include_usage": true });
        }
        if !req.tools.is_empty() {
            let tools: Vec<Value> = req
                .tools
                .iter()
                .map(|s| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": s.name,
                            "description": s.description,
                            "parameters": s.parameters,
                        }
                    })
                })
                .collect();
            body["tools"] = Value::Array(tools);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(m) = req.max_tokens {
            body["max_tokens"] = json!(m);
        }
        body
    }
}

fn message_to_json(m: &Message) -> Value {
    let role = match m.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };
    let mut v = json!({ "role": role });
    if let Some(c) = &m.content {
        v["content"] = json!(c);
    } else if matches!(m.role, Role::Assistant) && !m.tool_calls.is_empty() {
        v["content"] = Value::Null;
    }
    if !m.tool_calls.is_empty() {
        v["tool_calls"] = Value::Array(
            m.tool_calls
                .iter()
                .map(|tc| {
                    json!({
                        "id": tc.id,
                        "type": "function",
                        "function": {
                            "name": tc.name,
                            "arguments": tc.arguments.to_string(),
                        }
                    })
                })
                .collect(),
        );
    }
    if let Some(id) = &m.tool_call_id {
        v["tool_call_id"] = json!(id);
    }
    if let Some(n) = &m.name {
        v["name"] = json!(n);
    }
    v
}

#[async_trait]
impl LlmClient for OpenAiClient {
    async fn stream(&self, req: LlmRequest) -> Result<BoxStream<'static, Result<LlmStreamEvent>>> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = self.build_body(&req, true);
        let headers = self.build_headers()?;

        let http = self.http.clone();
        let resp =
            crate::retry::send_with_retry(|| http.post(&url).headers(headers.clone()).json(&body))
                .await?;

        let stream = sse_to_events(resp.bytes_stream()).boxed();
        Ok(stream)
    }
}

/// Convert a bytes stream of `text/event-stream` data into LLM events.
fn sse_to_events<S>(
    bytes: S,
) -> impl futures::Stream<Item = Result<LlmStreamEvent>> + Send + 'static
where
    S: futures::Stream<Item = std::result::Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
{
    let mut buf = String::new();
    let mut pending_calls: Vec<PartialToolCall> = Vec::new();
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
                    if data == "[DONE]" {
                        finished = true;
                        continue;
                    }
                    if data.is_empty() { continue; }
                    let parsed: Chunk = match serde_json::from_str(data) {
                        Ok(c) => c,
                        Err(e) => {
                            warn!(error = %e, data = %data, "failed to parse SSE chunk");
                            continue;
                        }
                    };
                    if let Some(u) = parsed.usage {
                        yield Ok(LlmStreamEvent::Usage(Usage {
                            prompt_tokens: u.prompt_tokens,
                            completion_tokens: u.completion_tokens,
                            total_tokens: u.total_tokens,
                        }));
                    }
                    let Some(choice) = parsed.choices.into_iter().next() else { continue };
                    let delta = choice.delta;
                    if let Some(text) = delta.content {
                        if !text.is_empty() {
                            yield Ok(LlmStreamEvent::Delta(text));
                        }
                    }
                    if let Some(tcs) = delta.tool_calls {
                        for piece in tcs {
                            let i = piece.index as usize;
                            while pending_calls.len() <= i {
                                pending_calls.push(PartialToolCall::default());
                            }
                            let entry = &mut pending_calls[i];
                            if let Some(id) = piece.id { entry.id = Some(id); }
                            if let Some(f) = piece.function {
                                if let Some(name) = f.name {
                                    entry.name = Some(name);
                                }
                                if let Some(args) = f.arguments {
                                    entry.arguments.push_str(&args);
                                }
                            }
                        }
                    }
                    if let Some(reason) = choice.finish_reason {
                        if !pending_calls.is_empty() {
                            let calls = std::mem::take(&mut pending_calls)
                                .into_iter()
                                .enumerate()
                                .filter_map(|(i, p)| p.finalize(i))
                                .collect::<Vec<_>>();
                            if !calls.is_empty() {
                                yield Ok(LlmStreamEvent::ToolCalls(calls));
                            }
                        }
                        yield Ok(LlmStreamEvent::Done(Some(reason)));
                        finished = true;
                    }
                }
            }
            if finished { break; }
        }
        if !finished {
            if !pending_calls.is_empty() {
                let calls = std::mem::take(&mut pending_calls)
                    .into_iter()
                    .enumerate()
                    .filter_map(|(i, p)| p.finalize(i))
                    .collect::<Vec<_>>();
                if !calls.is_empty() {
                    yield Ok(LlmStreamEvent::ToolCalls(calls));
                }
            }
            yield Ok(LlmStreamEvent::Done(None));
        }
    }
}

#[derive(Default)]
struct PartialToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

impl PartialToolCall {
    fn finalize(self, idx: usize) -> Option<ToolCall> {
        let name = self.name?;
        let id = self.id.unwrap_or_else(|| format!("call_{idx}"));
        let args: Value = if self.arguments.trim().is_empty() {
            Value::Object(Default::default())
        } else {
            serde_json::from_str(&self.arguments).unwrap_or(Value::String(self.arguments))
        };
        Some(ToolCall {
            id,
            name,
            arguments: args,
        })
    }
}

#[derive(Debug, Deserialize)]
struct Chunk {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<UsageBlock>,
}

#[derive(Debug, Deserialize)]
struct UsageBlock {
    #[serde(default)]
    prompt_tokens: Option<u32>,
    #[serde(default)]
    completion_tokens: Option<u32>,
    #[serde(default)]
    total_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    #[serde(default)]
    delta: Delta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<DeltaToolCall>>,
}

#[derive(Debug, Deserialize)]
struct DeltaToolCall {
    index: u32,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<DeltaFn>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct DeltaFn {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}
