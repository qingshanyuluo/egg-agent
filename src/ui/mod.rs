//! Borderless, whitespace-driven transcript UI.
//!
//! Style goals (mirroring mature terminal agents): no boxes, a colored gutter
//! bar per message, breathing room between turns, dim secondary text for tool
//! activity, and a single prompt line with a compact status line beneath it.
//!
//! ## Module layout
//!
//! `ui.rs` used to be one ~1500-line file whose `draw_transcript` mixed wrap
//! math, scroll offset, three kinds of hitbox, and selection highlighting for
//! every message role at once. It is now split so each concern is isolated:
//!
//! - [`cell`] — the [`cell::Cell`] trait: each message role renders *itself*
//!   into `CellLine`s (a line plus an optional hitbox tag).
//! - [`transcript`] — the thin driver: build cells, run the scroll / tail-pin
//!   loop, register hitboxes, apply the selection highlight.
//! - [`diff`] / [`exec`] — diff view (edit_file/write_file) and exec view
//!   (bash) bodies used by the tool cells.
//! - [`overlay`] — command menu, model picker, connect wizard.
//! - [`input`] — prompt box, status line, input-wrapping/cursor math.
//! - [`splash`] — startup egg + "copied" toast.
//!
//! `wrapped_height` (this module) stays THE single source of truth for wrapped
//! height, delegating to ratatui's own `Paragraph::line_count`.

mod cell;
mod diff;
mod exec;
mod input;
mod overlay;
mod splash;
mod transcript;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Color;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Role};

/// Left margin so content doesn't hug the terminal edge.
const PAD: &str = " ";
/// Solid bar that heads each top-level message.
const BAR: &str = "▌";
/// Thin bar for nested tool lines.
const THIN_BAR: &str = "▎";
/// Braille spinner frames for the "waiting for first token" animation.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Maximum on-screen height of the input box, borders included. Beyond this the
/// box stops growing and instead scrolls so the cursor stays visible.
const INPUT_MAX_HEIGHT: u16 = 12;
/// Visible content rows inside the input box at its maximum size (minus borders).
const INPUT_MAX_INNER_ROWS: u16 = INPUT_MAX_HEIGHT - 2;

/// ASCII egg shown on the splash screen of a fresh conversation.
/// Every row is the same display width so centering is trivial.
const EGG_ART: [&str; 9] = [
    "      ▄▄▄▄▄▄      ",
    "    ▄██    ██▄    ",
    "   ██        ██   ",
    "  ██    ◉◉    ██  ",
    "  ██          ██  ",
    "  ██          ██  ",
    "   ██        ██   ",
    "    ▀██    ██▀    ",
    "      ▀▀▀▀▀▀      ",
];

pub fn draw(frame: &mut Frame, app: &App) {
    // The input box height grows with content up to a cap; past the cap it
    // scrolls (see `input_scroll_offset`). Compute the wrapped row count inside
    // the bordered block, then add 2 for the border lines.
    let inner_w = frame.area().width.saturating_sub(2);
    let visual_rows = input::input_wrapped_rows(&app.input, inner_w);
    let input_height = (visual_rows.max(1) + 2).min(INPUT_MAX_HEIGHT); // +2 borders

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),                // transcript
            Constraint::Length(1),             // spacer
            Constraint::Length(input_height),  // input box
            Constraint::Length(1),             // status
        ])
        .split(frame.area());

    if app.show_splash && app.messages.is_empty() {
        app.splash_visible.set(true);
        splash::draw_splash(frame, app, chunks[0]);
    } else {
        app.splash_visible.set(false);
        transcript::draw_transcript(frame, app, chunks[0]);
    }
    input::draw_prompt(frame, app, chunks[2]);
    input::draw_status(frame, app, chunks[3]);

    // The `@`-file popup floats just above the input box (feature D). Drawn
    // before the modal overlay so a modal, if somehow both were open, wins.
    if let Some(popup) = &app.file_popup {
        overlay::draw_file_popup(frame, popup, chunks[2]);
    }

    if let Some(overlay) = &app.overlay {
        overlay::draw_overlay(frame, app, overlay, frame.area());
    } else {
        // Place the cursor at the end of the last input line.
        frame.set_cursor_position(input::input_cursor_pos(app, chunks[2]));
    }

    // "copied" toast, top-right, above everything.
    if app.toast_active() {
        splash::draw_toast(frame, frame.area());
    }
}

