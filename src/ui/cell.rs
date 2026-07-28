//! The [`Cell`] trait and one render struct per message [`Role`].
//!
//! ## Why this exists
//!
//! `draw_transcript` used to be a single ~350-line `for message in messages`
//! body with a giant `match message.role` inside it, interleaved with wrap
//! math, three kinds of hitbox bookkeeping, and selection highlighting. To
//! debug one message type you had to read past all the others.
//!
//! Now each role renders *itself*: a [`Cell`] turns a `&Message` into a
//! `Vec<CellLine>` — a styled [`Line`] plus an optional [`Hit`] tag marking it
//! as a clickable toggle (a "thought" or "tool/output" header). The driver in
//! [`super::transcript`] concatenates every cell's lines into the one flat list
//! the scroll / selection / render loop still operates on, and rebuilds the
//! `thought_rows` / `tool_rows` index from the [`Hit`] tags — so the downstream
//! math is byte-for-byte identical to the old monolith.
//!
//! ## Contract preserved
//!
//! Cells borrow the `&Message` and its index; nothing is copied into new owned
//! structs beyond the strings each line needs. The `msg_idx` a [`Hit`] carries
//! is exactly the message's position in `app.messages`, so the plugin injection
//! contract (`PluginEvent::Custom { msg_idx, field, text }` addressing messages
//! by index) is untouched.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::diff::render_diff;
use super::exec::{parse_exec, render_exec};
use super::{BAR, PAD, THIN_BAR, role_color, wrapped_height};
use crate::types::{Message, Role};

/// Default view width used when a cell renders width-dependent bodies (diffs)
/// outside a live frame — e.g. `desired_height` in tests. The real transcript
/// re-wraps every line via [`wrapped_height`] at the true terminal width, so
/// this only affects the pre-wrap truncation budget, never final layout.
const DIFF_FALLBACK_WIDTH: u16 = 100;

/// The tool name is the first whitespace-token of a `Role::Tool` message's
/// `content` (the `"name  …"` contract from `gfx::tool_call_label`).
fn tool_name(msg: &Message) -> &str {
    msg.content.split_whitespace().next().unwrap_or("")
}

/// For `edit_file` / `write_file` tool calls, reconstruct the colored diff body
/// from the stored raw args JSON. Returns `None` for any other tool, missing
/// args, or unparseable JSON (caller falls back to the plain call line).
fn diff_lines(msg: &Message) -> Option<Vec<Line<'static>>> {
    let args = msg.args.as_deref()?;
    let value: serde_json::Value = serde_json::from_str(args).ok()?;
    let obj = value.as_object()?;
    let (old, new) = match tool_name(msg) {
        "edit_file" => (
            obj.get("old_string").and_then(|v| v.as_str()).unwrap_or(""),
            obj.get("new_string").and_then(|v| v.as_str()).unwrap_or(""),
        ),
        // write_file replaces the whole file: diff against empty (all-add).
        "write_file" => (
            "",
            obj.get("content").and_then(|v| v.as_str()).unwrap_or(""),
        ),
        _ => return None,
    };
    Some(render_diff(old, new, DIFF_FALLBACK_WIDTH))
}

/// For a `bash` tool call, the `$ command` prompt line(s) reconstructed from the
/// stored args JSON (`{command: "…"}`). Returns `None` for other tools / missing
/// args. Multi-line commands each get a `$`-prefixed row so heredocs read right.
fn exec_header_lines(msg: &Message) -> Option<Vec<Line<'static>>> {
    if tool_name(msg) != "bash" {
        return None;
    }
    let args = msg.args.as_deref()?;
    let value: serde_json::Value = serde_json::from_str(args).ok()?;
    let command = value.as_object()?.get("command")?.as_str()?;
    let style = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    Some(
        command
            .lines()
            .map(|cmd| {
                Line::from(vec![
                    Span::raw("     "),
                    Span::styled("$ ", style),
                    Span::styled(cmd.to_string(), Style::default().fg(Color::Gray)),
                ])
            })
            .collect(),
    )
}

/// Marks a rendered line as a clickable toggle target, carrying the message
/// index the click should act on. The driver turns these into the
/// `thought_hitboxes` / `tool_hitboxes` screen-row maps.
#[derive(Clone, Copy)]
pub(super) enum Hit {
    /// A reasoning-block header (expand/collapse the thought).
    Thought(usize),
    /// A tool-call or tool-output header (expand/collapse it).
    Tool(usize),
}

/// One rendered transcript line plus its optional clickable-toggle tag.
pub(super) struct CellLine {
    pub line: Line<'static>,
    pub hit: Option<Hit>,
}

