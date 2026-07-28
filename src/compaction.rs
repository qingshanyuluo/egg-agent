use crate::llm::ChatMessage;
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Token estimator using the tiktoken approximation (1 token ≈ 4 chars for English,
/// ~2-3 for code). Conservative estimate suitable for Claude/GPT-4 class models.
pub fn estimate_tokens(text: &str) -> usize {
    // Chars / 3.5 gives a reasonable middle ground
    (text.len() as f64 / 3.5).ceil() as usize
}

pub fn estimate_message_tokens(msg: &ChatMessage) -> usize {
    let mut total = 0;
    
    // Content
    if let Some(content) = &msg.content {
        total += estimate_tokens(content);
    }
    
    // Reasoning (if present)
    if let Some(reasoning) = &msg.reasoning_content {
        total += estimate_tokens(reasoning);
    }
    
    // Tool calls
    if let Some(calls) = &msg.tool_calls {
        for call in calls {
            total += estimate_tokens(&call.function.name);
            total += estimate_tokens(&call.function.arguments);
        }
    }
    
    // Tool result (in tool_call_id messages)
    if msg.tool_call_id.is_some() {
        total += 10; // Small overhead for tool result message
    }
    
    total + 4 // Role + message overhead
}

/// Handoff summary that gets updated incrementally on each compaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffSummary {
    /// The core task objective
    pub objective: String,
    /// Key constraints and requirements
    pub constraints: Vec<String>,
    /// Current work state: what's been done, decisions made
    pub work_state: String,
    /// Next immediate move
    pub next_move: String,
    /// Files read or modified with brief context
    pub files: Vec<FileContext>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileContext {
    pub path: String,
    pub context: String, // e.g. "added compaction module", "read session.rs L120-180"
}

impl HandoffSummary {
    /// Render the handoff summary as markdown for injection into the conversation.
    pub fn render(&self) -> String {
        let mut out = String::from("## Session Handoff Summary\n\n");
        
        out.push_str(&format!("**Objective**: {}\n\n", self.objective));
        
        if !self.constraints.is_empty() {
            out.push_str("**Constraints**:\n");
            for c in &self.constraints {
                out.push_str(&format!("- {}\n", c));
            }
            out.push('\n');
        }
        
        out.push_str(&format!("**Work State**:\n{}\n\n", self.work_state));
        out.push_str(&format!("**Next Move**: {}\n\n", self.next_move));
        
        if !self.files.is_empty() {
            out.push_str("**Files**:\n");
            for f in &self.files {
                out.push_str(&format!("- `{}`: {}\n", f.path, f.context));
            }
        }
        
        out
    }
}

/// Configuration for the compaction strategy.
#[derive(Debug, Clone)]
pub struct CompactionConfig {
    /// Trigger compaction when total tokens exceed this threshold
    pub trigger_tokens: usize,
    /// Target tokens after compaction
    pub target_tokens: usize,
    /// Minimum number of recent messages to keep verbatim (anchored from the end)
    pub keep_recent_count: usize,
    /// Minimum token budget for recent messages (overrides count if needed)
    pub keep_recent_tokens: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            // Trigger at 80% of a 200k context (Claude 3.5+, GPT-4 Turbo, etc.)
            trigger_tokens: 160_000,
            // Compact down to 50% to give runway before next compaction
            target_tokens: 100_000,
            // Keep last 20 turns verbatim (10 user-assistant exchanges)
            keep_recent_count: 20,
            // Or last 30k tokens, whichever is larger — strong models handle this fine
            keep_recent_tokens: 30_000,
        }
    }
}

/// Compaction result: the new conversation history.
pub struct CompactionResult {
    pub messages: Vec<ChatMessage>,
    pub summary: HandoffSummary,
}

/// Core compaction engine using the anchored summary + verbatim tail strategy.
pub struct Compactor {
    config: CompactionConfig,
    /// The last handoff summary, updated incrementally on each compaction.
    last_summary: std::sync::RwLock<Option<HandoffSummary>>,
}

impl Compactor {
    pub fn new(config: CompactionConfig) -> Self {
        Self {
            config,
            last_summary: std::sync::RwLock::new(None),
        }
    }

