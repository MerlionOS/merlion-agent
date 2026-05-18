//! AWS Bedrock adapter for Anthropic Claude models.
//!
//! Bedrock exposes Anthropic's Messages API at
//! `POST https://bedrock-runtime.<region>.amazonaws.com/model/<MODEL_ID>/invoke`,
//! gated by SigV4 auth against the `bedrock` service. The request body shape
//! is exactly Anthropic's Messages API except `anthropic_version` is the
//! literal string `"bedrock-2023-05-31"` (not the upstream date).
//!
//! We hand-roll SigV4 here rather than pulling in `aws-sdk-*` (80+ transitive
//! deps). The `/invoke` endpoint is non-streaming; the `LlmClient` trait
//! requires a stream, so we fake a single-chunk "stream" that emits the full
//! decoded response as one Delta + one ToolCalls + one Usage + one Done.

use std::env;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::{self, BoxStream, StreamExt};
use hmac::{Hmac, Mac};
use merlion_core::{
    Error, LlmClient, LlmRequest, LlmStreamEvent, Message, Result, Role, ToolCall, Usage,
};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const BEDROCK_ANTHROPIC_VERSION: &str = "bedrock-2023-05-31";
const SERVICE: &str = "bedrock";
const DEFAULT_REGION: &str = "us-east-1";
const DEFAULT_MAX_TOKENS: u32 = 4096;

pub struct BedrockClient {
    http: reqwest::Client,
    region: String,
    access_key: String,
    secret_key: String,
    session_token: Option<String>,
}

impl BedrockClient {
    /// Reads `AWS_REGION` (default `us-east-1`), `AWS_ACCESS_KEY_ID`,
    /// `AWS_SECRET_ACCESS_KEY`, and optional `AWS_SESSION_TOKEN` from env.
    pub fn from_env() -> Result<Self> {
        let region = env::var("AWS_REGION")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_REGION.to_string());
        let access_key = env::var("AWS_ACCESS_KEY_ID")
            .map_err(|_| Error::Llm("AWS_ACCESS_KEY_ID not set".into()))?;
        let secret_key = env::var("AWS_SECRET_ACCESS_KEY")
            .map_err(|_| Error::Llm("AWS_SECRET_ACCESS_KEY not set".into()))?;
        let mut client = Self::new(region, access_key, secret_key)?;
        if let Ok(token) = env::var("AWS_SESSION_TOKEN") {
            if !token.is_empty() {
                client = client.with_session_token(token);
            }
        }
        Ok(client)
    }

    pub fn new(
        region: impl Into<String>,
        access_key: impl Into<String>,
        secret_key: impl Into<String>,
    ) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(600))
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| Error::Llm(format!("http build: {e}")))?;
        Ok(Self {
            http,
            region: region.into(),
            access_key: access_key.into(),
            secret_key: secret_key.into(),
            session_token: None,
        })
    }

    pub fn with_session_token(mut self, token: impl Into<String>) -> Self {
        self.session_token = Some(token.into());
        self
    }

    fn host(&self) -> String {
        format!("bedrock-runtime.{}.amazonaws.com", self.region)
    }

    fn invoke_url(&self, model: &str) -> String {
        format!(
            "https://{}/model/{}/invoke",
            self.host(),
            urlencode_model(model)
        )
    }
}

