//! The thin transcript driver.
//!
//! This is what remains of the old ~350-line `draw_transcript` body once every
//! per-role rendering branch moved into [`super::cell`]. Its job is now purely
//! *control flow*, unchanged from before:
//!
//! 1. build one [`Cell`](super::cell::Cell) per message and flatten their lines
//!    into a single `Vec<Line>` (plus a parallel list of `(flat_idx, msg_idx)`
//!    for each clickable thought / tool header, reconstructed from the cells'
//!    [`Hit`](super::cell::Hit) tags),
//! 2. pre-compute wrapped visual positions via [`super::wrapped_height`] (the
//!    single source of truth),
//! 3. publish totals + re-clamp scroll, pick the tail/scrolled-back offset,
//! 4. register thought/tool hitboxes and per-row plaintext, apply the selection
//!    highlight,
//! 5. render via `split_off` + `Paragraph::scroll` (u16-overflow-safe).
//!
//! The wrap/scroll/hitbox math is byte-for-byte the same as the monolith, so
//! the migrated tail-pin / resize-reclamp tests pass unchanged.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::cell::{Hit, build_cell};
use super::{PAD, SPINNER, line_plaintext, wrapped_height};
use crate::app::App;

pub(super) fn draw_transcript(frame: &mut Frame, app: &App, area: Rect) {
    // Flatten every cell's lines into one styled list, then show the tail that
    // fits so the newest content always stays in view ("stick to bottom").
    let mut lines: Vec<Line> = Vec::new();
    // Flat line index -> message index, for each clickable "thought" line.
    let mut thought_rows: Vec<(usize, usize)> = Vec::new();
    // Flat line index -> message index, for each clickable tool/output line.
    let mut tool_rows: Vec<(usize, usize)> = Vec::new();

    for (msg_idx, message) in app.messages.iter().enumerate() {
        let cell = build_cell(msg_idx, message);
        for cl in cell.display_lines() {
            match cl.hit {
                Some(Hit::Thought(mi)) => thought_rows.push((lines.len(), mi)),
                Some(Hit::Tool(mi)) => tool_rows.push((lines.len(), mi)),
                None => {}
            }
            lines.push(cl.line);
        }
    }

    // Spinner line while waiting for the first token of a turn.
    if app.waiting_first_token {
        let frame_idx = app.spinner_frame() % SPINNER.len();
        let secs = app.wait_started.map_or(0, |t| t.elapsed().as_secs());
        lines.push(Line::from(vec![
            Span::raw(PAD),
            Span::styled(
                SPINNER[frame_idx],
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" thinking… {secs}s"),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }

    let height = area.height as usize;
    let term_w = area.width.max(1); // terminal width for wrapping calculations

    // --- Pre-compute visual (wrapped) row positions for each logical line ---
    // SINGLE SOURCE OF TRUTH: `wrapped_height` delegates to ratatui's own
    // `Paragraph::line_count`, i.e. the exact `WordWrapper` the final render
    // uses. So `visual_pos` / `total_visual` can never drift from the drawn
    // layout — the class of bug where the tail scrolls out of view is gone.
    //
    // `visual_pos[i]` is the 0-based visual row where logical line `i` starts;
    // `total_visual` is the full rendered height in visual rows.
    let mut visual_pos: Vec<usize> = Vec::with_capacity(lines.len());
    let mut heights: Vec<usize> = Vec::with_capacity(lines.len());
    let mut total_visual = 0usize;
    for line in &lines {
        visual_pos.push(total_visual);
        let h = wrapped_height(line, term_w);
        heights.push(h);
        total_visual += h;
    }

    // Store for scroll_up() / scroll_down() clamping (via Cell interior mutability).
    app.total_rows.set(total_visual);
    app.view_height.set(height);
    // Re-clamp the manual scroll against THIS frame's totals. A width change
    // (resize) alters wrapping and thus `total_visual`, so a `scroll_back` that
    // was valid last frame may now exceed the max — clamp it here rather than
    // trusting the stale value carried by scroll_up()/scroll_down().
    app.clamp_scroll();

    // Auto-scroll: when following new content, show the bottom `height` rows.
    // `scroll_back` is in visual-row units, keeping everything in one coordinate
    // space. `visual_scroll` stays `usize` end-to-end — no u16 truncation.
    let auto_scroll = total_visual.saturating_sub(height);
    let visual_scroll = if app.is_scrolled_back() {
        auto_scroll.saturating_sub(app.scroll_back.get())
    } else {
        auto_scroll
    };

    // --- Hitbox computation ---
    // Compute each thought-row's screen-y directly from its pre-computed visual
    // position minus the scroll offset.
    let mut hitboxes = app.thought_hitboxes.borrow_mut();
    hitboxes.clear();
    for (flat_idx, msg_idx) in &thought_rows {
        let screen_y = area.y as isize + visual_pos[*flat_idx] as isize - visual_scroll as isize;
        if screen_y >= 0 && (screen_y as u16) < area.y + area.height {
            hitboxes.push((screen_y as u16, *msg_idx));
        }
    }
    drop(hitboxes);

    // --- Tool hitbox computation ---
    let mut tool_hitboxes = app.tool_hitboxes.borrow_mut();
    tool_hitboxes.clear();
    for (flat_idx, msg_idx) in &tool_rows {
        let screen_y = area.y as isize + visual_pos[*flat_idx] as isize - visual_scroll as isize;
        if screen_y >= 0 && (screen_y as u16) < area.y + area.height {
            tool_hitboxes.push((screen_y as u16, *msg_idx));
        }
    }
    drop(tool_hitboxes);

    // --- Record plaintext per screen row (for drag-select copy) + apply selection highlight ---
    let mut row_text = app.row_text.borrow_mut();
    row_text.clear();
    let sel = app.selection_rows();
    for (flat_idx, line) in lines.iter_mut().enumerate() {
        let screen_y = area.y as isize + visual_pos[flat_idx] as isize - visual_scroll as isize;
        if screen_y < 0 || (screen_y as u16) >= area.y + area.height {
            continue;
        }
        let screen_y = screen_y as u16;
        // Only the first screen row of a wrapped line gets the plaintext;
        // subsequent continuation rows are visual overflow.
        row_text.insert(screen_y, line_plaintext(line));
        if let Some((top, bottom)) = sel
            && screen_y >= top
            && screen_y <= bottom
        {
            for span in line.spans.iter_mut() {
                span.style = span.style.add_modifier(Modifier::REVERSED);
            }
        }
    }
    drop(row_text);

    log::debug!(
        "transcript: {n_lines} logical lines, total_visual={total_visual} view_h={height} \
         v_scroll={visual_scroll} scroll_back={sb} term_w={term_w} thought_hits={th} tool_hits={tl}",
        n_lines = lines.len(),
        sb = app.scroll_back.get(),
        th = app.thought_hitboxes.borrow().len(),
        tl = app.tool_hitboxes.borrow().len(),
    );

    // --- Render ---
    // We DON'T hand the whole transcript to `Paragraph::scroll`, because that
    // offset is a `u16` and would overflow once a long conversation wraps past
    // 65535 rows. Instead, drop every logical line fully above the viewport and
    // scroll only within the first partially-visible line. That inner offset is
    // always < one logical line's wrapped height, so it comfortably fits a u16.
    let mut first_visible = 0usize;
    while first_visible < lines.len()
        && visual_pos[first_visible] + heights[first_visible] <= visual_scroll
    {
        first_visible += 1;
    }
    let inner_offset = visual_scroll.saturating_sub(
        visual_pos.get(first_visible).copied().unwrap_or(visual_scroll),
    );
    let visible_lines: Vec<Line> = lines.split_off(first_visible.min(lines.len()));

    let paragraph = Paragraph::new(visible_lines)
        .wrap(Wrap { trim: false })
        .scroll((inner_offset as u16, 0));
    frame.render_widget(paragraph, area);

    // --- Layout invariant self-check (cheap; catches wrap drift immediately) ---
    debug_assert!(
        visual_scroll + height >= total_visual || app.is_scrolled_back(),
        "transcript not pinned to bottom: v_scroll={visual_scroll} + h={height} < total={total_visual}"
    );
}