    /// Check if compaction is needed and perform it if so.
    /// Returns the original history if no compaction needed, or the compacted history.
    /// The handoff summary is updated incrementally (anchored) across calls.
    pub fn compact_if_needed(&self, messages: &[ChatMessage]) -> Result<Vec<ChatMessage>> {
        if !self.should_compact(messages) {
            return Ok(messages.to_vec());
        }

        let previous = self.last_summary.read().ok().and_then(|g| g.clone());
        let result = self.compact(messages, previous.as_ref())?;

        // Remember the updated summary for the next compaction (anchoring).
        if let Ok(mut guard) = self.last_summary.write() {
            *guard = Some(result.summary.clone());
        }

        Ok(result.messages)
    }

    /// Check if compaction is needed.
    pub fn should_compact(&self, messages: &[ChatMessage]) -> bool {
        let total: usize = messages.iter().map(|m| estimate_message_tokens(m)).sum();
        total > self.config.trigger_tokens
    }

    /// Perform compaction: generate a handoff summary from the older turns,
    /// then keep recent turns verbatim.
    pub fn compact(
        &self,
        messages: &[ChatMessage],
        previous_summary: Option<&HandoffSummary>,
    ) -> Result<CompactionResult> {
        // 1. Split into compactable (older) and keep (recent)
        let (to_compact, to_keep) = self.split_messages(messages)?;

        // 2. Generate or update the handoff summary
        let summary = self.generate_summary(&to_compact, previous_summary)?;

        // 3. Reconstruct: system + summary message + recent verbatim
        let mut new_messages = Vec::new();

        // Preserve original system message if present
        if let Some(first) = messages.first() {
            if first.role == "system" {
                new_messages.push(first.clone());
            }
        }

        // Inject handoff summary as a user message
        new_messages.push(ChatMessage::user(summary.render()));

        // Append recent messages verbatim
        new_messages.extend_from_slice(to_keep);

        Ok(CompactionResult {
            messages: new_messages,
            summary,
        })
    }

