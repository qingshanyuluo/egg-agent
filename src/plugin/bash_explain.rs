//! Bash explanation plugin: explains shell commands in plain Chinese.
//!
//! When the model calls the `bash` tool, this plugin (if enabled) sends the
//! command to the aux model for a human-readable explanation. The result is
//! returned via [`PluginEvent::BashExplanation`] and rendered below the
//! tool call line.
//!
//! Toggle on/off via the `/explain` slash command. Disabled by default.

use std::sync::{Arc, Mutex};

use tokio::sync::mpsc::UnboundedSender;

use crate::agent::AgentEvent;
use crate::app::{App, Message, Role};
use crate::llm::{ChatMessage, LlmClient};
use super::{Plugin, PluginEvent};

pub struct BashExplainPlugin {
    /// Toggled by `/explain`. Defaults to `false`.
    enabled: Mutex<bool>,
}

impl BashExplainPlugin {
    pub fn new() -> Self {
        Self {
            enabled: Mutex::new(false),
        }
    }
}

impl Plugin for BashExplainPlugin {
    fn name(&self) -> &'static str {
        "bash-explain"
    }

    fn commands(&self) -> Vec<(&'static str, &'static str)> {
        vec![("explain", "Toggle bash command explanation on/off")]
    }

    fn handle_command(&self, name: &str, app: &mut App) -> bool {
        if name != "explain" {
            return false;
        }
        let mut enabled = self.enabled.lock().unwrap();
        *enabled = !*enabled;
        let status = if *enabled { "on" } else { "off" };
        log::info!("bash explain plugin: {status}");
        app.messages
            .push(Message::new(Role::System, format!("bash explanation: {status}")));
        true
    }

    fn is_enabled(&self) -> bool {
        *self.enabled.lock().unwrap()
    }

    fn on_agent_event(
        &self,
        event: &AgentEvent,
        app: &mut App,
        aux: Option<&Arc<dyn LlmClient>>,
        events: &UnboundedSender<PluginEvent>,
    ) {
        if !self.is_enabled() {
            return;
        }
        let aux = match aux {
            Some(a) => a,
            None => return,
        };

        if let AgentEvent::ToolCall { name, args } = event {
            if name != "bash" {
                return;
            }
            let command = extract_command(args);
            if command.is_empty() {
                return;
            }

            let msg_idx = app.messages.len().saturating_sub(1);
            log::debug!(
                "bash explain: firing for msg_idx={msg_idx} command='{command}'"
            );
            let aux = aux.clone();
            let events = events.clone();
            let cmd = command.clone();
            tokio::spawn(async move {
                match explain(&*aux, &cmd).await {
                    Ok(explanation) => {
                        log::debug!(
                            "bash explain: done msg_idx={msg_idx} result_len={}",
                            explanation.len()
                        );
                        let _ = events.send(PluginEvent::Custom {
                            msg_idx,
                            field: "explanation",
                            text: explanation,
                        });
                    }
                    Err(e) => {
                        log::warn!("bash explain: failed msg_idx={msg_idx}: {e:#}");
                    }
                }
            });
        }
    }
}

/// Pull the `command` field out of a JSON tool-call arguments string.
fn extract_command(args: &str) -> String {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(args) else {
        return String::new();
    };
    val.get("command")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// Ask the aux model to explain what a shell command does.
async fn explain(llm: &dyn LlmClient, command: &str) -> anyhow::Result<String> {
    let messages = vec![
        ChatMessage::system(
            "You are a shell command explainer. Given a shell command, \
             explain in one short Chinese sentence what it does and whether \
             it has side effects (creates/deletes/modifies files). \
             Be concise — one line only. Example: 'ls -la' → '列出当前目录所有文件（含隐藏文件）的详细信息，无副作用'",
        ),
        ChatMessage::user(command),
    ];
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let result = llm.chat_stream(&messages, &[], &tx).await?;
    drop(tx);

    let explanation = result.content.unwrap_or_default();
    if explanation.trim().is_empty() {
        anyhow::bail!("empty explanation response");
    }
    Ok(explanation.trim().to_string())
}
