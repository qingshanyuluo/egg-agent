//! Input box + status line: the bottom two regions of the layout.
//!
//! All the wrapping/cursor math here delegates to the parent module's
//! [`super::wrapped_height`] / [`super::wrapped_last_row`] so the input box
//! wraps and places its cursor with the exact same `WordWrapper` the transcript
//! uses — no drift between the height reserved and the text drawn.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use super::{INPUT_MAX_INNER_ROWS, wrapped_height, wrapped_last_row};
use crate::app::App;

pub(super) fn draw_prompt(frame: &mut Frame, app: &App, area: Rect) {
    let border_color = if app.running {
        Color::DarkGray
    } else {
        Color::Cyan
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(" input ");
    frame.render_widget(Clear, area);

    let text: Vec<Line> = if app.input.is_empty() {
        vec![Line::from(Span::styled(
            "Type a message…  (Alt/Shift+Enter newline · ↑↓ history · @ file · Ctrl+X editor)",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ))]
    } else {
        input_logical_lines(&app.input)
            .into_iter()
            .map(|l| Line::from(Span::raw(l.to_string())))
            .collect()
    };

    frame.render_widget(
        Paragraph::new(text)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((input_scroll_offset(&app.input, area), 0)),
        area,
    );
}

/// Split the input into the logical lines the box renders, restoring the
/// trailing blank line that `str::lines()` drops when the buffer ends with a
/// newline (so `Alt+Enter` shows a fresh empty row).
pub(super) fn input_logical_lines(input: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = input.lines().collect();
    if input.is_empty() || input.ends_with('\n') {
        lines.push("");
    }
    lines
}

/// Number of wrapped (visual) rows the given input text occupies inside a box of
/// the given inner width.
///
/// This mirrors the wrapping used when rendering the input EXACTLY by delegating
/// to ratatui's own `WordWrapper` (`wrapped_height`) per logical line, instead
/// of a naive character-width division. The naive math over-/under-counts
/// whenever a line wraps at a word boundary, which is what made the cursor drift
/// away from the text and pushed the last line out of view past two rows.
pub(super) fn input_wrapped_rows(input: &str, inner_w: u16) -> u16 {
    let inner_w = inner_w.max(1);
    let mut rows = 0u16;
    for line in input_logical_lines(input) {
        let l = Line::from(Span::raw(line));
        rows = rows.saturating_add(wrapped_height(&l, inner_w) as u16);
    }
    rows.max(1)
}

/// Vertical scroll offset (in visual rows) for the input paragraph. Zero while
/// the content fits within the capped box; once it overflows we scroll so the
/// last (cursor) row stays pinned to the bottom, letting the cursor move up and
/// down through the buffer.
pub(super) fn input_scroll_offset(input: &str, area: Rect) -> u16 {
    let inner_w = area.width.saturating_sub(2).max(1);
    let rows = input_wrapped_rows(input, inner_w);
    rows.saturating_sub(INPUT_MAX_INNER_ROWS)
}

/// Compute the (x, y) cursor position inside the bordered input box, placing it
/// at the end of the last visual line of input text (accounting for wrapping).
///
/// Wrapping math delegates to the same ratatui `WordWrapper` the renderer uses
/// (`wrapped_height`) so the cursor never drifts from the drawn text, even when
/// a line wraps at a word boundary.
pub(super) fn input_cursor_pos(app: &App, area: Rect) -> (u16, u16) {
    let block = Block::default().borders(Borders::ALL);
    let inner = block.inner(area);
    let inner_w = inner.width.max(1);

    let text: &str = app.input.as_str();
    if text.is_empty() {
        return (inner.x, inner.y);
    }

    let logical = input_logical_lines(text);
    // Safe: `input_logical_lines` always returns at least one element.
    let (last, preceding) = logical.split_last().unwrap();

    // All preceding logical lines contribute their full wrapped-row count.
    let mut visual_row: u16 = 0;
    for line in preceding {
        let l = Line::from(Span::raw(*line));
        visual_row = visual_row.saturating_add(wrapped_height(&l, inner_w) as u16);
    }

    // Rows the last line itself occupies once wrapped; the cursor sits at the
    // end of its final row.
    let last_line = Line::from(Span::raw(*last));
    let (last_rows, cursor_col) = wrapped_last_row(&last_line, inner_w);
    visual_row = visual_row.saturating_add(last_rows.saturating_sub(1));

    // When the buffer overflows the capped box the paragraph is scrolled, so
    // shift the cursor up by the same offset and clamp to the visible rows.
    let scroll = input_scroll_offset(text, area);
    let visible_row = visual_row
        .saturating_sub(scroll)
        .min(inner.height.saturating_sub(1));

    (inner.x + cursor_col.min(inner_w), inner.y + visible_row)
}

pub(super) fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let state = if app.running { "working…" } else { "ready" };
    let scroll = if app.is_scrolled_back() {
        format!(" · scrolled ↑{}", app.scroll_back.get())
    } else {
        String::new()
    };
    // Optional layout HUD: `EGG_DEBUG=1` appends live scroll/height figures so a
    // "tail cut off" report can be eyeballed without reading the log file. The
    // env var is read once (OnceLock) to avoid a syscall every frame.
    let hud = if debug_hud_enabled() {
        format!(
            " · rows {}/{} back {} vh {}",
            app.total_rows.get().saturating_sub(app.scroll_back.get()),
            app.total_rows.get(),
            app.scroll_back.get(),
            app.view_height.get(),
        )
    } else {
        String::new()
    };
    let text = format!(
        " {} · {} · {}{}{} · ↑↓ history · Enter send · Ctrl+C quit",
        app.provider, app.model, state, scroll, hud
    );
    let status = Paragraph::new(Line::from(Span::styled(
        text,
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(status, area);
}

/// Whether the `EGG_DEBUG` layout HUD is on (checked once for the process).
fn debug_hud_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("EGG_DEBUG")
            .map(|v| v != "0" && !v.is_empty())
            .unwrap_or(false)
    })
}
