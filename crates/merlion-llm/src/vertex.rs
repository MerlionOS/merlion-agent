//! Google Vertex AI adapter.
//!
//! Wire format is identical to Gemini Studio (`system_instruction`,
//! `contents`, `functionCall`/`functionResponse` parts, SSE-as-full-fragment).
//! The two differences are:
//!
//! 1. The URL is region- and project-scoped:
//!    `https://{region}-aiplatform.googleapis.com/v1/projects/{project}/
//!    locations/{region}/publishers/google/models/{model}:streamGenerateContent?alt=sse`.
//! 2. Auth is OAuth Bearer rather than an API key. We shell out to
//!    `gcloud auth print-access-token` instead of pulling in `yup-oauth2`,
//!    which keeps the dep surface (and the binary) small at the cost of
//!    requiring `gcloud` on PATH.
//!
//! The body builder, schema sanitizer, and SSE parser are copied from
//! `gemini.rs` rather than shared, because the modules deliberately don't
//! expose those helpers publicly — keeping the providers independent means
//! we can evolve one without churning the other.

use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use merlion_core::{
    Error, LlmClient, LlmRequest, LlmStreamEvent, Message, Result, Role, ToolCall, Usage,
};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};
use tokio::process::Command;
use tracing::warn;

const DEFAULT_REGION: &str = "us-central1";

pub struct VertexClient {
    http: reqwest::Client,
    project: String,
    region: String,
    /// Override base URL host (everything before `/v1/projects/...`). Used by
    /// tests to point at a fixture server. When `None` the URL is built from
    /// `{region}-aiplatform.googleapis.com`.
    base_override: Option<String>,
    /// When set, skip the `gcloud` shellout and use this token directly.
    /// Test-only — callers in production should leave this `None`.
    token_override: Option<String>,
}

impl VertexClient {
    pub fn new(project: impl Into<String>, region: impl Into<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(600))
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| Error::Llm(format!("http build: {e}")))?;
        let region = region.into();
        let region = if region.is_empty() { DEFAULT_REGION.to_string() } else { region };
        Ok(Self {
            http,
            project: project.into(),
            region,
            base_override: None,
            token_override: None,
        })
    }

    /// Read project from `GOOGLE_CLOUD_PROJECT` (falling back to `GCP_PROJECT`)
    /// and region from `GOOGLE_CLOUD_REGION` (defaulting to `us-central1`).
    pub fn from_env() -> Result<Self> {
        let project = std::env::var("GOOGLE_CLOUD_PROJECT")
            .or_else(|_| std::env::var("GCP_PROJECT"))
            .map_err(|_| {
                Error::Llm(
                    "Vertex AI requires GOOGLE_CLOUD_PROJECT or GCP_PROJECT to be set".into(),
                )
            })?;
        let region =
            std::env::var("GOOGLE_CLOUD_REGION").unwrap_or_else(|_| DEFAULT_REGION.to_string());
        Self::new(project, region)
    }

    /// Inject a pre-acquired bearer token, bypassing the `gcloud` shellout.
    /// Intended for tests; production code should rely on `gcloud`.
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token_override = Some(token.into());
        self
    }

    /// Override the URL host (everything before `/v1/projects/...`). Tests
    /// point this at `http://127.0.0.1:<port>` to drive a fixture server.
    pub fn with_base_url(mut self, base: impl Into<String>) -> Self {
        let mut s = base.into();
        while s.ends_with('/') {
            s.pop();
        }
        self.base_override = Some(s);
        self
    }

    pub(crate) fn build_url(&self, model: &str) -> String {
        match &self.base_override {
            Some(base) => build_url_with_base(base, &self.project, &self.region, model),
            None => build_url(&self.project, &self.region, model),
        }
    }

    pub(crate) fn build_body(&self, req: &LlmRequest) -> Value {
        build_body(req)
    }

    async fn acquire_token(&self) -> Result<String> {
        if let Some(t) = &self.token_override {
            return Ok(t.clone());
        }
        let out = Command::new("gcloud")
            .arg("auth")
            .arg("print-access-token")
            .output()
            .await
            .map_err(|e| {
                Error::Llm(format!(
                    "gcloud CLI not available or not authenticated (failed to spawn `gcloud auth print-access-token`): {e}"
                ))
            })?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(Error::Llm(format!(
                "gcloud CLI not available or not authenticated (`gcloud auth print-access-token` exited {}): {}",
                out.status,
                stderr.trim()
            )));
        }
        let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if token.is_empty() {
            return Err(Error::Llm(
                "gcloud CLI not available or not authenticated (empty access token)".into(),
            ));
        }
        Ok(token)
    }
}

/// Free-standing URL builder so tests can exercise it without instantiating
/// a client. Mirrors the production path exactly.
pub fn build_url(project: &str, region: &str, model: &str) -> String {
    format!(
        "https://{region}-aiplatform.googleapis.com/v1/projects/{project}/locations/{region}/publishers/google/models/{model}:streamGenerateContent?alt=sse"
    )
}

