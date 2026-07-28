//! Colored unified-diff rendering for `edit_file` / `write_file` tool calls
//! (feature A).
//!
//! We do NOT shell out to git: the edit tool already carries the change in its
//! args (`old_string` → `new_string` for `edit_file`; `""` → `content` for
//! `write_file`). [`render_diff`] turns that pair into styled `-`/`+`/context
//! lines using [`similar::TextDiff::from_lines`], the same pure-Rust differ
//! codex uses.
//!
//! The inputs here are small by construction — the model supplies only a few
//! context lines around each edit — so we render *every* change (no hunking /
//! `@@` headers). Long lines are truncated to the view width so a pathological
//! single-line file can't blow the transcript up; egg's normal wrap handles the
//! rest.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use similar::{ChangeTag, TextDiff};

/// Left gutter for every diff row: 3 spaces to align under the tool-call
/// `   ▎ name` header emitted by [`super::cell`].
const GUTTER: &str = "   ";

/// Render `old` → `new` as colored diff lines. Deletions are red (`-`),
/// insertions green (`+`), unchanged context dim (` `). `write_file` passes
/// `old == ""`, yielding an all-insert diff.
pub(super) fn render_diff(old: &str, new: &str, width: u16) -> Vec<Line<'static>> {
    // Budget for the content portion of each row: total width minus the gutter
    // and the 2-col "sign + space" prefix. Keep at least a few columns.
    let content_cols = (width as usize)
        .saturating_sub(GUTTER.len() + 2)
        .max(8);

    let diff = TextDiff::from_lines(old, new);
    let mut lines: Vec<Line<'static>> = Vec::new();

    for change in diff.iter_all_changes() {
        let (sign, style) = match change.tag() {
            ChangeTag::Delete => ("-", Style::default().fg(Color::Red)),
            ChangeTag::Insert => ("+", Style::default().fg(Color::Green)),
            ChangeTag::Equal => (
                " ",
                Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
            ),
        };
        // `change.value()` keeps the trailing newline; strip it for display.
        let raw = change.value();
        let text = raw.strip_suffix('\n').unwrap_or(raw);
        let shown = truncate_to(text, content_cols);
        lines.push(Line::from(vec![
            Span::raw(GUTTER),
            Span::styled(format!("{sign} "), style),
            Span::styled(shown, style),
        ]));
    }

    lines
}

/// Truncate `s` to at most `cols` characters, appending `…` when clipped.
/// Char-based (not byte) so multibyte content isn't split mid-codepoint.
fn truncate_to(s: &str, cols: usize) -> String {
    if s.chars().count() <= cols {
        return s.to_string();
    }
    let keep = cols.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Extract the leading sign char of each rendered diff row (the second span
    /// is `"{sign} "`).
    fn signs(lines: &[Line<'static>]) -> Vec<char> {
        lines
            .iter()
            .map(|l| l.spans[1].content.chars().next().unwrap())
            .collect()
    }

    #[test]
    fn add_remove_and_context_counts() {
        let old = "a\nb\nc\n";
        let new = "a\nB\nc\n";
        let lines = render_diff(old, new, 80);
        let s = signs(&lines);
        // context a, delete b, insert B, context c.
        assert_eq!(s.iter().filter(|&&c| c == ' ').count(), 2);
        assert_eq!(s.iter().filter(|&&c| c == '-').count(), 1);
        assert_eq!(s.iter().filter(|&&c| c == '+').count(), 1);
    }

    #[test]
    fn write_file_is_all_insert() {
        let lines = render_diff("", "one\ntwo\nthree\n", 80);
        let s = signs(&lines);
        assert_eq!(s.len(), 3);
        assert!(s.iter().all(|&c| c == '+'));
    }

    #[test]
    fn styling_matches_tag() {
        let lines = render_diff("keep\ndrop\n", "keep\nadd\n", 80);
        for l in &lines {
            let sign = l.spans[1].content.chars().next().unwrap();
            let fg = l.spans[1].style.fg;
            match sign {
                '-' => assert_eq!(fg, Some(Color::Red)),
                '+' => assert_eq!(fg, Some(Color::Green)),
                ' ' => assert_eq!(fg, Some(Color::DarkGray)),
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn long_line_is_truncated_with_ellipsis() {
        let long = "x".repeat(200);
        let lines = render_diff("", &format!("{long}\n"), 40);
        let content = &lines[0].spans[2].content;
        assert!(content.ends_with('…'));
        assert!(content.chars().count() <= 40);
    }
}
