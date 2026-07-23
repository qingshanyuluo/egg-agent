//! Translation plugin: auto-translate reasoning (chain-of-thought) to Chinese.
//!
//! While reasoning streams in, this plugin accumulates it. On the first
//! content token (signaling reasoning is complete), it spawns an async task
//! that calls the aux LLM to produce a Chinese translation. The result is
//! sent back via [`PluginEvent::TranslationReady`] and stored on the
//! corresponding [`Message`].
//!
//! Toggle on/off via the `/translate` slash command. Enabled by default
//! when aux is configured; silent no-op when aux is absent or disabled.

use std::sync::{Arc, Mutex};

use tokio::sync::mpsc::UnboundedSender;

use crate::agent::AgentEvent;
use crate::app::{App, Message, Role};
use crate::llm::{ChatMessage, LlmClient};
use super::{Plugin, PluginEvent};

pub struct TranslatePlugin {
    /// Message index currently accumulating reasoning, if any.
    active_idx: Mutex<Option<usize>>,
    /// Accumulated reasoning text so far.
    buffer: Mutex<String>,
    /// Toggled by `/translate`. Defaults to `true`.
    enabled: Mutex<bool>,
}

impl TranslatePlugin {
    pub fn new() -> Self {
        Self {
            active_idx: Mutex::new(None),
            buffer: Mutex::new(String::new()),
            enabled: Mutex::new(true),
        }
    }
}

impl Plugin for TranslatePlugin {
    fn name(&self) -> &'static str {
        "translate"
    }

    fn commands(&self) -> Vec<(&'static str, &'static str)> {
        vec![("translate", "Toggle reasoning translation on/off")]
    }

    fn handle_command(&self, name: &str, app: &mut App) -> bool {
        if name != "translate" {
            return false;
        }
        let mut enabled = self.enabled.lock().unwrap();
        *enabled = !*enabled;
        let status = if *enabled { "on" } else { "off" };
        log::info!("translation plugin: {status}");
        app.messages
            .push(Message::new(Role::System, format!("translation: {status}")));
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

        match event {
            AgentEvent::TurnStart => {
                *self.active_idx.lock().unwrap() = None;
                self.buffer.lock().unwrap().clear();
            }
            AgentEvent::ReasoningDelta(delta) => {
                let idx = app.streaming_idx();
                *self.active_idx.lock().unwrap() = Some(idx);
                self.buffer.lock().unwrap().push_str(delta);
            }
            AgentEvent::ContentDelta(_) | AgentEvent::ToolCall { .. } => {
                // Reasoning is complete when content arrives OR the model makes a
                // tool call. Both cases mean the current thinking block is finished.
                let maybe_idx = *self.active_idx.lock().unwrap();
                if let Some(idx) = maybe_idx {
                    let reasoning = self.buffer.lock().unwrap().clone();
                    self.buffer.lock().unwrap().clear();
                    *self.active_idx.lock().unwrap() = None;
                    if !reasoning.is_empty() {
                        log::debug!(
                            "translate: firing for msg_idx={idx} reasoning_len={}",
                            reasoning.len()
                        );
                        let aux = aux.clone();
                        let events = events.clone();
                        tokio::spawn(async move {
                            match translate(&*aux, &reasoning).await {
                                Ok(translated) => {
                                    log::debug!(
                                        "translate: done msg_idx={idx} result_len={}",
                                        translated.len()
                                    );
                                    let _ = events.send(PluginEvent::Custom {
                                        msg_idx: idx,
                                        field: "translation",
                                        text: translated,
                                    });
                                }
                                Err(e) => {
                                    log::warn!("translate: failed msg_idx={idx}: {e:#}");
                                }
                            }
                        });
                    }
                }
            }
            AgentEvent::Done(_) | AgentEvent::Error { .. } => {
                *self.active_idx.lock().unwrap() = None;
                self.buffer.lock().unwrap().clear();
            }
            _ => {}
        }
    }
}

/// Ask the aux model to translate reasoning text to Chinese.
async fn translate(llm: &dyn LlmClient, text: &str) -> anyhow::Result<String> {
    let messages = vec![
        ChatMessage::system(
            "Translate the following reasoning/thinking to Chinese. \
             Keep technical terms (API, function names, file paths) in the \
             original language. Output only the translation, no explanation.",
        ),
        ChatMessage::user(text),
    ];
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let result = llm.chat_stream(&messages, &[], &tx).await?;
    drop(tx);

    let translated = result.content.unwrap_or_default();
    if translated.trim().is_empty() {
        anyhow::bail!("empty translation response");
    }
    Ok(translated.trim().to_string())
}