/// Build the JSON body Bedrock expects: Anthropic Messages API shape with
/// the literal `"bedrock-2023-05-31"` version string.
pub fn build_invoke_body(req: &LlmRequest) -> Value {
    let (system, messages) = convert_messages(&req.messages);
    let mut body = json!({
        "anthropic_version": BEDROCK_ANTHROPIC_VERSION,
        "max_tokens": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        "messages": messages,
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

/// Walk the merlion message list and produce (system_prompt, anthropic_messages).
/// Logic mirrors `anthropic.rs::convert_messages`; we keep a private copy here
/// rather than depending on that fn so the two adapters stay independent.
fn convert_messages(messages: &[Message]) -> (String, Vec<Value>) {
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
impl LlmClient for BedrockClient {
    async fn stream(
        &self,
        req: LlmRequest,
    ) -> Result<BoxStream<'static, Result<LlmStreamEvent>>> {
        let url = self.invoke_url(&req.model);
        let path = format!("/model/{}/invoke", urlencode_model(&req.model));
        let body = build_invoke_body(&req);
        let body_bytes = serde_json::to_vec(&body)?;
        let host = self.host();

        let now = Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_stamp = now.format("%Y%m%d").to_string();

        let payload_hash = hex_sha256(&body_bytes);

        let auth = SigV4Inputs {
            method: "POST",
            canonical_uri: &path,
            canonical_query: "",
            host: &host,
            amz_date: &amz_date,
            date_stamp: &date_stamp,
            region: &self.region,
            service: SERVICE,
            access_key: &self.access_key,
            secret_key: &self.secret_key,
            session_token: self.session_token.as_deref(),
            payload_hash: &payload_hash,
        }
        .sign();

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            HeaderName::from_static("host"),
            HeaderValue::from_str(&host).map_err(|e| Error::Llm(format!("host header: {e}")))?,
        );
        headers.insert(
            HeaderName::from_static("x-amz-date"),
            HeaderValue::from_str(&amz_date)
                .map_err(|e| Error::Llm(format!("x-amz-date header: {e}")))?,
        );
        headers.insert(
            HeaderName::from_static("x-amz-content-sha256"),
            HeaderValue::from_str(&payload_hash)
                .map_err(|e| Error::Llm(format!("x-amz-content-sha256 header: {e}")))?,
        );
        if let Some(token) = &self.session_token {
            headers.insert(
                HeaderName::from_static("x-amz-security-token"),
                HeaderValue::from_str(token)
                    .map_err(|e| Error::Llm(format!("x-amz-security-token header: {e}")))?,
            );
        }
        headers.insert(
            reqwest::header::AUTHORIZATION,
            HeaderValue::from_str(&auth.authorization)
                .map_err(|e| Error::Llm(format!("authorization header: {e}")))?,
        );

        let http = self.http.clone();
        let body_bytes_for_send = body_bytes.clone();
        let resp = crate::retry::send_with_retry(|| {
            http.post(&url)
                .headers(headers.clone())
                .body(body_bytes_for_send.clone())
        })
        .await?;

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Llm(format!("bedrock body: {e}")))?;
        let parsed: Value = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Llm(format!("bedrock json: {e}")))?;

        let events = invoke_response_to_events(parsed);
        Ok(stream::iter(events.into_iter().map(Ok)).boxed())
    }
}

/// Translate a Bedrock /invoke response body into the trait's event stream.
/// Order: Delta (if any text), ToolCalls (if any tool_use), Usage (if reported),
/// Done(stop_reason).
fn invoke_response_to_events(body: Value) -> Vec<LlmStreamEvent> {
    let mut out: Vec<LlmStreamEvent> = Vec::new();
    let mut text_buf = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();

    if let Some(blocks) = body.get("content").and_then(|c| c.as_array()) {
        for block in blocks {
            let btype = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match btype {
                "text" => {
                    if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                        text_buf.push_str(t);
                    }
                }
                "tool_use" => {
                    let id = block
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = block
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let arguments = block.get("input").cloned().unwrap_or(Value::Null);
                    tool_calls.push(ToolCall {
                        id,
                        name,
                        arguments,
                    });
                }
                _ => {}
            }
        }
    }

    if !text_buf.is_empty() {
        out.push(LlmStreamEvent::Delta(text_buf));
    }
    if !tool_calls.is_empty() {
        out.push(LlmStreamEvent::ToolCalls(tool_calls));
    }

    if let Some(u) = body.get("usage") {
        let prompt_tokens = u
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);
        let completion_tokens = u
            .get("output_tokens")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);
        if prompt_tokens.is_some() || completion_tokens.is_some() {
            let total = match (prompt_tokens, completion_tokens) {
                (Some(p), Some(c)) => Some(p + c),
                _ => None,
            };
            out.push(LlmStreamEvent::Usage(Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens: total,
            }));
        }
    }

    let stop = body
        .get("stop_reason")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    out.push(LlmStreamEvent::Done(stop));
    out
}

// --- SigV4 -------------------------------------------------------------------

pub struct SigV4Inputs<'a> {
    pub method: &'a str,
    pub canonical_uri: &'a str,
    pub canonical_query: &'a str,
    pub host: &'a str,
    pub amz_date: &'a str,
    pub date_stamp: &'a str,
    pub region: &'a str,
    pub service: &'a str,
    pub access_key: &'a str,
    pub secret_key: &'a str,
    pub session_token: Option<&'a str>,
    pub payload_hash: &'a str,
}

pub struct SigV4Output {
    pub authorization: String,
    pub signature: String,
    #[allow(dead_code)]
    pub canonical_request: String,
    #[allow(dead_code)]
    pub string_to_sign: String,
    #[allow(dead_code)]
    pub signed_headers: String,
}

