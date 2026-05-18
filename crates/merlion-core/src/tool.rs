use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::message::ToolResult;

/// JSON-schema-style description of a tool. Mirrors the `function` entry of
/// the OpenAI chat-completions `tools` array.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    /// JSON schema describing the tool's input. Use the `serde_json::json!`
    /// macro to construct.
    pub parameters: serde_json::Value,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn schema(&self) -> ToolSchema;

    /// Invoke the tool. `args` is the parsed JSON arguments. The returned
    /// string becomes the `content` of the tool message sent back to the
    /// model. Implementations should map their own errors into a non-error
    /// [`ToolResult`] with `is_error = true` rather than bubbling, so the
    /// agent loop can keep going.
    async fn call(&self, call_id: &str, args: serde_json::Value) -> ToolResult;
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<T: Tool + 'static>(&mut self, tool: T) {
        let name = tool.schema().name;
        self.tools.insert(name, Arc::new(tool));
    }

    pub fn register_arc(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.schema().name;
        self.tools.insert(name, tool);
    }

    pub fn get(&self, name: &str) -> Result<Arc<dyn Tool>> {
        self.tools
            .get(name)
            .cloned()
            .ok_or_else(|| Error::ToolNotFound { name: name.into() })
    }

    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.tools.values().map(|t| t.schema()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }
}
