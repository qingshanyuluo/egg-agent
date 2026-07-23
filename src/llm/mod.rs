//! LLM types and client trait.
//!
//! The wire format mirrors the OpenAI Chat Completions API, which DeepSeek
//! (and Kimi / GLM / Qwen in compatibility mode) all speak.

pub mod openai;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

/// A single message in the conversation, matching the OpenAI schema.
///
/// `content` is optional because an assistant turn that only calls tools may
/// have `content: null`. `tool_calls` is present on assistant turns that call
/// tools; `tool_call_id` is present on `role = "tool"` result messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Chain-of-thought. Received from streaming providers and preserved on the
    /// assembled message; never sent back (providers reject or ignore it on
    /// input), so it is skipped on serialize. Reserved for history/inspection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[allow(dead_code)]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self::text("system", content)
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::text("user", content)
    }

    pub fn text(role: &str, content: impl Into<String>) -> Self {
        Self {
            role: role.to_string(),
            content: Some(content.into()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    /// A `role = "tool"` message carrying the result of one tool call.
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".to_string(),
            content: Some(content.into()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "default_function_type")]
    pub kind: String,
    pub function: FunctionCall,
}

fn default_function_type() -> String {
    "function".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// Arguments are a JSON-encoded string, per the OpenAI spec.
    pub arguments: String,
}

/// A tool definition sent to the model so it knows what it can call.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionDef,
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionDef {
    pub name: String,
    pub description: String,
    /// JSON Schema describing the parameters object.
    pub parameters: Value,
}

/// Incremental pieces of a streamed assistant turn.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// A chunk of chain-of-thought (`delta.reasoning_content`).
    Reasoning(String),
    /// A chunk of the visible answer (`delta.content`).
    Content(String),
}

/// Abstraction over an LLM backend so we can add a Claude Messages provider later.
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Stream one assistant turn. Incremental reasoning/content deltas are sent
    /// on `events` as they arrive; the fully-accumulated message (including any
    /// tool calls) is returned so the caller can append it to history verbatim.
    async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDef],
        events: &UnboundedSender<StreamEvent>,
    ) -> Result<ChatMessage>;
}
