//! The agentic loop: alternate between LLM turns and tool execution until the
//! model produces a final text answer. Each turn is streamed, so reasoning and
//! content deltas reach the UI as they arrive.

use std::sync::Arc;

use tokio::sync::mpsc::{self, UnboundedSender};

use crate::llm::{ChatMessage, LlmClient, StreamEvent};
use crate::tools::ToolRegistry;

/// Safety cap: stop after this many LLM round-trips so a misbehaving model
/// can't loop on tools forever.
const MAX_ITERS: usize = 12;

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
    /// Something went wrong; the turn is over.
    Error(String),
    /// The turn finished (success). Carries the updated history for the next turn.
    Done(Vec<ChatMessage>),
}

/// Run one full agent turn (possibly several LLM round-trips if tools are used).
pub async fn run_agent(
    mut history: Vec<ChatMessage>,
    llm: Arc<dyn LlmClient>,
    tools: Arc<ToolRegistry>,
    tx: UnboundedSender<AgentEvent>,
) {
    // Ensure a system prompt is at the front exactly once.
    if history.first().map(|m| m.role.as_str()) != Some("system") {
        history.insert(0, ChatMessage::system(SYSTEM_PROMPT));
    }

    let defs = tools.defs();

    for _ in 0..MAX_ITERS {
        let _ = tx.send(AgentEvent::TurnStart);

        let msg = match stream_one_turn(&*llm, &history, &defs, &tx).await {
            Ok(m) => m,
            Err(e) => {
                let _ = tx.send(AgentEvent::Error(format!("{e:#}")));
                return;
            }
        };

        // Record the assistant turn (may contain tool_calls and/or content).
        history.push(msg.clone());

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
                // Loop again with the tool results appended.
            }
            _ => {
                // No tool calls -> final answer already streamed via ContentDelta.
                let _ = tx.send(AgentEvent::Done(history));
                return;
            }
        }
    }

    let _ = tx.send(AgentEvent::Error(format!(
        "reached max iterations ({MAX_ITERS}) without a final answer"
    )));
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

    // Forward stream deltas to the UI concurrently with the HTTP stream.
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
    drop(stx); // close the channel so the forwarder finishes
    let _ = forwarder.await;
    result
}