impl CellLine {
    fn plain(line: Line<'static>) -> Self {
        Self { line, hit: None }
    }
    fn tagged(line: Line<'static>, hit: Hit) -> Self {
        Self { line, hit: Some(hit) }
    }
}

/// A transcript item that renders itself into styled lines.
pub(super) trait Cell {
    /// The styled lines this message contributes, top to bottom.
    fn display_lines(&self) -> Vec<CellLine>;

    /// Total wrapped height in visual rows at `width`. Defaults to summing
    /// [`wrapped_height`] over [`Self::display_lines`] — the same oracle the
    /// driver uses — so a cell's self-reported height can't drift from what the
    /// transcript actually lays out. Used by tests as a height oracle.
    #[cfg_attr(not(test), allow(dead_code))]
    fn desired_height(&self, width: u16) -> u16 {
        self.display_lines()
            .iter()
            .map(|cl| wrapped_height(&cl.line, width) as u16)
            .fold(0u16, |a, h| a.saturating_add(h))
    }
}

/// Build the cell for a message given its transcript index.
pub(super) fn build_cell(idx: usize, msg: &Message) -> Box<dyn Cell + '_> {
    match msg.role {
        Role::User => Box::new(UserCell { msg }),
        Role::Assistant => Box::new(AssistantCell { idx, msg }),
        Role::Tool => Box::new(ToolCell { idx, msg }),
        Role::ToolOutput => Box::new(ToolOutputCell { idx, msg }),
        Role::System => Box::new(SystemCell { msg }),
    }
}

// ---- shared line builders (moved verbatim from the old ui.rs) ----

/// A top-level user/assistant/system line: colored bar on the first line
/// (only for user messages), aligned indent on continuation lines.
fn top_level_line(index: usize, text: &str, color: Color, dim: bool, role: Role) -> Line<'static> {
    let content_style = if dim {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default()
    };
    if index == 0 {
        if role == Role::User {
            Line::from(vec![
                Span::raw(PAD),
                Span::styled(BAR, Style::default().fg(color).add_modifier(Modifier::BOLD)),
                Span::raw(" "),
                Span::styled(text.to_string(), content_style),
            ])
        } else {
            Line::from(vec![
                Span::raw("  "),
                Span::styled(text.to_string(), content_style),
            ])
        }
    } else {
        // Match first-line indent for this role: ▌ + space = 3 for user, 2 otherwise.
        let indent = if role == Role::User { "   " } else { "  " };
        Line::from(vec![Span::raw(indent), Span::styled(text.to_string(), content_style)])
    }
}

/// A tool invocation: `  ▎ name  args`, name emphasized, args dim.
fn tool_call_line(index: usize, text: &str, color: Color) -> Line<'static> {
    if index > 0 {
        return Line::from(vec![
            Span::raw("     "),
            Span::styled(text.to_string(), Style::default().fg(Color::DarkGray)),
        ]);
    }
    // Split "name  args" into a bright name and a dim remainder.
    let (name, rest) = match text.split_once("  ") {
        Some((n, r)) => (n, r),
        None => (text, ""),
    };
    let mut spans = vec![
        Span::raw("   "),
        Span::styled(THIN_BAR, Style::default().fg(color)),
        Span::raw(" "),
        Span::styled(name.to_string(), Style::default().fg(color).add_modifier(Modifier::BOLD)),
    ];
    if !rest.is_empty() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(rest.to_string(), Style::default().fg(Color::DarkGray)));
    }
    Line::from(spans)
}

/// Dim, indented tool output.
fn tool_output_line(text: &str) -> Line<'static> {
    Line::from(vec![
        Span::raw("     "),
        Span::styled(text.to_string(), Style::default().fg(Color::DarkGray)),
    ])
}

/// Trim leading / trailing blank lines (streaming artifacts) from `content`,
/// keeping internal blanks (paragraph breaks). Mirrors the old inline logic.
fn trimmed_content_lines(content: &str) -> Vec<&str> {
    let raw: Vec<&str> = if content.is_empty() {
        vec![""]
    } else {
        content.lines().collect()
    };
    let first = raw.iter().position(|l| !l.trim().is_empty()).unwrap_or(0);
    let last = raw.iter().rposition(|l| !l.trim().is_empty()).unwrap_or(0);
    if first <= last {
        raw[first..=last].to_vec()
    } else {
        Vec::new()
    }
}