    /// Split messages into (to_compact, to_keep).
    /// Ensures tool_call/tool_result pairs are not severed.
    fn split_messages<'a>(&self, messages: &'a [ChatMessage]) -> Result<(&'a [ChatMessage], &'a [ChatMessage])> {
        if messages.is_empty() {
            return Ok((&[], &[]));
        }

        // Find the split point: count back from the end
        let mut keep_start = messages.len();
        let mut token_budget = 0;
        let mut count = 0;

        for (i, msg) in messages.iter().enumerate().rev() {
            let tokens = estimate_message_tokens(msg);
            token_budget += tokens;
            count += 1;
            keep_start = i;
            
            // Stop if we've satisfied both count and token budget
            if count >= self.config.keep_recent_count && token_budget >= self.config.keep_recent_tokens {
                break;
            }
        }

        // Adjust split point to avoid breaking tool call/result pairs
        keep_start = self.adjust_for_tool_boundaries(messages, keep_start);

        // System message should never be compacted
        let system_offset = if messages.first().map(|m| m.role == "system").unwrap_or(false) {
            1
        } else {
            0
        };

        let split = keep_start.max(system_offset);

        Ok((&messages[system_offset..split], &messages[split..]))
    }

    /// Adjust the split index to ensure tool_call and tool_result stay together.
    fn adjust_for_tool_boundaries(&self, messages: &[ChatMessage], mut split: usize) -> usize {
        // If the message at split-1 is an assistant with tool_calls, include the next tool result
        if split > 0 && split < messages.len() {
            if let Some(prev) = messages.get(split - 1) {
                if prev.role == "assistant" && prev.tool_calls.is_some() {
                    // Next message should be the tool results; include it
                    split += 1;
                }
            }
        }
        split
    }

    /// Generate or update the handoff summary from the compactable messages.
    /// In a real implementation, you'd call the LLM here with a structured prompt.
    /// For now, this is a placeholder that extracts surface signals.
    fn generate_summary(
        &self,
        messages: &[ChatMessage],
        previous: Option<&HandoffSummary>,
    ) -> Result<HandoffSummary> {
        // Placeholder: extract objective from first user message, file ops from tool calls
        let mut objective = previous
            .map(|s| s.objective.clone())
            .unwrap_or_else(|| "Continue the task".to_string());

        let constraints = previous
            .map(|s| s.constraints.clone())
            .unwrap_or_default();

        let mut files = previous
            .map(|s| s.files.clone())
            .unwrap_or_default();

        // Scan messages for file operations
        for msg in messages {
            if let Some(calls) = &msg.tool_calls {
                for call in calls {
                    match call.function.name.as_str() {
                        "read_file" | "edit_file" | "write_file" => {
                            // Extract path from input (simplified)
                            if let Some(path) = self.extract_path_from_tool_input(&call.function.arguments) {
                                if !files.iter().any(|f| f.path == path) {
                                    files.push(FileContext {
                                        path: path.clone(),
                                        context: format!("{} operation", call.function.name),
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }

            // Update objective from first user message if not set
            if msg.role == "user" && objective == "Continue the task" {
                if let Some(content) = &msg.content {
                    let first_line = content.lines().next().unwrap_or("");
                    if !first_line.is_empty() && first_line.len() < 200 {
                        objective = first_line.to_string();
                    }
                }
            }
        }

        // In production: call LLM with a structured prompt to generate work_state and next_move
        let work_state = format!(
            "Processed {} messages, {} file operations tracked.",
            messages.len(),
            files.len()
        );

        let next_move = "Continue from the recent context.".to_string();

        Ok(HandoffSummary {
            objective,
            constraints,
            work_state,
            next_move,
            files,
        })
    }

    /// Naive path extraction from tool input JSON string.
    fn extract_path_from_tool_input(&self, input: &str) -> Option<String> {
        // Try to parse as JSON and extract "path" field
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(input) {
            if let Some(path) = val.get("path").and_then(|v| v.as_str()) {
                return Some(path.to_string());
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_tokens() {
        assert!(estimate_tokens("hello world") > 0);
        assert!(estimate_tokens("fn main() {}") > 0);
    }

    #[test]
    fn test_should_compact() {
        let config = CompactionConfig {
            trigger_tokens: 1000,
            target_tokens: 500,
            keep_recent_count: 5,
            keep_recent_tokens: 200,
        };
        let compactor = Compactor::new(config);

        let messages = vec![
            ChatMessage::user("a".repeat(4000)), // ~1143 tokens
        ];

        assert!(compactor.should_compact(&messages));
    }

    #[test]
    fn test_split_messages() {
        let config = CompactionConfig {
            trigger_tokens: 10000,
            target_tokens: 5000,
            keep_recent_count: 2,
            keep_recent_tokens: 10, // Very low, so count dominates
        };
        let compactor = Compactor::new(config);

        let messages = vec![
            ChatMessage::system("system"),
            ChatMessage::user("a".repeat(100)), // Make msg1 bigger
            ChatMessage::assistant("msg2"),
            ChatMessage::user("msg3"),
        ];

        let (to_compact, to_keep) = compactor.split_messages(&messages).unwrap();
        
        // With keep_recent_count=2, we keep last 2 (msg2, msg3)
        // System is excluded, so to_compact has msg1
        assert!(to_compact.len() >= 1, "Expected at least msg1 in to_compact, got {}", to_compact.len());
        assert!(to_keep.len() >= 2, "Expected at least msg2, msg3 in to_keep, got {}", to_keep.len());
    }

    #[test]
    fn test_handoff_summary_render() {
        let summary = HandoffSummary {
            objective: "Build a compaction module".to_string(),
            constraints: vec!["Preserve tool boundaries".to_string()],
            work_state: "Created compaction.rs with core logic".to_string(),
            next_move: "Wire into session.rs".to_string(),
            files: vec![FileContext {
                path: "src/compaction.rs".to_string(),
                context: "created new module".to_string(),
            }],
        };

        let rendered = summary.render();
        assert!(rendered.contains("## Session Handoff Summary"));
        assert!(rendered.contains("**Objective**: Build a compaction module"));
        assert!(rendered.contains("src/compaction.rs"));
    }
}
