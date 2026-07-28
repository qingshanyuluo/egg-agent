//! Memory plugin: auto-distill experience notes from hard-won trajectories.
//!
//! Three-stage funnel, each stage cheaper than the next:
//!
//! 1. **Heuristic** — a finished turn (`AgentEvent::Done`) with
//!    `>= min_tool_calls` tool calls is a "complex exploration".
//! 2. **Screening** — the cheap aux model judges whether the turn matches
//!    "repeated trial-and-error, figured things out, finally succeeded".
//!    Turns that sailed through smoothly (or were mechanical chores) are
//!    dropped here, before any expensive model is involved.
//! 3. **Summarizing** — a (ideally stronger) model distills the trajectory
//!    into a reusable experience note under `~/.egg-agent/memory/`.
//!
//! Archival is fully asynchronous and never touches the conversation history.
//! Without an `[aux]` config the screening stage is skipped; without a
//! `[memory]` config the main model summarizes its own trajectory.
//!
//! Toggle via `/memory`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc::UnboundedSender;

use super::{Plugin, PluginEvent};
use crate::agent::AgentEvent;
use crate::app::{App, Message, Role};
use crate::llm::{ChatMessage, LlmClient};
use crate::memory;

/// Per-entry truncation keeps the trace compact.
const ARGS_CAP: usize = 240;
const OUTPUT_CAP: usize = 500;
/// Trace budget for the summarizer (bytes); middle steps are elided past it.
const TRACE_CAP: usize = 24_000;
/// Smaller budget for the aux screener — small models stay sharp on short input.
const SCREEN_CAP: usize = 6_000;

pub struct MemoryPlugin {
    /// The summarizer — ideally a stronger model than the main one.
    summarizer: Arc<dyn LlmClient>,
    /// The cheap gatekeeper (aux model). `None` = no screening, archive all
    /// complex turns.
    screener: Option<Arc<dyn LlmClient>>,
    min_tool_calls: u32,
    /// Toggled by `/memory`.
    enabled: Mutex<bool>,
    /// One archival pipeline in flight at most.
    busy: Arc<AtomicBool>,
}

impl MemoryPlugin {
    pub fn new(
        summarizer: Arc<dyn LlmClient>,
        screener: Option<Arc<dyn LlmClient>>,
        min_tool_calls: u32,
    ) -> Self {
        Self {
            summarizer,
            screener,
            min_tool_calls,
            enabled: Mutex::new(true),
            busy: Arc::new(AtomicBool::new(false)),
        }
    }

    fn is_on(&self) -> bool {
        *self.enabled.lock().unwrap()
    }
}

impl Plugin for MemoryPlugin {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn commands(&self) -> Vec<(&'static str, &'static str)> {
        vec![("memory", "Toggle auto experience-memory archival on/off")]
    }

    fn handle_command(&self, name: &str, app: &mut App) -> bool {
        if name != "memory" {
            return false;
        }
        let mut enabled = self.enabled.lock().unwrap();
        *enabled = !*enabled;
        let status = if *enabled { "on" } else { "off" };
        log::info!("memory plugin: {status}");
        app.messages
            .push(Message::new(Role::System, format!("memory archival: {status}")));
        true
    }

    fn is_enabled(&self) -> bool {
        self.is_on()
    }

