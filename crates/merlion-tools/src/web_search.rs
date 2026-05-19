//! `web_search` tool — pluggable backend.
//!
//! Currently supports Tavily (https://tavily.com) — a search API designed
//! for AI agents that returns clean snippets. Set `TAVILY_API_KEY` to
//! enable. Other backends (Brave, SerpAPI) are TODO behind the same trait.

use async_trait::async_trait;
use merlion_core::{Tool, ToolResult, ToolSchema};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

const TAVILY_URL: &str = "https://api.tavily.com/search";

#[derive(Default)]
pub struct WebSearch;

#[derive(Debug, Deserialize)]
struct Args {
    query: String,
    #[serde(default)]
    max_results: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct TavilyResponse {
    #[serde(default)]
    results: Vec<TavilyResult>,
    #[serde(default)]
    answer: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TavilyResult {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

#[async_trait]
impl Tool for WebSearch {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "web_search".into(),
            description: "Search the web and return a numbered list of titled snippets with URLs. \
                 Use this before `web_fetch` when you don't know the exact URL. \
                 Requires TAVILY_API_KEY env var."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "max_results": { "type": "integer", "description": "default 5, cap 20" }
                },
                "required": ["query"]
            }),
        }
    }

    async fn call(&self, call_id: &str, args: Value) -> ToolResult {
        let parsed: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return err(call_id, format!("invalid arguments: {e}")),
        };
        let api_key = match std::env::var("TAVILY_API_KEY") {
            Ok(k) => k,
            Err(_) => {
                return err(
                    call_id,
                    "TAVILY_API_KEY is not set. Sign up at https://tavily.com and export the key."
                        .into(),
                );
            }
        };
        let max = parsed.max_results.unwrap_or(5).min(20);
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
        {
            Ok(c) => c,
            Err(e) => return err(call_id, format!("http client: {e}")),
        };
        let resp = match client
            .post(TAVILY_URL)
            .json(&json!({
                "api_key": api_key,
                "query": parsed.query,
                "max_results": max,
                "include_answer": true,
                "search_depth": "basic",
            }))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return err(call_id, format!("request: {e}")),
        };
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return err(call_id, format!("tavily {status}: {body}"));
        }
        let parsed_resp: TavilyResponse = match resp.json().await {
            Ok(p) => p,
            Err(e) => return err(call_id, format!("tavily response: {e}")),
        };
        let mut out = String::new();
        if let Some(answer) = parsed_resp.answer {
            if !answer.is_empty() {
                out.push_str("Summary:\n");
                out.push_str(&answer);
                out.push_str("\n\nResults:\n");
            }
        }
        for (i, r) in parsed_resp.results.iter().enumerate() {
            let title = r.title.as_deref().unwrap_or("(untitled)");
            let url = r.url.as_deref().unwrap_or("");
            let snippet = r
                .content
                .as_deref()
                .unwrap_or("")
                .chars()
                .take(400)
                .collect::<String>();
            out.push_str(&format!("{}. {title}\n   {url}\n   {snippet}\n\n", i + 1));
        }
        if out.is_empty() {
            out = "(no results)".into();
        }
        ok(call_id, out)
    }
}

fn ok(call_id: &str, content: String) -> ToolResult {
    ToolResult {
        tool_call_id: call_id.into(),
        name: "web_search".into(),
        content,
        is_error: false,
    }
}

fn err(call_id: &str, msg: String) -> ToolResult {
    ToolResult {
        tool_call_id: call_id.into(),
        name: "web_search".into(),
        content: msg,
        is_error: true,
    }
}