/// The MemoryPlugin archival note (yellow), if present. Emitted right after a
/// message's main content, matching the old ordering.
fn memory_lines(msg: &Message) -> Vec<CellLine> {
    let mut out = Vec::new();
    if let Some(note) = &msg.memory
        && !note.is_empty()
    {
        for text in note.lines() {
            out.push(CellLine::plain(Line::from(vec![
                Span::raw("     "),
                Span::styled(text.to_string(), Style::default().fg(Color::Yellow)),
            ])));
        }
    }
    out
}
/// The reasoning block (assistant only): a clickable header row plus, when
/// expanded, the full chain-of-thought and its optional translation. Emitted
/// before the message's content, matching the old ordering.
fn reasoning_lines(idx: usize, msg: &Message) -> Vec<CellLine> {
    let mut out = Vec::new();
    if msg.reasoning.is_empty() {
        return out;
    }
    if msg.reasoning_collapsed {
        let secs = msg.reasoning_secs.unwrap_or(0);
        out.push(CellLine::tagged(
            Line::from(vec![
                Span::raw("   "),
                Span::styled(
                    format!("▸ thought for {secs}s"),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    "  (click to expand)",
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                ),
            ]),
            Hit::Thought(idx),
        ));
    } else {
        let header = match msg.reasoning_secs {
            Some(s) => format!("▾ thought for {s}s"),
            None => "▾ thinking".to_string(),
        };
        out.push(CellLine::tagged(
            Line::from(vec![
                Span::raw("   "),
                Span::styled(header, Style::default().fg(Color::DarkGray)),
                Span::styled(
                    "  (click to collapse)",
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                ),
            ]),
            Hit::Thought(idx),
        ));
        for text in msg.reasoning.lines() {
            if text.trim().is_empty() {
                continue;
            }
            out.push(CellLine::plain(Line::from(vec![
                Span::raw("     "),
                Span::styled(text.to_string(), Style::default().fg(Color::DarkGray)),
            ])));
        }
        // Translation (from TranslatePlugin), shown below the original.
        if let Some(translation) = &msg.translation
            && !translation.is_empty()
        {
            out.push(CellLine::plain(Line::from(vec![
                Span::raw("     "),
                Span::styled("⟡ ", Style::default().fg(Color::Yellow)),
                Span::styled(translation.to_string(), Style::default().fg(Color::Yellow)),
            ])));
        }
    }
    out
}

// ---- per-role cells ----

struct UserCell<'a> {
    msg: &'a Message,
}
impl Cell for UserCell<'_> {
    fn display_lines(&self) -> Vec<CellLine> {
        let color = role_color(Role::User);
        let mut out = Vec::new();
        for (i, text) in trimmed_content_lines(&self.msg.content).iter().enumerate() {
            out.push(CellLine::plain(top_level_line(i, text, color, false, Role::User)));
        }
        out.extend(memory_lines(self.msg));
        out
    }
}

struct SystemCell<'a> {
    msg: &'a Message,
}
impl Cell for SystemCell<'_> {
    fn display_lines(&self) -> Vec<CellLine> {
        let color = role_color(Role::System);
        let mut out = Vec::new();
        for (i, text) in trimmed_content_lines(&self.msg.content).iter().enumerate() {
            out.push(CellLine::plain(top_level_line(i, text, color, true, Role::System)));
        }
        out.extend(memory_lines(self.msg));
        out
    }
}

struct AssistantCell<'a> {
    idx: usize,
    msg: &'a Message,
}
impl Cell for AssistantCell<'_> {
    fn display_lines(&self) -> Vec<CellLine> {
        let color = role_color(Role::Assistant);
        let mut out = reasoning_lines(self.idx, self.msg);
        // An assistant turn with reasoning but no visible content yet: nothing
        // more to render (matches the old `continue`).
        if !self.msg.content.is_empty() {
            for (i, text) in trimmed_content_lines(&self.msg.content).iter().enumerate() {
                out.push(CellLine::plain(top_level_line(i, text, color, false, Role::Assistant)));
            }
        }
        out.extend(memory_lines(self.msg));
        out
    }
}