    fn on_agent_event(
        &self,
        event: &AgentEvent,
        app: &mut App,
        _aux: Option<&Arc<dyn LlmClient>>,
        events: &UnboundedSender<PluginEvent>,
    ) {
        let AgentEvent::Done(history) = event else {
            return;
        };
        if !self.is_on() {
            return;
        }

        // Stage 1: cheap heuristic — enough tool calls to count as complex.
        let digest = digest_turn(history);
        if digest.tool_calls < self.min_tool_calls {
            return;
        }
        if self.busy.swap(true, Ordering::SeqCst) {
            log::info!("memory: archival already in flight, skipping this turn");
            return;
        }

        let msg_idx = app.messages.len();
        let status = if self.screener.is_some() {
            format!(
                "🧠 检测到复杂探索（{} 次工具调用 / {} 次失败）—— 小模型筛选中…",
                digest.tool_calls, digest.failures
            )
        } else {
            format!(
                "🧠 检测到复杂探索（{} 次工具调用 / {} 次失败）—— 正在复盘归档经验…",
                digest.tool_calls, digest.failures
            )
        };
        app.messages.push(Message::new(Role::System, status));
        log::info!(
            "memory: complex turn detected ({} calls / {} failures)",
            digest.tool_calls,
            digest.failures
        );

        let summarizer = self.summarizer.clone();
        let screener = self.screener.clone();
        let busy = self.busy.clone();
        let events = events.clone();
        tokio::spawn(async move {
            let text = run_pipeline(summarizer, screener, &digest).await;
            busy.store(false, Ordering::SeqCst);
            let _ = events.send(PluginEvent::Custom {
                msg_idx,
                field: "memory",
                text,
            });
        });
    }
}

/// The async archival pipeline: screening (optional) → summarize → save.
/// Returns the user-facing result line.
async fn run_pipeline(
    summarizer: Arc<dyn LlmClient>,
    screener: Option<Arc<dyn LlmClient>>,
    digest: &TurnDigest,
) -> String {
    // Stage 2: aux-model screening. Screening *errors* fail open — a network
    // hiccup shouldn't silently drop a potentially valuable experience.
    if let Some(screener) = screener {
        match screen(&*screener, digest).await {
            Ok(verdict) if !verdict.worthy => {
                log::info!("memory: screened out — {}", verdict.reason);
                return format!("⏭ 小模型判定不值得归档：{}", verdict.reason);
            }
            Ok(_) => log::info!("memory: screening passed"),
            Err(e) => log::warn!("memory: screening failed ({e:#}), archiving anyway"),
        }
    }

    // Stage 3: distill + persist.
    match summarize(&*summarizer, digest).await {
        Ok(body) => match memory::save(&body, digest.tool_calls, digest.failures) {
            Ok(path) => format!(
                "✅ 经验已归档 → {}\n📌 {}",
                path.display(),
                memory::title_of(&body)
            ),
            Err(e) => format!("⚠️ 总结完成但写入失败：{e:#}"),
        },
        Err(e) => format!("⚠️ 经验归档失败：{e:#}"),
    }
}

/// A compact, summarizer-ready view of the last user turn.
struct TurnDigest {
    task: String,
    entries: Vec<String>,
    answer: String,
    tool_calls: u32,
    failures: u32,
}

impl TurnDigest {
    /// Render the trace within a byte budget, eliding middle steps when over.
    fn trace(&self, cap: usize) -> String {
        elide_middle(&self.entries, cap)
    }
}

/// Scan the tail of the history (everything after the last user message) and
/// condense it into a trace of tool calls and their outcomes.
fn digest_turn(history: &[ChatMessage]) -> TurnDigest {
    let start = history.iter().rposition(|m| m.role == "user").unwrap_or(0);
    let task = history[start].content.clone().unwrap_or_default();

    let mut entries: Vec<String> = Vec::new();
    let mut tool_calls = 0u32;
    let mut failures = 0u32;
    let mut answer = String::new();

    for m in &history[start + 1..] {
        match m.role.as_str() {
            "assistant" => {
                if let Some(calls) = &m.tool_calls {
                    for c in calls {
                        tool_calls += 1;
                        entries.push(format!(
                            "### {tool_calls}. {}\n参数: {}",
                            c.function.name,
                            truncate(&c.function.arguments, ARGS_CAP)
                        ));
                    }
                }
                // The last non-empty assistant content is the final answer.
                if let Some(c) = &m.content
                    && !c.trim().is_empty()
                {
                    answer = c.clone();
                }
            }
            "tool" => {
                let out = m.content.as_deref().unwrap_or("");
                let failed = is_failure(out);
                if failed {
                    failures += 1;
                }
                let tag = if failed { "FAIL" } else { "ok" };
                entries.push(format!("→ [{tag}] {}", truncate(out, OUTPUT_CAP)));
            }
            _ => {}
        }
    }

    TurnDigest {
        task,
        entries,
        answer,
        tool_calls,
        failures,
    }
}