impl<'a> SigV4Inputs<'a> {
    pub fn sign(&self) -> SigV4Output {
        // Headers we sign — lowercase names, sorted lexicographically.
        // Bedrock's minimum: host, x-amz-content-sha256, x-amz-date,
        // plus x-amz-security-token if present.
        let mut signed: Vec<(&str, &str)> = vec![
            ("host", self.host),
            ("x-amz-content-sha256", self.payload_hash),
            ("x-amz-date", self.amz_date),
        ];
        if let Some(tok) = self.session_token {
            signed.push(("x-amz-security-token", tok));
        }
        signed.sort_by(|a, b| a.0.cmp(b.0));

        let canonical_headers: String = signed
            .iter()
            .map(|(k, v)| format!("{}:{}\n", k, v.trim()))
            .collect();
        let signed_headers = signed
            .iter()
            .map(|(k, _)| *k)
            .collect::<Vec<_>>()
            .join(";");

        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            self.method,
            self.canonical_uri,
            self.canonical_query,
            canonical_headers,
            signed_headers,
            self.payload_hash,
        );

        let scope = format!(
            "{}/{}/{}/aws4_request",
            self.date_stamp, self.region, self.service
        );
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            self.amz_date,
            scope,
            hex_sha256(canonical_request.as_bytes()),
        );

        let k_date = hmac_sha256(
            format!("AWS4{}", self.secret_key).as_bytes(),
            self.date_stamp.as_bytes(),
        );
        let k_region = hmac_sha256(&k_date, self.region.as_bytes());
        let k_service = hmac_sha256(&k_region, self.service.as_bytes());
        let k_signing = hmac_sha256(&k_service, b"aws4_request");
        let signature = hex(&hmac_sha256(&k_signing, string_to_sign.as_bytes()));

        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
            self.access_key, scope, signed_headers, signature,
        );

        SigV4Output {
            authorization,
            signature,
            canonical_request,
            string_to_sign,
            signed_headers,
        }
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex(&hasher.finalize())
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key)
        .expect("HMAC accepts arbitrary-length keys");
    mac.update(msg);
    mac.finalize().into_bytes().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// Bedrock accepts model IDs verbatim — including the `:` in version suffixes
/// like `claude-3-5-sonnet-20241022-v2:0`. reqwest's URL builder would
/// percent-encode `:` in the path; we leave it alone to match what Bedrock
/// expects. The canonical URI passed to SigV4 must match exactly what's on the
/// wire, so the same string flows into both.
fn urlencode_model(model: &str) -> String {
    model.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use merlion_core::Message;

    #[test]
    fn body_carries_bedrock_anthropic_version() {
        let req = LlmRequest {
            model: "anthropic.claude-3-5-sonnet-20241022-v2:0".into(),
            messages: vec![Message::system("be brief"), Message::user("hi")],
            tools: vec![],
            temperature: Some(0.4),
            max_tokens: Some(1024),
        };
        let body = build_invoke_body(&req);
        assert_eq!(body["anthropic_version"], "bedrock-2023-05-31");
        assert_eq!(body["max_tokens"], 1024);
        assert_eq!(body["system"], "be brief");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"][0]["text"], "hi");
        let temp = body["temperature"].as_f64().unwrap();
        assert!((temp - 0.4).abs() < 1e-6);
        // No `model` field — model goes in the URL for Bedrock.
        assert!(body.get("model").is_none());
        // No `stream` field — we always hit /invoke (non-streaming).
        assert!(body.get("stream").is_none());
    }

    #[test]
    fn invoke_response_translates_to_events() {
        let body = json!({
            "id": "msg_01abc",
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "text", "text": "Sure, "},
                {"type": "text", "text": "running it."},
                {"type": "tool_use", "id": "tu_1", "name": "bash", "input": {"command": "ls"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 42, "output_tokens": 7}
        });
        let events = invoke_response_to_events(body);
        assert_eq!(events.len(), 4);
        match &events[0] {
            LlmStreamEvent::Delta(s) => assert_eq!(s, "Sure, running it."),
            _ => panic!("expected Delta first"),
        }
        match &events[1] {
            LlmStreamEvent::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].id, "tu_1");
                assert_eq!(calls[0].name, "bash");
                assert_eq!(calls[0].arguments["command"], "ls");
            }
            _ => panic!("expected ToolCalls"),
        }
        match &events[2] {
            LlmStreamEvent::Usage(u) => {
                assert_eq!(u.prompt_tokens, Some(42));
                assert_eq!(u.completion_tokens, Some(7));
                assert_eq!(u.total_tokens, Some(49));
            }
            _ => panic!("expected Usage"),
        }
        match &events[3] {
            LlmStreamEvent::Done(reason) => assert_eq!(reason.as_deref(), Some("tool_use")),
            _ => panic!("expected Done"),
        }
    }
}
