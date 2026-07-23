//! Filesystem tools: read_file and write_file.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{Value, json};

use super::Tool;

pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &'static str {
        "read_file"
    }

    fn description(&self) -> &'static str {
        "Read a text file and return its contents with line numbers. \
         Line numbers help the edit_file tool locate exact positions."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file, relative to the current working directory or absolute."
                }
            },
            "required": ["path"]
        })
    }

    async fn call(&self, args: Value) -> Result<String> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .context("missing required argument 'path'")?;
        let content = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("could not read file '{path}'"))?;

        // Add line numbers (Claude Code / Kimi Code style) so the model can
        // reference exact lines in edit_file's old_string.
        let numbered: String = content
            .lines()
            .enumerate()
            .map(|(i, line)| format!("{:>6}\t{}", i + 1, line))
            .collect::<Vec<_>>()
            .join("\n");

        Ok(numbered)
    }
}

pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &'static str {
        "write_file"
    }

    fn description(&self) -> &'static str {
        "Write (overwrite) a text file at the given path with the given content. Creates parent directories if needed."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to write."
                },
                "content": {
                    "type": "string",
                    "description": "The full content to write to the file."
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn call(&self, args: Value) -> Result<String> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .context("missing required argument 'path'")?;
        let content = args
            .get("content")
            .and_then(Value::as_str)
            .context("missing required argument 'content'")?;

        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .with_context(|| format!("could not create parent dirs for '{path}'"))?;
            }
        }

        tokio::fs::write(path, content)
            .await
            .with_context(|| format!("could not write file '{path}'"))?;

        Ok(format!("wrote {} bytes to '{path}'", content.len()))
    }
}