/// A tool result counts as a failure when the tool itself reported an error
/// (`error: …` from dispatch) or the shell command exited non-zero.
fn is_failure(output: &str) -> bool {
    let first = output.trim_start().lines().next().unwrap_or("");
    if first.starts_with("error:") {
        return true;
    }
    if let Some(code) = first.strip_prefix("exit code: ") {
        return code.trim() != "0";
    }
    false
}

/// Truncate `s` to at most `cap` bytes on a char boundary.
fn truncate(s: &str, cap: usize) -> &str {
    if s.len() <= cap {
        return s;
    }
    let mut end = cap;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Join trace entries; if the total exceeds the budget, keep the head and
/// tail and elide the middle (recent steps matter most, but the first steps
/// often show the original approach).
fn elide_middle(entries: &[String], cap: usize) -> String {
    let total: usize = entries.iter().map(|e| e.len() + 1).sum();
    if total <= cap {
        return entries.join("\n");
    }
    let head_budget = cap / 4;
    let tail_budget = cap - head_budget;

    let mut head: Vec<&String> = Vec::new();
    let mut used = 0;
    for e in entries {
        if used + e.len() > head_budget {
            break;
        }
        used += e.len() + 1;
        head.push(e);
    }

    let mut tail: Vec<&String> = Vec::new();
    let mut used = 0;
    for e in entries.iter().rev() {
        if used + e.len() > tail_budget {
            break;
        }
        used += e.len() + 1;
        tail.push(e);
    }
    tail.reverse();

    let skipped = entries.len() - head.len() - tail.len();
    let mut out = head.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n");
    out.push_str(&format!("\n…（中间 {skipped} 步略）…\n"));
    out.push_str(&tail.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n"));
    out
}

// ---- Stage 2: aux-model screening ----

struct Verdict {
    worthy: bool,
    reason: String,
}

const SCREEN_PROMPT: &str = "你是 AI 编程助手的经验筛选器。给你一轮对话的轨迹摘要：agent 接到任务后执行了若干次工具调用，最终给出了答复。\n\n\
判断这轮经历是否值得归档为「经验记忆」。唯一标准：agent 是否经历了有意义的探索或试错才成功——\
例如：多次构建/测试失败后才修复、反复尝试才摸清环境或 API 用法、版本兼容或配置问题排查、思路走偏后自我纠正。\
这类经历对未来有参考价值，答 YES。\n\n\
以下情况答 NO：\n\
- 一帆风顺：直接就做对，没有实质试错或摸索\n\
- 纯机械执行：批量重命名、格式化、例行查询等例行操作\n\
- 只是读取/检索信息，没有解决实际问题\n\
- 最终没有真正完成任务\n\
- 闲聊或纯知识问答\n\n\
输出格式（严格两行）：\n\
第一行：YES 或 NO\n\
第二行：一句话理由";

/// Ask the aux model whether this trajectory is worth archiving.
async fn screen(llm: &dyn LlmClient, digest: &TurnDigest) -> anyhow::Result<Verdict> {
    let user = format!(
        "## 原始任务\n{}\n\n## 执行轨迹（{} 次工具调用，{} 次失败）\n{}\n\n## 最终答复\n{}",
        digest.task,
        digest.tool_calls,
        digest.failures,
        digest.trace(SCREEN_CAP),
        truncate(&digest.answer, 800)
    );
    let messages = vec![
        ChatMessage::system(SCREEN_PROMPT),
        ChatMessage::user(user),
    ];
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let msg = llm.chat_stream(&messages, &[], &tx).await?;
    drop(tx);

    Ok(parse_verdict(&msg.content.unwrap_or_default()))
}

/// Parse the screener's two-line verdict. Unparseable output fails open
/// (treated as worthy) — a noisy small model shouldn't drop experiences.
fn parse_verdict(text: &str) -> Verdict {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let Some(first) = lines.next() else {
        return Verdict {
            worthy: true,
            reason: "（筛选器无输出，默认归档）".to_string(),
        };
    };
    let verdict_word = first.trim().to_ascii_uppercase();
    let reason = lines.next().unwrap_or("").trim().to_string();
    if verdict_word.starts_with("NO") {
        Verdict {
            worthy: false,
            reason: if reason.is_empty() {
                "未给出理由".to_string()
            } else {
                reason
            },
        }
    } else {
        // YES or anything unexpected → archive.
        Verdict {
            worthy: true,
            reason,
        }
    }
}

// ---- Stage 3: summarizer ----

const SUMMARIZE_PROMPT: &str = "你是一名资深软件工程复盘专家。给你一段 AI 编程助手解决问题的真实轨迹：\
它经历了多次失败尝试，最终找到了可行路径。请把这段经历提炼成一份「经验记忆」，\
供未来的 AI 助手（或人类）遇到类似问题时直接参考，避免重复踩坑。\n\n\
严格要求：\n\
1. 输出中文 Markdown，第一行必须是 `# <一句话标题>`，标题即经验主题（如「cargo 依赖冲突时的排查顺序」）。\n\
   第二行必须是 `scope: global` 或 `scope: project`——判断标准：这条经验离开当前项目后是否仍然通用。\
   通用的工程方法、工具用法、排错思路 → global；只与这个项目的代码、配置、环境相关 → project。拿不准就 project。\n\
   第三行必须是 `category: <主题分类>`，1-2 层小写短横线路径（如 `rust/cargo`、`git`、`shell/quoting`），\
   描述这条经验属于哪个主题领域；**优先从下方「已有分类」中复用**，没有完全合适的才新建。分类按主题划分，不要出现日期。\n\
2. 结构固定为以下小节：\n\
   ## 任务背景 —— 要做什么；项目/环境关键信息（语言、框架、版本，只写轨迹中能确认的，不确定就不写）。\n\
   ## 走过的弯路 —— 每条一行：`失败的做法 → 失败原因`，保留关键报错关键词、版本号、路径。这是最有价值的部分。\n\
   ## 最终可行的路径 —— 按顺序的关键步骤，具体到命令、文件、参数。\n\
   ## 可复用的经验 —— 2-5 条抽象规则，回答「下次遇到同类问题应该直接怎么做、避免什么」。\n\
   ## 标签 —— 一行逗号分隔的小写标签（如 rust, cargo, serde），3-6 个。\n\
3. 聚焦「下次如何少走弯路」，不要逐条复述轨迹，省略与教训无关的细节。\n\
4. 所有事实必须来自轨迹本身，禁止编造轨迹中不存在的命令、报错或版本号。";

/// Ask the summarizer model to distill the trajectory into a memory note.
async fn summarize(llm: &dyn LlmClient, digest: &TurnDigest) -> anyhow::Result<String> {
    // Telling the model which project the trajectory came from lets it make
    // an informed global-vs-project scope call; showing the categories that
    // already exist keeps the topic tree converging on shared branches.
    let project = memory::project_name().unwrap_or_else(|| "（未知）".to_string());
    let fmt_cats = |cats: Vec<String>| {
        if cats.is_empty() {
            "（空）".to_string()
        } else {
            cats.join(", ")
        }
    };
    let existing = format!(
        "project: {}\nglobal: {}",
        fmt_cats(memory::list_categories(&memory::project_scope())),
        fmt_cats(memory::list_categories("global")),
    );
    let user = format!(
        "## 当前项目\n{project}\n\n## 已有分类（优先复用，没有完全合适的才新建）\n{existing}\n\n## 原始任务\n{}\n\n## 执行轨迹（{} 次工具调用，{} 次失败）\n{}\n\n## 最终答复\n{}",
        digest.task,
        digest.tool_calls,
        digest.failures,
        digest.trace(TRACE_CAP),
        digest.answer
    );
    let messages = vec![
        ChatMessage::system(SUMMARIZE_PROMPT),
        ChatMessage::user(user),
    ];
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let msg = llm.chat_stream(&messages, &[], &tx).await?;
    drop(tx);

    let body = msg.content.unwrap_or_default();
    if body.trim().is_empty() {
        anyhow::bail!("summarizer returned an empty note");
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{FunctionCall, ToolCall};

    fn tool_call(name: &str, args: &str) -> Option<Vec<ToolCall>> {
        Some(vec![ToolCall {
            id: format!("call_{name}"),
            kind: "function".to_string(),
            function: FunctionCall {
                name: name.to_string(),
                arguments: args.to_string(),
            },
        }])
    }

    fn assistant_with_calls(calls: Option<Vec<ToolCall>>) -> ChatMessage {
        ChatMessage {
            role: "assistant".to_string(),
            content: None,
            reasoning_content: None,
            tool_calls: calls,
            tool_call_id: None,
        }
    }

    #[test]
    fn failure_detection() {
        assert!(is_failure("error: file not found: foo.rs"));
        assert!(is_failure("exit code: 1\n--- stdout ---\nboom"));
        assert!(is_failure("exit code: 101"));
        assert!(is_failure("exit code: signal"));
        assert!(!is_failure("exit code: 0\n--- stdout ---\nok"));
        assert!(!is_failure("wrote 12 bytes to 'x'"));
    }

    #[test]
    fn digest_counts_calls_and_failures_in_last_turn_only() {
        let history = vec![
            ChatMessage::system("sys"),
            // Previous turn: should not be counted.
            ChatMessage::user("old task"),
            assistant_with_calls(tool_call("bash", r#"{"command":"ls"}"#)),
            ChatMessage::tool_result("call_bash", "error: old failure"),
            ChatMessage::text("assistant", "old answer"),
            // Current turn.
            ChatMessage::user("fix the build"),
            assistant_with_calls(tool_call("bash", r#"{"command":"cargo build"}"#)),
            ChatMessage::tool_result("call_bash", "exit code: 101\n--- stderr ---\nerror[E0308]"),
            assistant_with_calls(tool_call("edit", r#"{"path":"src/main.rs"}"#)),
            ChatMessage::tool_result("call_edit", "error: old_string not found"),
            assistant_with_calls(tool_call("bash", r#"{"command":"cargo build"}"#)),
            ChatMessage::tool_result("call_bash", "exit code: 0"),
            ChatMessage::text("assistant", "修好了，问题是类型不匹配。"),
        ];

        let d = digest_turn(&history);
        assert_eq!(d.task, "fix the build");
        assert_eq!(d.tool_calls, 3);
        assert_eq!(d.failures, 2);
        assert_eq!(d.answer, "修好了，问题是类型不匹配。");
        let trace = d.trace(TRACE_CAP);
        assert!(trace.contains("cargo build"));
        assert!(trace.contains("[FAIL]"));
        assert!(!trace.contains("old failure"), "previous turn must be excluded");
    }

    #[test]
    fn elide_middle_keeps_head_and_tail() {
        let small: Vec<String> = (0..10).map(|i| format!("step {i}")).collect();
        assert_eq!(elide_middle(&small, TRACE_CAP), small.join("\n"));

        let big: Vec<String> = (0..2000).map(|i| format!("step {i} {}", "x".repeat(50))).collect();
        let out = elide_middle(&big, TRACE_CAP);
        assert!(out.len() <= TRACE_CAP + 100);
        assert!(out.contains("step 0 "), "keeps the head");
        assert!(out.contains("step 1999 "), "keeps the tail");
        assert!(out.contains("步略"), "marks the elision");
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let s = "中文字符串测试";
        let t = truncate(s, 4);
        assert_eq!(t, "中");
    }

    #[test]
    fn verdict_parsing() {
        let v = parse_verdict("YES\n多次构建失败后才修复，有参考价值");
        assert!(v.worthy);
        assert_eq!(v.reason, "多次构建失败后才修复，有参考价值");

        let v = parse_verdict("NO\n一帆风顺，直接就做对了");
        assert!(!v.worthy);
        assert_eq!(v.reason, "一帆风顺，直接就做对了");

        // Case-insensitive, tolerant of surrounding whitespace.
        assert!(parse_verdict("  yes\n理由").worthy);
        assert!(!parse_verdict("No").worthy);

        // Garbage / empty output fails open.
        assert!(parse_verdict("").worthy);
        assert!(parse_verdict("我觉得……").worthy);
    }
}