/// Color assigned to each message role's gutter/content.
fn role_color(role: Role) -> Color {
    match role {
        Role::User => Color::Cyan,
        Role::Assistant => Color::Green,
        Role::Tool => Color::Yellow,
        Role::ToolOutput => Color::DarkGray,
        Role::System => Color::DarkGray,
    }
}

/// Number of screen rows a single logical line occupies once wrapped to `width`.
///
/// This is THE single source of truth for wrapping height. It delegates to
/// ratatui's own `Paragraph::line_count`, which internally runs the exact same
/// `WordWrapper` (word-boundary wrapping, `trim: false`) that the final render
/// uses — so the count can never drift from what actually gets drawn.
///
/// Wrapping is per-input-line independent inside `WordWrapper` (each line resets
/// the pending-word/whitespace state), so counting line-by-line and summing is
/// identical to counting the whole transcript at once.
fn wrapped_height(line: &Line, width: u16) -> usize {
    if width == 0 {
        return 1;
    }
    // `line_count` returns 0 for empty text; a blank logical line still occupies
    // one screen row, so clamp to at least 1.
    Paragraph::new(line.clone())
        .wrap(Wrap { trim: false })
        .line_count(width)
        .max(1)
}

/// Wrap `line` to `width` exactly as the input box renders it, and report
/// `(rows, last_row_fill)`:
///   - `rows`   — number of visual rows the line occupies (>= 1),
///   - `last_row_fill` — display columns occupied on the FINAL visual row,
///     i.e. where the cursor sits after the last character.
///
/// The fill width is read back from a scratch `Buffer` that the same
/// `Paragraph`/`WordWrapper` renders into, so it matches the on-screen layout
/// byte-for-byte. A naive `total_width % width` breaks under word-boundary
/// wrapping (earlier rows are often short because a whole word was pushed down),
/// which is what made the cursor drift a few columns past the text.
fn wrapped_last_row(line: &Line, width: u16) -> (u16, u16) {
    let width = width.max(1);
    let rows = wrapped_height(line, width) as u16;

    // A line that fits on one visual row: the fill is just its display width
    // (clamped). This also preserves any trailing spaces, which a buffer scan
    // below could not distinguish from blank cells.
    if rows <= 1 {
        let w = UnicodeWidthStr::width(line_text(line).as_str()) as u16;
        return (1, w.min(width));
    }

    // Multi-row: render into a scratch buffer with the SAME `WordWrapper` the
    // input box uses, then measure the rightmost non-blank cell on the final
    // visual row. This tracks word-boundary wrapping exactly, where a naive
    // `total_width % width` drifts.
    let area = Rect::new(0, 0, width, rows);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    Paragraph::new(line.clone())
        .wrap(Wrap { trim: false })
        .render(area, &mut buf);

    let last_y = rows.saturating_sub(1);
    let mut fill = 0u16;
    for x in 0..width {
        if let Some(cell) = buf.cell((x, last_y))
            && cell.symbol() != " "
            && !cell.symbol().is_empty()
        {
            fill = x + 1;
        }
    }
    (rows, fill.min(width))
}

/// Concatenated text of a `Line`'s spans (no styling), for width measurement.
fn line_text(line: &Line) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// Plain text of a line for clipboard copy, with the leading gutter decoration
/// (indent + bar/marker glyph) stripped so the copied text is clean.
fn line_plaintext(line: &Line) -> String {
    let raw: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    strip_gutter(&raw)
}

/// Remove leading indentation and any gutter marker (`▌ ▎ ▸ ▾`) so copied text
/// doesn't carry the UI's decoration. Leaves the content otherwise untouched.
fn strip_gutter(s: &str) -> String {
    let t = s.trim_start_matches(' ');
    let t = t
        .strip_prefix('▌')
        .or_else(|| t.strip_prefix('▎'))
        .or_else(|| t.strip_prefix('▸'))
        .or_else(|| t.strip_prefix('▾'))
        .unwrap_or(t);
    t.trim_start_matches(' ').to_string()
}

