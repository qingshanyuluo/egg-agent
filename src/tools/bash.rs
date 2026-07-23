//! bash tool: run a shell command with a timeout.

use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::process::Command;

use super::Tool;

const TIMEOUT: Duration = Duration::from_secs(30);

pub struct Bash;

#[async_trait]
impl Tool for Bash {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn description(&self) -> &'static str {
        "Run a shell command via `sh -c` and return combined stdout+stderr. \
         Has a 30-second timeout. Use for listing files, running builds, tests, git, etc."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute."
                }
            },
            "required": ["command"]
        })
    }

    async fn call(&self, args: Value) -> Result<String> {
        let command = args
            .get("command")
            .and_then(Value::as_str)
            .context("missing required argument 'command'")?;

        let fut = Command::new("sh").arg("-c").arg(command).output();

        let output = match tokio::time::timeout(TIMEOUT, fut).await {
            Ok(res) => res.context("failed to spawn command")?,
            Err(_) => return Ok(format!("error: command timed out after {TIMEOUT:?}")),
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".to_string());

        let mut result = format!("exit code: {code}\n");
        if !stdout.is_empty() {
            result.push_str("--- stdout ---\n");
            result.push_str(&stdout);
        }
        if !stderr.is_empty() {
            result.push_str("--- stderr ---\n");
            result.push_str(&stderr);
        }
        Ok(result)
    }
}