struct ToolCell<'a> {
    idx: usize,
    msg: &'a Message,
}
impl Cell for ToolCell<'_> {
    fn display_lines(&self) -> Vec<CellLine> {
        let color = role_color(Role::Tool);
        let mut out = Vec::new();

        if self.msg.tool_collapsed {
            // Old ordering: content is skipped, no explanation while collapsed,
            // memory note, THEN the clickable summary line.
            out.extend(memory_lines(self.msg));
            let summary = self.msg.content.lines().next().unwrap_or("");
            out.push(CellLine::tagged(
                Line::from(vec![
                    Span::raw("   "),
                    Span::styled(
                        format!("▸ {summary}"),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        "  (click to expand)",
                        Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                    ),
                ]),
                Hit::Tool(self.idx),
            ));
        } else {
            // Expanded: the call lines (first line is the collapse hitbox).
            for (i, text) in trimmed_content_lines(&self.msg.content).iter().enumerate() {
                let line = tool_call_line(i, text, color);
                if i == 0 {
                    out.push(CellLine::tagged(line, Hit::Tool(self.idx)));
                } else {
                    out.push(CellLine::plain(line));
                }
            }
            // Feature A: for edit_file/write_file, show a colored diff of the
            // change under the call line (reconstructed from the stored args).
            if let Some(diff) = diff_lines(self.msg) {
                out.extend(diff.into_iter().map(CellLine::plain));
            }
            // Feature B: for bash, show the `$ command` prompt under the call line.
            if let Some(header) = exec_header_lines(self.msg) {
                out.extend(header.into_iter().map(CellLine::plain));
            }
            // Tool explanation (from BashExplainPlugin).
            if let Some(explanation) = &self.msg.explanation
                && !explanation.is_empty()
            {
                out.push(CellLine::plain(Line::from(vec![
                    Span::raw("     "),
                    Span::styled("⟡ ", Style::default().fg(Color::Yellow)),
                    Span::styled(explanation.to_string(), Style::default().fg(Color::Yellow)),
                ])));
            }
            out.extend(memory_lines(self.msg));
        }

        out
    }
}

struct ToolOutputCell<'a> {
    idx: usize,
    msg: &'a Message,
}
impl Cell for ToolOutputCell<'_> {
    fn display_lines(&self) -> Vec<CellLine> {
        let mut out = Vec::new();

        // Old ordering for a ToolOutput message: (content preview only when
        // there's no full_content), memory note, THEN the output block.
        if self.msg.full_content.is_none() {
            for text in trimmed_content_lines(&self.msg.content) {
                out.push(CellLine::plain(tool_output_line(text)));
            }
        }
        out.extend(memory_lines(self.msg));

        match &self.msg.full_content {
            Some(full) => {
                let line_count = full.lines().count();
                if self.msg.output_collapsed {
                    out.push(CellLine::tagged(
                        Line::from(vec![
                            Span::raw("   "),
                            Span::styled(
                                format!("▸ output ({line_count} lines)"),
                                Style::default().fg(Color::DarkGray),
                            ),
                            Span::styled(
                                "  (click to expand)",
                                Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                            ),
                        ]),
                        Hit::Tool(self.idx),
                    ));
                } else {
                    out.push(CellLine::tagged(
                        Line::from(vec![
                            Span::raw("   "),
                            Span::styled(
                                format!("▾ output ({line_count} lines)"),
                                Style::default().fg(Color::DarkGray),
                            ),
                            Span::styled(
                                "  (click to collapse)",
                                Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                            ),
                        ]),
                        Hit::Tool(self.idx),
                    ));
                    // Feature B: a bash result parses into exit-badge + streams;
                    // any other tool output falls back to the plain dim block.
                    if let Some(view) = parse_exec(full) {
                        out.extend(render_exec(&view).into_iter().map(CellLine::plain));
                    } else {
                        for text in full.lines() {
                            out.push(CellLine::plain(tool_output_line(text)));
                        }
                    }
                }
            }
            None => {
                // Preview already emitted above (before the memory note).
            }
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `desired_height` must equal summing `wrapped_height` over the same cell's
    /// `display_lines` — i.e. a cell can never mis-report its own height versus
    /// the oracle the transcript loop lays out with. Uses a multi-line assistant
    /// message narrow enough to force at least one line to wrap.
    #[test]
    fn desired_height_matches_wrapped_oracle() {
        let long = "this assistant line is deliberately long enough that it must \
                    wrap across more than one visual row at a narrow width";
        let msg = Message::new(Role::Assistant, format!("{long}\nsecond paragraph line"));
        let cell = build_cell(0, &msg);

        for width in [24u16, 40, 80] {
            let oracle: u16 = cell
                .display_lines()
                .iter()
                .map(|cl| wrapped_height(&cl.line, width) as u16)
                .fold(0, |a, h| a.saturating_add(h));
            assert_eq!(
                cell.desired_height(width),
                oracle,
                "desired_height drifted from wrapped_height sum at width {width}"
            );
        }
    }

    /// A wrapping line makes the height strictly exceed the line count, so this
    /// isn't a trivially-true identity — the oracle is exercising real wrap math.
    #[test]
    fn narrow_width_forces_wrap() {
        let msg = Message::new(
            Role::Assistant,
            "one long unbroken sentence that clearly cannot fit in twenty columns",
        );
        let cell = build_cell(0, &msg);
        let lines = cell.display_lines().len() as u16;
        assert!(
            cell.desired_height(20) > lines,
            "expected wrapping to add rows beyond the {lines} logical lines"
        );
    }
}