// Test-only re-imports so the migrated `#[cfg(test)] mod tests` below (which
// keeps its original `use super::*`) still resolves the input helpers that now
// live in `input.rs`, plus `Span` used by the test fixtures. Gated on `test` so
// release builds see no unused imports.
#[cfg(test)]
use ratatui::text::Span;
#[cfg(test)]
use input::{input_cursor_pos, input_scroll_offset, input_wrapped_rows};

#[cfg(test)]
mod tests {
    //! Snapshot / invariant regression tests for the transcript renderer.
    //!
    //! These guard the original bug: after a long conversation the tail of the
    //! transcript scrolled out of view because a hand-rolled `ceil(w/term_w)`
    //! line count drifted from ratatui's real `WordWrapper`. The fix made
    //! `wrapped_height` (→ `Paragraph::line_count`) the single source of truth.
    //! The tests below assert (a) `wrapped_height` equals the *actually rendered*
    //! row count across CJK / long-word / exact-boundary inputs, and (b) the last
    //! transcript line lands on the bottom row of the transcript area for a range
    //! of widths and message counts.

    use super::*;
    use crate::app::App;
    use crate::types::{Message, Role};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Build an `App` in "following the tail" state (not scrolled back, splash
    /// off) with the given transcript messages.
    fn app_with(messages: Vec<Message>) -> App {
        let mut app = App::new("test-model".into(), "test-provider".into());
        app.show_splash = false;
        app.messages = messages;
        app
    }

    /// Render `line` alone into a tall buffer of the given width and count how
    /// many rows it actually occupies (rows containing any non-space cell, plus
    /// the untouched trailing blank rows of a wrapped-but-empty line handled by
    /// the caller). This is the ground truth the renderer produces on screen.
    fn rendered_rows(line: &Line, width: u16) -> usize {
        let height = 200u16; // comfortably taller than any single wrapped line here
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| {
                let para = Paragraph::new(line.clone()).wrap(Wrap { trim: false });
                f.render_widget(para, f.area());
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let mut last_nonblank: Option<u16> = None;
        for y in 0..height {
            let mut has_ink = false;
            for x in 0..width {
                if buf[(x, y)].symbol() != " " {
                    has_ink = true;
                    break;
                }
            }
            if has_ink {
                last_nonblank = Some(y);
            }
        }
        // `+1` to turn a 0-based row index into a count; an all-blank line still
        // occupies one row, matching `wrapped_height`'s `.max(1)`.
        last_nonblank.map_or(1, |y| y as usize + 1)
    }

    /// The whole point of the fix: our height oracle must match what ratatui
    /// actually paints, or the tail scrolls off. Covers CJK (double-width),
    /// a single unbreakable long word, and text sized to the exact wrap column.
    #[test]
    fn wrapped_height_matches_rendered_rows() {
        let cases: Vec<(&str, Line)> = vec![
            ("ascii short", Line::from("hello world")),
            (
                "cjk long",
                Line::from("你好世界这是一段很长的中文文本用来测试折行是否正确".to_string()),
            ),
            (
                "unbreakable long word",
                Line::from("x".repeat(250)),
            ),
            (
                "mixed cjk + ascii",
                Line::from("prefix 中文 middle 更多的中文字符 suffix tail words here".to_string()),
            ),
            (
                "leading gutter + long",
                Line::from(vec![
                    Span::raw("     "),
                    Span::raw("a fairly long sentence that will certainly wrap several times across".to_string()),
                ]),
            ),
        ];

        for width in [10u16, 20, 24, 40, 80] {
            for (label, line) in &cases {
                let oracle = wrapped_height(line, width);
                let actual = rendered_rows(line, width);
                assert_eq!(
                    oracle, actual,
                    "wrapped_height drift: case={label:?} width={width} \
                     oracle={oracle} actual={actual}"
                );
            }
        }
    }

    /// An exact wrap-boundary is the classic off-by-one trap: a line whose width
    /// is an exact multiple of the terminal width must NOT gain a phantom extra
    /// row. Check widths where `content_width % width == 0`.
    #[test]
    fn wrapped_height_exact_boundary_no_phantom_row() {
        for width in [8u16, 10, 16, 20] {
            for mult in 1..=4u16 {
                let line = Line::from("a".repeat((width * mult) as usize));
                let oracle = wrapped_height(&line, width);
                let actual = rendered_rows(&line, width);
                assert_eq!(
                    oracle, actual,
                    "boundary drift: width={width} mult={mult} oracle={oracle} actual={actual}"
                );
            }
        }
    }

    /// Read the full plaintext of a rendered buffer row (trimmed of trailing
    /// spaces) so we can locate content on screen.
    fn row_string(buf: &ratatui::buffer::Buffer, y: u16, width: u16) -> String {
        let mut s = String::new();
        for x in 0..width {
            s.push_str(buf[(x, y)].symbol());
        }
        s.trim_end().to_string()
    }
    /// The core regression: after rendering while following the tail, the last
    /// transcript line's text must be *visible* in the transcript region and
    /// nothing may render below it — i.e. the tail is never scrolled out of view.
    ///
    /// Two valid shapes:
    ///   * content overflows the viewport → marker sits on the very bottom row
    ///     (classic "stick to bottom"); this is the case the original bug broke.
    ///   * content fits with room to spare → marker sits on its natural row and
    ///     every row below it is blank.
    /// Both reduce to: the marker is present, and it is the last non-blank row.
    ///
    /// The transcript occupies the top region of the layout; below it sit a
    /// 1-row spacer, the input box (≥3 rows), and a 1-row status line.
    fn assert_tail_pinned(width: u16, height: u16, n_msgs: usize) {
        // Give the last message a unique, searchable marker as its final line.
        let marker = "ZZ_TAIL_MARKER_ZZ";
        let mut messages = Vec::new();
        for i in 0..n_msgs {
            let body = if i == n_msgs - 1 {
                format!(
                    "message {i} body with some length so it wraps a bit on narrow \
                     terminals and exercises the wrapper\n{marker}"
                )
            } else {
                format!(
                    "message {i} 这是一条包含中文的消息 with mixed content that is long \
                     enough to wrap across several visual rows on a narrow terminal window"
                )
            };
            messages.push(Message::new(Role::User, body));
        }
        let app = app_with(messages);

        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
        let buf = terminal.backend().buffer().clone();

        // Layout (see `draw`): transcript = Min(1); then spacer(1), input(3..=12),
        // status(1). Input height here is 3 (single empty input line + 2 borders).
        let input_h = 3u16;
        let transcript_h = height - 1 /*spacer*/ - input_h - 1 /*status*/;

        // Find the last non-blank row within the transcript region and assert it
        // is the marker line. If the tail had scrolled out of view, the marker
        // would be absent and some *other* content row would be the last one.
        let mut last_content: Option<u16> = None;
        for y in 0..transcript_h {
            if !row_string(&buf, y, width).is_empty() {
                last_content = Some(y);
            }
        }
        let dump = || {
            (0..transcript_h)
                .map(|y| format!("  {y:>2}| {}", row_string(&buf, y, width)))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let last = last_content.unwrap_or_else(|| {
            panic!(
                "transcript region is entirely blank: width={width} height={height} n_msgs={n_msgs}"
            )
        });
        let last_row = row_string(&buf, last, width);
        assert!(
            last_row.contains(marker),
            "tail not pinned: width={width} height={height} n_msgs={n_msgs}\n\
             last non-blank transcript row {last} = {last_row:?}, expected it to hold {marker:?}\n\
             transcript region:\n{}",
            dump()
        );
    }

    #[test]
    fn tail_pinned_various_widths() {
        // Narrow terminals wrap the most and are where the old drift bit hardest.
        for width in [20u16, 24, 40, 80, 120] {
            assert_tail_pinned(width, 24, 40);
        }
    }

    #[test]
    fn tail_pinned_many_messages() {
        // A long conversation is the reported failure scenario.
        for n in [1usize, 5, 50, 200] {
            assert_tail_pinned(40, 24, n);
        }
    }

    #[test]
    fn tail_pinned_short_terminal() {
        // A very short transcript window (only a couple of rows) still pins.
        assert_tail_pinned(40, 8, 30);
    }

    /// When following the tail (not scrolled back), the layout invariant the
    /// renderer `debug_assert!`s must hold: `visual_scroll + view_height >=
    /// total_rows`. We can't read `visual_scroll` directly, but `total_rows` and
    /// `view_height` are published to the `App` each frame, and when not scrolled
    /// back `visual_scroll == max(0, total_rows - view_height)`, so the invariant
    /// reduces to a tautology *iff* the published totals are self-consistent.
    /// This asserts those published values are sane (view_height > 0, totals set).
    #[test]
    fn published_totals_are_set_after_draw() {
        let app = app_with(vec![Message::new(
            Role::User,
            "hello 世界 this wraps eventually across the width of a modest window".to_string(),
        )]);
        let mut terminal = Terminal::new(TestBackend::new(40, 24)).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();

        assert!(app.view_height.get() > 0, "view_height not published");
        assert!(app.total_rows.get() > 0, "total_rows not published");
        // Following the tail => scroll_back stays clamped at 0.
        assert_eq!(app.scroll_back.get(), 0, "unexpected scroll_back while following tail");
    }
    /// Regression for the resize-clamp-staleness bug (task #4): scroll back near
    /// the top at a narrow width (many wrapped rows → large max_scroll_back),
    /// then re-render at a much wider width (far fewer rows → smaller max). The
    /// per-frame `clamp_scroll` must pull `scroll_back` down to the new max so we
    /// never end up scrolled *past* the top with a blank viewport.
    #[test]
    fn resize_wider_reclamps_scroll_back() {
        let mut messages = Vec::new();
        for i in 0..30 {
            messages.push(Message::new(
                Role::User,
                format!("message {i} 这是一段中文 with enough text to wrap several rows when narrow"),
            ));
        }
        let mut app = app_with(messages);

        // Frame 1 — narrow, tall content. Scroll almost to the very top.
        let mut narrow = Terminal::new(TestBackend::new(20, 24)).unwrap();
        narrow.draw(|f| draw(f, &app)).unwrap();
        for _ in 0..500 {
            app.scroll_up(); // saturates at the narrow-frame max_scroll_back
        }
        let narrow_max = app.total_rows.get().saturating_sub(app.view_height.get());
        assert_eq!(
            app.scroll_back.get(),
            narrow_max,
            "scroll_up should saturate at the narrow frame's max"
        );

        // Frame 2 — much wider: fewer wrapped rows, so a smaller valid max. The
        // stale narrow `scroll_back` now exceeds it and MUST be clamped down.
        let mut wide = Terminal::new(TestBackend::new(120, 24)).unwrap();
        wide.draw(|f| draw(f, &app)).unwrap();
        let wide_max = app.total_rows.get().saturating_sub(app.view_height.get());
        assert!(
            app.scroll_back.get() <= wide_max,
            "resize did not re-clamp scroll_back: scroll_back={} > wide_max={wide_max}",
            app.scroll_back.get()
        );

        // And the transcript must not be blank at the top (proof we didn't scroll
        // past the content): the first transcript row has ink.
        let buf = wide.backend().buffer();
        let first_row = row_string(buf, 0, 120);
        assert!(
            !first_row.is_empty(),
            "top transcript row is blank after resize — scrolled past content"
        );
    }

    /// The input box only scrolls once the content exceeds the capped inner
    /// height; before that the offset is zero so the whole buffer is visible.
    #[test]
    fn input_scroll_offset_kicks_in_past_cap() {
        let area = Rect { x: 0, y: 0, width: 40, height: INPUT_MAX_HEIGHT };
        let inner_w = area.width - 2;

        // A few short lines fit entirely: no scroll.
        let short = "a\nb\nc";
        assert_eq!(input_scroll_offset(short, area), 0);
        assert_eq!(input_wrapped_rows(short, inner_w), 3);

        // Exactly filling the visible rows still needs no scroll.
        let exact: String = (0..INPUT_MAX_INNER_ROWS).map(|_| "x\n").collect();
        // trailing '\n' adds a blank row, so drop it for the exact-fit case.
        let exact = exact.trim_end_matches('\n');
        assert_eq!(input_wrapped_rows(exact, inner_w), INPUT_MAX_INNER_ROWS);
        assert_eq!(input_scroll_offset(exact, area), 0);

        // Two rows past the cap → scroll by two so the bottom stays visible.
        let overflow: String = (0..INPUT_MAX_INNER_ROWS + 2).map(|_| "x\n").collect();
        let overflow = overflow.trim_end_matches('\n');
        assert_eq!(input_scroll_offset(overflow, area), 2);
    }

    /// When the buffer overflows, the cursor must stay on the last visible row
    /// of the box rather than running off the bottom.
    #[test]
    fn input_cursor_stays_visible_when_overflowing() {
        let area = Rect { x: 0, y: 0, width: 40, height: INPUT_MAX_HEIGHT };
        let mut app = App::new("m".into(), "p".into());
        // Many lines, cursor conceptually at the end.
        app.input = (0..30).map(|i| format!("line{i}\n")).collect();
        let (_x, y) = input_cursor_pos(&app, area);
        // Inner area starts at y+1 (top border) and is INPUT_MAX_INNER_ROWS tall.
        let last_inner_row = area.y + 1 + INPUT_MAX_INNER_ROWS - 1;
        assert_eq!(
            y, last_inner_row,
            "cursor should sit on the last visible input row when overflowing"
        );
    }

    /// Regression: the input-height count and the cursor row must both agree
    /// with ratatui's real word-wrapper, even when a logical line wraps at a
    /// word boundary (where naive char-width division would drift and push the
    /// cursor/tail off-screen). The cursor row must never exceed the box height.
    #[test]
    fn input_wrapping_matches_word_wrapper() {
        let width = 20u16;
        let inner_w = width - 2; // 18 columns inside the border
        let area = Rect { x: 0, y: 0, width, height: INPUT_MAX_HEIGHT };

        // A run of words that the WordWrapper breaks at spaces rather than
        // mid-word, so the visual row count is decided by word boundaries.
        let text = "the quick brown fox jumps over the lazy dog again and again";
        let mut app = App::new("m".into(), "p".into());
        app.input = text.to_string();

        // The height helper must equal ratatui's own line_count for the block.
        let expected = wrapped_height(&Line::from(Span::raw(text)), inner_w) as u16;
        assert_eq!(input_wrapped_rows(text, inner_w), expected.max(1));

        // The cursor row stays inside the (unscrolled) box for a single line
        // that wraps to a handful of rows.
        let (_x, y) = input_cursor_pos(&app, area);
        assert!(
            y >= area.y + 1 && y <= area.y + 1 + INPUT_MAX_INNER_ROWS - 1,
            "cursor row {y} escaped the input box"
        );
    }

    /// True fill width of the last visual row when `line` is wrapped to `width`,
    /// measured from a real render — the oracle for `wrapped_last_row`'s column.
    fn rendered_last_row_fill(line: &Line, width: u16) -> u16 {
        let rows = rendered_rows(line, width) as u16;
        let mut terminal =
            Terminal::new(TestBackend::new(width, rows.max(1) + 2)).unwrap();
        terminal
            .draw(|f| {
                let para = Paragraph::new(line.clone()).wrap(Wrap { trim: false });
                f.render_widget(para, f.area());
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let last_y = rows.saturating_sub(1);
        let mut fill = 0u16;
        for x in 0..width {
            if buf[(x, last_y)].symbol() != " " {
                fill = x + 1;
            }
        }
        fill
    }

    /// Bug 2 regression: the cursor column must equal the *actual* fill of the
    /// wrapped last row. The old code used `total_width % inner_w`, which drifts
    /// whenever the WordWrapper breaks earlier rows at a space (so the last row
    /// is not simply the arithmetic remainder). These are exactly the cases
    /// where users saw the cursor land a few cells off from the text.
    #[test]
    fn cursor_column_matches_rendered_last_row() {
        let cases: Vec<&str> = vec![
            "the quick brown fox jumps over the lazy dog again and again",
            "short",
            "one two three four five six seven eight nine ten eleven twelve",
            "supercalifragilisticexpialidocious and a few more trailing words",
            "你好 世界 这是 一段 中文 用来 测试 折行 光标 列 是否 对齐 的 文本",
        ];
        for width in [12u16, 18, 24, 40] {
            for text in &cases {
                let line = Line::from(Span::raw(*text));
                let (_rows, col) = wrapped_last_row(&line, width);
                let oracle = rendered_last_row_fill(&line, width).min(width);
                assert_eq!(
                    col, oracle,
                    "cursor column drift: text={text:?} width={width} \
                     got={col} oracle={oracle}"
                );
            }
        }
    }


}


