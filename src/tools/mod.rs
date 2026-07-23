//! Tool abstraction and registry.
//!
//! Each tool exposes a JSON Schema (for the LLM) and an async `call`. The
//! registry turns the toolset into `ToolDef`s for the request and dispatches
//! tool calls by name.

pub mod bash;
pub mod edit;
pub mod fs;
pub mod search;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use crate::llm::{FunctionDef, ToolDef};

/// Cap tool output so a huge file or command doesn't blow up the context window.
pub const MAX_OUTPUT_CHARS: usize = 16_000;

pub fn truncate(mut s: String) -> String {
    if s.chars().count() > MAX_OUTPUT_CHARS {
        let kept: String = s.chars().take(MAX_OUTPUT_CHARS).collect();
        s = format!(
            "{kept}\n\n... [output truncated at {MAX_OUTPUT_CHARS} chars] ..."
        );
    }
    s
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    /// JSON Schema for the parameters object.
    fn parameters(&self) -> Value;
    /// Execute the tool. `args` is the parsed arguments object from the model.
    async fn call(&self, args: Value) -> Result<String>;
}

pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    /// The default toolset for a mini SWE agent.
    pub fn default_set() -> Self {
        Self {
            tools: vec![
                Box::new(fs::ReadFile),
                Box::new(fs::WriteFile),
                Box::new(edit::EditFile),
                Box::new(bash::Bash),
                Box::new(search::Search),
            ],
        }
    }

    /// Tool definitions to send to the model.
    pub fn defs(&self) -> Vec<ToolDef> {
        self.tools
            .iter()
            .map(|t| ToolDef {
                kind: "function".to_string(),
                function: FunctionDef {
                    name: t.name().to_string(),
                    description: t.description().to_string(),
                    parameters: t.parameters(),
                },
            })
            .collect()
    }

    /// Run a tool by name with JSON-string arguments (as the model sends them).
    /// Any failure is returned as a string so the model can see and recover.
    pub async fn dispatch(&self, name: &str, arguments: &str) -> String {
        let Some(tool) = self.tools.iter().find(|t| t.name() == name) else {
            return format!("error: unknown tool '{name}'");
        };

        let args: Value = if arguments.trim().is_empty() {
            Value::Object(Default::default())
        } else {
            match serde_json::from_str(arguments) {
                Ok(v) => v,
                Err(e) => return format!("error: invalid JSON arguments: {e}"),
            }
        };

        match tool.call(args).await {
            Ok(out) => truncate(out),
            Err(e) => format!("error: {e:#}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bash_write_read_roundtrip() {
        let reg = ToolRegistry::default_set();

        // bash: list something deterministic
        let out = reg.dispatch("bash", r#"{"command":"echo hello_egg"}"#).await;
        assert!(out.contains("hello_egg"), "bash out: {out}");
        assert!(out.contains("exit code: 0"), "bash out: {out}");

        // write_file then read_file
        let p = std::env::temp_dir().join("egg_agent_test.txt");
        let path = p.to_string_lossy().to_string();
        let args = serde_json::json!({"path": path, "content": "hi from egg"}).to_string();
        let out = reg.dispatch("write_file", &args).await;
        assert!(out.contains("wrote"), "write out: {out}");

        let args = serde_json::json!({"path": path}).to_string();
        let out = reg.dispatch("read_file", &args).await;
        assert!(out.contains("hi from egg"), "read out: {out}");

        // unknown tool -> graceful error string, not a panic
        let out = reg.dispatch("nope", "{}").await;
        assert!(out.starts_with("error: unknown tool"), "unknown: {out}");

        // bad JSON -> graceful error
        let out = reg.dispatch("bash", "{not json").await;
        assert!(out.starts_with("error: invalid JSON"), "badjson: {out}");

        let _ = std::fs::remove_file(&p);
    }
}
