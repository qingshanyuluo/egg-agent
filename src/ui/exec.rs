//! Exec-command view for `bash` tool calls (feature B).
//!
//! The bash tool (`src/tools/bash.rs`) returns a fixed block:
//! ```text
//! exit code: N
//! --- stdout ---
//! …stdout…
//! --- stderr ---
//! …stderr…
//! ```
//! [`parse_exec`] splits that back into an [`ExecView`] so the paired
//! `ToolOutput` cell can render an exit-code badge (green 0 / red non-zero)
//! plus separately-styled stdout / stderr, instead of one dim blob.
//!
//! Timeouts (`error: command timed out after …`) and any other tool that
//! doesn't emit the `exit code:` prefix return `None`, so the caller falls back
//! to the generic preview — no behavior change for non-bash tools.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Left indent matching `cell::tool_output_line` (5 spaces).
const INDENT: &str = "     ";

const STDOUT_MARKER: &str = "--- stdout ---";
const STDERR_MARKER: &str = "--- stderr ---";

/// A parsed bash-tool result.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ExecView {
    /// Exit status: `Some(n)` for a numeric code, `None` when the process was
    /// killed by a signal (the tool prints `exit code: signal`).
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Parse a bash-tool output block. Returns `None` when `output` isn't a bash
/// result (no `exit code:` header) so the caller can fall back.
pub(super) fn parse_exec(output: &str) -> Option<ExecView> {
    let rest = output.strip_prefix("exit code: ")?;
    // The code is the remainder of the first line.
    let (code_str, after) = match rest.split_once('\n') {
        Some((c, a)) => (c, a),
        None => (rest, ""),
    };
    let code = code_str.trim().parse::<i32>().ok();

    // Split the body into optional stdout / stderr sections. The markers each
    // sit on their own line; everything up to the next marker is that stream.
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut target: Option<&mut String> = None;
    for line in after.split_inclusive('\n') {
        let trimmed = line.strip_suffix('\n').unwrap_or(line);
        match trimmed {
            STDOUT_MARKER => target = Some(&mut stdout),
            STDERR_MARKER => target = Some(&mut stderr),
            _ => {
                if let Some(buf) = target.as_deref_mut() {
                    buf.push_str(line);
                }
            }
        }
    }

    Some(ExecView {
        code,
        stdout: stdout.trim_end_matches('\n').to_string(),
        stderr: stderr.trim_end_matches('\n').to_string(),
    })
}

/// Render the exit-code badge and both streams as styled lines. stdout is
/// shown normal-dim, stderr in red; the badge is green for `0`, red otherwise.
pub(super) fn render_exec(view: &ExecView) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    let (badge, badge_style) = match view.code {
        Some(0) => (
            "● exit 0".to_string(),
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        ),
        Some(n) => (
            format!("● exit {n}"),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        None => (
            "● killed (signal)".to_string(),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
    };
    lines.push(Line::from(vec![
        Span::raw(INDENT),
        Span::styled(badge, badge_style),
    ]));

    for text in view.stdout.lines() {
        lines.push(Line::from(vec![
            Span::raw(INDENT),
            Span::styled(text.to_string(), Style::default().fg(Color::DarkGray)),
        ]));
    }
    for text in view.stderr.lines() {
        lines.push(Line::from(vec![
            Span::raw(INDENT),
            Span::styled(
                text.to_string(),
                Style::default().fg(Color::Red).add_modifier(Modifier::DIM),
            ),
        ]));
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_code_and_both_streams() {
        let block = "exit code: 0\n--- stdout ---\nhello\nworld\n--- stderr ---\nwarn\n";
        let v = parse_exec(block).unwrap();
        assert_eq!(v.code, Some(0));
        assert_eq!(v.stdout, "hello\nworld");
        assert_eq!(v.stderr, "warn");
    }

    #[test]
    fn parses_nonzero_with_only_stderr() {
        let block = "exit code: 1\n--- stderr ---\nboom\n";
        let v = parse_exec(block).unwrap();
        assert_eq!(v.code, Some(1));
        assert_eq!(v.stdout, "");
        assert_eq!(v.stderr, "boom");
    }

    #[test]
    fn signal_exit_has_no_numeric_code() {
        let block = "exit code: signal\n--- stdout ---\n\n";
        let v = parse_exec(block).unwrap();
        assert_eq!(v.code, None);
    }

    #[test]
    fn non_bash_output_is_not_parsed() {
        assert!(parse_exec("error: command timed out after 30s").is_none());
        assert!(parse_exec("some other tool result").is_none());
    }

    #[test]
    fn badge_color_tracks_exit_code() {
        let ok = render_exec(&ExecView { code: Some(0), stdout: String::new(), stderr: String::new() });
        assert_eq!(ok[0].spans[1].style.fg, Some(Color::Green));
        let bad = render_exec(&ExecView { code: Some(2), stdout: String::new(), stderr: String::new() });
        assert_eq!(bad[0].spans[1].style.fg, Some(Color::Red));
    }
}
