//! The agentic loop: alternate between LLM turns and tool execution until the
//! model produces a final text answer. Each turn is streamed, so reasoning and
//! content deltas reach the UI as they arrive.
//!
//! There is no artificial iteration cap — the loop runs until the model stops
//! calling tools and produces a natural-language reply, or until a stream-level
//! error (timeout / stall / network) occurs. The only guardrails are the
//! per-chunk timeout (5s) and per-request timeout (120s) in the HTTP client.

use std::sync::Arc;

use tokio::sync::mpsc::{self, UnboundedSender};

use crate::compaction::{Compactor, CompactionConfig};
use crate::llm::{ChatMessage, LlmClient, StreamEvent};
use crate::tools::ToolRegistry;

const SYSTEM_PROMPT: &str = "You are egg-agent, a minimal software-engineering assistant running in a \
terminal. You can read and write files and run shell commands via the provided tools. \
Work step by step: call tools to inspect the project and make changes, then, once the task \
is done, reply in natural language summarizing what you did. Prefer using tools over guessing. \
Keep tool commands focused and safe.";

/// Events streamed from the background agent task back to the UI.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// A new assistant turn started; the UI should show a waiting spinner until
    /// the first token arrives.
    TurnStart,
    /// A chunk of chain-of-thought text.
    ReasoningDelta(String),
    /// A chunk of visible answer text.
    ContentDelta(String),
    /// The model requested a tool call.
    ToolCall { name: String, args: String },
    /// A tool finished; `output` is what will be fed back to the model.
    ToolResult { output: String },
    /// Something went wrong. `partial_history` carries the accumulated LLM
    /// conversation (including any completed tool rounds) when the error
    /// happened — `None` means "stream-level error with no progress".
    Error {
        message: String,
        partial_history: Option<Vec<ChatMessage>>,
    },
    /// The turn finished (success). Carries the updated history for the next turn.
    Done(Vec<ChatMessage>),
}

/// Run the agent loop: stream LLM turns, execute tools, repeat until the model
/// produces a final answer or a stream error occurs. No artificial iteration limit.
pub async fn run_agent(
    mut history: Vec<ChatMessage>,
    llm: Arc<dyn LlmClient>,
    tools: Arc<ToolRegistry>,
    tx: UnboundedSender<AgentEvent>,
) {
    if history.first().map(|m| m.role.as_str()) != Some("system") {
        history.insert(0, ChatMessage::system(SYSTEM_PROMPT));
    }

    // Initialize compactor with config tuned for Opus 4.8
    let compactor = Compactor::new(CompactionConfig {
        trigger_tokens: 180_000,  // Trigger at ~90% of 200k
        target_tokens: 120_000,   // Compact down to 60%
        keep_recent_count: 8,     // Keep last 8 turns verbatim
        keep_recent_tokens: 30_000, // Or 30k tokens, whichever is more
    });

    let defs = tools.defs();

    loop {
        let _ = tx.send(AgentEvent::TurnStart);

        let msg = match stream_one_turn(&*llm, &history, &defs, &tx).await {
            Ok(m) => m,
            Err(e) => {
                let _ = tx.send(AgentEvent::Error {
                    message: format!("{e:#}"),
                    partial_history: None,
                });
                return;
            }
        };

        history.push(msg.clone());

        // Compact if needed
        if let Ok(new_history) = compactor.compact_if_needed(&history) {
            history = new_history;
        }

        match msg.tool_calls {
            Some(calls) if !calls.is_empty() => {
                for call in calls {
                    let _ = tx.send(AgentEvent::ToolCall {
                        name: call.function.name.clone(),
                        args: call.function.arguments.clone(),
                    });

                    let output = tools
                        .dispatch(&call.function.name, &call.function.arguments)
                        .await;

                    let _ = tx.send(AgentEvent::ToolResult {
                        output: output.clone(),
                    });

                    history.push(ChatMessage::tool_result(call.id, output));
                }
                // Continue loop — model can call more tools.
            }
            _ => {
                // No tool calls → final answer.
                let _ = tx.send(AgentEvent::Done(history));
                return;
            }
        }
    }
}

/// Drive a single streamed LLM call, forwarding reasoning/content deltas to the
/// UI as they arrive, and return the fully-assembled message.
async fn stream_one_turn(
    llm: &dyn LlmClient,
    history: &[ChatMessage],
    defs: &[crate::llm::ToolDef],
    tx: &UnboundedSender<AgentEvent>,
) -> anyhow::Result<ChatMessage> {
    let (stx, mut srx) = mpsc::unbounded_channel::<StreamEvent>();

    let tx_fwd = tx.clone();
    let forwarder = tokio::spawn(async move {
        while let Some(ev) = srx.recv().await {
            let agent_ev = match ev {
                StreamEvent::Reasoning(s) => AgentEvent::ReasoningDelta(s),
                StreamEvent::Content(s) => AgentEvent::ContentDelta(s),
            };
            let _ = tx_fwd.send(agent_ev);
        }
    });

    let result = llm.chat_stream(history, defs, &stx).await;
    drop(stx);
    let _ = forwarder.await;
    result
}