fn build_url_with_base(base: &str, project: &str, region: &str, model: &str) -> String {
    format!(
        "{base}/v1/projects/{project}/locations/{region}/publishers/google/models/{model}:streamGenerateContent?alt=sse"
    )
}

pub fn build_body(req: &LlmRequest) -> Value {
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

/// Strip JSON-schema keywords Vertex rejects. Same set as Gemini Studio —
/// the underlying model accepts the same dialect.
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

/// Convert merlion's OpenAI-shaped message list into Vertex's
/// `(system_instruction_text, contents[])` form. Behaviour matches the
/// Gemini Studio converter — Vertex shares the wire format.
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
impl LlmClient for VertexClient {
    async fn stream(
        &self,
        req: LlmRequest,
    ) -> Result<BoxStream<'static, Result<LlmStreamEvent>>> {
        let token = self.acquire_token().await?;
        let url = self.build_url(&req.model);
        let body = self.build_body(&req);

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let bearer = format!("Bearer {token}");
        let auth = HeaderValue::from_str(&bearer)
            .map_err(|e| Error::Llm(format!("invalid bearer token: {e}")))?;
        headers.insert(AUTHORIZATION, auth);

        let http = self.http.clone();
        let resp = crate::retry::send_with_retry(|| {
            http.post(&url).headers(headers.clone()).json(&body)
        })
        .await?;

        let stream = vertex_sse_to_events(resp.bytes_stream()).boxed();
        Ok(stream)
    }
}

fn vertex_sse_to_events<S>(
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
                            warn!(error = %e, data = %data, "failed to parse Vertex SSE chunk");
                            continue;
                        }
                    };

                    let Some(candidate) = parsed
                        .get("candidates")
                        .and_then(|c| c.as_array())
                        .and_then(|a| a.first())
                    else {
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
                                let id = format!("vtx_{}_{}", name, call_counter);
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
    use merlion_core::{Message, ToolCall, ToolSchema};
    use serde_json::json;

    #[test]
    fn build_url_uses_region_project_and_model() {
        let url = build_url("my-proj", "us-central1", "gemini-2.0-flash-001");
        assert_eq!(
            url,
            "https://us-central1-aiplatform.googleapis.com/v1/projects/my-proj/locations/us-central1/publishers/google/models/gemini-2.0-flash-001:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn build_url_honours_alternate_region() {
        let url = build_url("p", "europe-west4", "gemini-1.5-pro");
        assert!(url.starts_with("https://europe-west4-aiplatform.googleapis.com/"));
        assert!(url.contains("/projects/p/locations/europe-west4/"));
    }

    #[test]
    fn client_default_region_is_us_central1() {
        let c = VertexClient::new("p", "").unwrap();
        assert_eq!(c.region, "us-central1");
    }

    #[test]
    fn build_body_emits_contents_without_system_when_none() {
        let req = LlmRequest {
            model: "gemini-2.0-flash-001".into(),
            messages: vec![Message::user("hi")],
            tools: vec![],
            temperature: None,
            max_tokens: None,
        };
        let body = build_body(&req);
        assert!(body.get("contents").is_some());
        assert!(body.get("system_instruction").is_none());
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn build_body_promotes_system_messages_to_system_instruction() {
        let req = LlmRequest {
            model: "gemini-2.0-flash-001".into(),
            messages: vec![Message::system("be brief"), Message::user("hi")],
            tools: vec![],
            temperature: None,
            max_tokens: None,
        };
        let body = build_body(&req);
        assert_eq!(body["system_instruction"]["parts"][0]["text"], "be brief");
    }

    #[test]
    fn build_body_wraps_tools_in_function_declarations() {
        let tool = ToolSchema {
            name: "bash".into(),
            description: "run a shell command".into(),
            parameters: json!({
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "$schema": "https://json-schema.org/draft/2020-12/schema"
            }),
        };
        let req = LlmRequest {
            model: "gemini-2.0-flash-001".into(),
            messages: vec![Message::user("ls")],
            tools: vec![tool],
            temperature: Some(0.5),
            max_tokens: Some(64),
        };
        let body = build_body(&req);
        let decl = &body["tools"][0]["functionDeclarations"][0];
        assert_eq!(decl["name"], "bash");
        assert_eq!(decl["description"], "run a shell command");
        // sanitize_schema must have stripped $schema.
        assert!(decl["parameters"].get("$schema").is_none());
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 64);
    }

    #[test]
    fn convert_messages_collapses_consecutive_tool_responses() {
        use merlion_core::ToolResult;
        let msgs = vec![
            Message::user("two"),
            Message::assistant_tool_calls(vec![
                ToolCall {
                    id: "a".into(),
                    name: "bash".into(),
                    arguments: json!({"command": "ls"}),
                },
                ToolCall {
                    id: "b".into(),
                    name: "bash".into(),
                    arguments: json!({"command": "pwd"}),
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
        let turn = out.last().unwrap();
        assert_eq!(turn["role"], "user");
        assert_eq!(turn["parts"].as_array().unwrap().len(), 2);
    }
}
