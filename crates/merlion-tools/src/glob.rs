use std::path::PathBuf;

use async_trait::async_trait;
use merlion_core::{Tool, ToolResult, ToolSchema};
use serde::Deserialize;
use serde_json::json;

#[derive(Default)]
pub struct Glob;

#[derive(Debug, Deserialize)]
struct Args {
    pattern: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    max_results: Option<usize>,
}

const DEFAULT_MAX_RESULTS: usize = 500;

#[async_trait]
impl Tool for Glob {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "glob".into(),
            description:
                "Find files by shell-style glob pattern (e.g. `**/*.rs`, `src/**/*.{ts,tsx}`). \
                 Returns matching paths one per line, sorted alphabetically. \
                 Use `cwd` to anchor the search; default cap is 500 results."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Shell-style glob pattern, e.g. `**/*.rs`." },
                    "cwd": { "type": "string", "description": "Base directory. Defaults to `.`." },
                    "max_results": { "type": "integer", "description": "Maximum number of results to return. Default 500." }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn call(&self, call_id: &str, args: serde_json::Value) -> ToolResult {
        let parsed: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return err(call_id, format!("invalid arguments: {e}")),
        };
        let max_results = parsed.max_results.unwrap_or(DEFAULT_MAX_RESULTS);

        // Resolve the cwd. If unset, use ".". Convert to absolute if it isn't,
        // so output paths are unambiguous.
        let cwd_str = parsed.cwd.unwrap_or_else(|| ".".to_string());
        let cwd_path = PathBuf::from(&cwd_str);
        let abs_cwd = if cwd_path.is_absolute() {
            cwd_path
        } else {
            match std::env::current_dir() {
                Ok(c) => c.join(&cwd_path),
                Err(e) => return err(call_id, format!("failed to read current_dir: {e}")),
            }
        };

        // Join cwd + pattern with '/'.
        let full_pattern = format!("{}/{}", abs_cwd.display(), parsed.pattern);

        let iter = match glob::glob(&full_pattern) {
            Ok(i) => i,
            Err(e) => return err(call_id, format!("invalid glob pattern: {e}")),
        };

        let mut matches: Vec<String> = Vec::new();
        let mut total_seen: usize = 0;
        let mut truncated = false;
        for entry in iter {
            match entry {
                Ok(path) => {
                    total_seen += 1;
                    if matches.len() < max_results {
                        matches.push(path.display().to_string());
                    } else {
                        truncated = true;
                        // Keep counting so we know there were more, but bail
                        // once we know we've exceeded the cap — counting all
                        // could be expensive on huge trees.
                        break;
                    }
                }
                Err(_) => {
                    // Skip unreadable entries silently.
                    continue;
                }
            }
        }

        if matches.is_empty() {
            return ToolResult {
                tool_call_id: call_id.into(),
                name: "glob".into(),
                content: "(no matches)".into(),
                is_error: false,
            };
        }

        matches.sort();
        let mut content = matches.join("\n");
        if truncated {
            content.push_str(&format!("\n…[truncated at {} results]", max_results));
            let _ = total_seen; // not surfaced; count would require draining iter
        }

        ToolResult {
            tool_call_id: call_id.into(),
            name: "glob".into(),
            content,
            is_error: false,
        }
    }
}

fn err(call_id: &str, msg: String) -> ToolResult {
    ToolResult {
        tool_call_id: call_id.into(),
        name: "glob".into(),
        content: msg,
        is_error: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Path to a deterministic fixture dir we build per-test. Each test
    /// owns its own subdir under `target/glob-test/<test-name>/` so they
    /// don't race or depend on the surrounding workspace layout.
    fn fixture_dir(name: &str) -> PathBuf {
        let base = std::env::temp_dir()
            .join("merlion-glob-test")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[tokio::test]
    async fn finds_a_file_via_glob_pattern() {
        let dir = fixture_dir("finds");
        std::fs::write(dir.join("hello.toml"), "[package]\nname = \"x\"\n").unwrap();
        let tool = Glob;
        let args = json!({
            "pattern": "*.toml",
            "cwd": dir.to_str().unwrap(),
        });
        let res = tool.call("call-1", args).await;
        assert!(!res.is_error, "tool reported error: {}", res.content);
        assert!(
            res.content.contains("hello.toml"),
            "expected hello.toml in output, got:\n{}",
            res.content
        );
    }

    #[tokio::test]
    async fn zero_matches_returns_no_matches() {
        let dir = fixture_dir("zero");
        let tool = Glob;
        let args = json!({
            "pattern": "this_file_definitely_does_not_exist_xyz_*.nope",
            "cwd": dir.to_str().unwrap(),
        });
        let res = tool.call("call-2", args).await;
        assert!(!res.is_error, "tool reported error: {}", res.content);
        assert_eq!(res.content, "(no matches)");
    }

    #[tokio::test]
    async fn respects_max_results_and_emits_truncation_message() {
        let dir = fixture_dir("max-results");
        // 5 .rs files so a max_results of 2 forces truncation regardless
        // of which file the OS lists first.
        for i in 0..5 {
            std::fs::write(dir.join(format!("f{i}.rs")), "// stub\n").unwrap();
        }
        let tool = Glob;
        let args = json!({
            "pattern": "*.rs",
            "cwd": dir.to_str().unwrap(),
            "max_results": 2
        });
        let res = tool.call("call-3", args).await;
        assert!(!res.is_error, "tool reported error: {}", res.content);
        assert!(
            res.content.contains("…[truncated at 2 results]"),
            "expected truncation marker, got:\n{}",
            res.content
        );
        // Body should have exactly 2 result lines before the truncation line.
        let lines: Vec<&str> = res.content.lines().collect();
        assert_eq!(
            lines.len(),
            3,
            "expected 2 results + 1 truncation line, got: {:?}",
            lines
        );
    }
}
