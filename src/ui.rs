//! Borderless, whitespace-driven transcript UI.
//!
//! Style goals (mirroring mature terminal agents): no boxes, a colored gutter
//! bar per message, breathing room between turns, dim secondary text for tool
//! activity, and a single prompt line with a compact status line beneath it.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, ConnectWizard, ModelPicker, Overlay, Role};

/// Left margin so content doesn't hug the terminal edge.
const PAD: &str = " ";
/// Solid bar that heads each top-level message.
const BAR: &str = "▌";
/// Thin bar for nested tool lines.
const THIN_BAR: &str = "▎";
/// Braille spinner frames for the "waiting for first token" animation.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

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
    // The input box height grows with content. Compute visual (wrapped) row count
    // inside the bordered block, then add 2 for the border lines.
    let inner_w = frame.area().width.saturating_sub(2);
    let mut visual_rows = 0u16;
    for line in app.input.lines() {
        let w = UnicodeWidthStr::width(line).max(1) as u16;
        visual_rows += (w + inner_w - 1) / inner_w;
    }
    // str::lines() drops a trailing empty line, so if the user pressed
    // Alt+Enter at the end of the input we need to account for the new
    // blank line explicitly.
    if app.input.ends_with('\n') {
        visual_rows += 1;
    }
    let input_height = (visual_rows.max(1) + 2).min(12); // +2 borders, cap at 12

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),              // transcript
            Constraint::Length(1),           // spacer
            Constraint::Length(input_height), // input box
            Constraint::Length(1),           // status
        ])
        .split(frame.area());

    if app.show_splash && app.messages.is_empty() {
        app.splash_visible.set(true);
        draw_splash(frame, app, chunks[0]);
    } else {
        app.splash_visible.set(false);
        draw_transcript(frame, app, chunks[0]);
    }
    draw_prompt(frame, app, chunks[2]);
    draw_status(frame, app, chunks[3]);

    if let Some(overlay) = &app.overlay {
        draw_overlay(frame, app, overlay, frame.area());
    } else {
        // Place the cursor at the end of the last input line.
        frame.set_cursor_position(input_cursor_pos(app, chunks[2]));
    }

    // "copied" toast, top-right, above everything.
    if app.toast_active() {
        draw_toast(frame, frame.area());
    }
}

/// Centered rectangle `pct_w`% × up to `max_h` rows within `area`.
fn centered(area: Rect, pct_w: u16, max_h: u16) -> Rect {
    let w = (area.width * pct_w / 100).clamp(20, area.width.saturating_sub(2));
    let h = max_h.min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect { x, y, width: w, height: h }
}

fn draw_overlay(frame: &mut Frame, app: &App, overlay: &Overlay, area: Rect) {
    app.overlay_hitboxes.borrow_mut().clear();
    match overlay {
        Overlay::CommandMenu { filter, selected } => {
            draw_command_menu(frame, app, filter, *selected, area)
        }
        Overlay::ModelPicker(picker) => draw_model_picker(frame, app, picker, area),
        Overlay::ConnectWizard(wiz) => draw_connect_wizard(frame, wiz, area),
    }
}

fn draw_command_menu(frame: &mut Frame, app: &App, filter: &str, selected: usize, area: Rect) {
    let matches = app.filtered_commands(filter);
    let popup = centered(area, 60, (matches.len() as u16 + 3).max(4));
    frame.render_widget(Clear, popup);

    let mut lines: Vec<Line> = Vec::new();
    let inner_top = popup.y + 1; // account for top border
    for (i, cmd) in matches.iter().enumerate() {
        let selected_row = i == selected;
        let marker = if selected_row { "❯ " } else { "  " };
        let style = if selected_row {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{marker}/{}", cmd.name), style),
            Span::styled(
                format!("  {}", cmd.description),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        // Register clickable row (offset by border).
        app.overlay_hitboxes
            .borrow_mut()
            .push((inner_top + i as u16, i));
    }
    if matches.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no matching command",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let title = format!(" command  /{filter} ");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(title);
    frame.render_widget(Paragraph::new(lines).block(block), popup);
}

fn draw_model_picker(frame: &mut Frame, app: &App, picker: &ModelPicker, area: Rect) {
    let popup = centered(area, 70, 16);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" select model ");

    match picker {
        ModelPicker::Loading => {
            let frame_idx = app.spinner_frame() % SPINNER.len();
            let body = Line::from(vec![
                Span::raw(" "),
                Span::styled(SPINNER[frame_idx], Style::default().fg(Color::Cyan)),
                Span::styled(" fetching models…", Style::default().fg(Color::DarkGray)),
            ]);
            frame.render_widget(Paragraph::new(body).block(block), popup);
        }
        ModelPicker::Error(e) => {
            let body = vec![
                Line::from(Span::styled(
                    format!(" error: {e}"),
                    Style::default().fg(Color::Red),
                )),
                Line::from(Span::styled(
                    " Esc to close",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            frame.render_widget(Paragraph::new(body).block(block), popup);
        }
        ModelPicker::Ready { all, filter, selected } => {
            let matches = App::filtered_models(all, filter);
            // Rows available for the list (inside borders, minus the filter line).
            let visible = popup.height.saturating_sub(3) as usize;
            // Scroll so the selected item stays visible.
            let start = selected.saturating_sub(visible.saturating_sub(1));
            let inner_top = popup.y + 1;

            let mut lines: Vec<Line> = Vec::new();
            // Filter line at the top of the body.
            lines.push(Line::from(vec![
                Span::styled(" filter: ", Style::default().fg(Color::DarkGray)),
                Span::raw(filter.as_str()),
            ]));

            for (vis_row, (i, model)) in matches
                .iter()
                .enumerate()
                .skip(start)
                .take(visible)
                .enumerate()
            {
                let is_sel = i == *selected;
                let is_current = **model == app.model
                    || model.starts_with(&format!("{} (", app.model));
                let marker = if is_sel { "❯ " } else { "  " };
                let cur = if is_current { " *" } else { "" };
                let style = if is_sel {
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("{marker}{model}"), style),
                    Span::styled(cur, Style::default().fg(Color::Green)),
                ]));
                // +1 for the filter line offset within the body.
                app.overlay_hitboxes
                    .borrow_mut()
                    .push((inner_top + 1 + vis_row as u16, i));
            }

            let count_hint = format!(" {} models · ↑↓ Enter · Esc ", matches.len());
            let block = block.title_bottom(count_hint);
            frame.render_widget(Paragraph::new(lines).block(block), popup);
        }
    }
}

/// Render the interactive connect-provider wizard as a centered form.
fn draw_connect_wizard(frame: &mut Frame, wiz: &ConnectWizard, area: Rect) {
    let popup = centered(area, 70, 10);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" connect provider ");

    let hint = " Tab/Enter · Esc to cancel ";
    let block = block.title_bottom(hint);

    let label_style = |focused: bool| -> Style {
        if focused {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        }
    };

    let value_style = |focused: bool| -> Style {
        if focused {
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        }
    };

    let mut lines: Vec<Line> = Vec::new();

    // Provider name field.
    lines.push(Line::from(vec![
        Span::styled(" Name    ", label_style(wiz.field == 0)),
        Span::styled(
            if wiz.name.is_empty() && wiz.field != 0 {
                "(e.g. deepseek)"
            } else {
                &wiz.name
            },
            value_style(wiz.field == 0),
        ),
        Span::styled(
            if wiz.field == 0 { " ▌" } else { "" },
            Style::default().fg(Color::Cyan),
        ),
    ]));

    // API Key field (masked display).
    let masked_key = if wiz.api_key.is_empty() {
        if wiz.field == 1 {
            String::new()
        } else {
            "(sk-...)".to_string()
        }
    } else {
        crate::cli::mask(&wiz.api_key)
    };
    lines.push(Line::from(vec![
        Span::styled(" API Key ", label_style(wiz.field == 1)),
        Span::styled(&masked_key, value_style(wiz.field == 1)),
        Span::styled(
            if wiz.field == 1 { " ▌" } else { "" },
            Style::default().fg(Color::Cyan),
        ),
    ]));

    // Base URL field.
    let url_display = if wiz.base_url.is_empty() && wiz.field != 2 {
        "https://api.openai.com/v1"
    } else {
        &wiz.base_url
    };
    lines.push(Line::from(vec![
        Span::styled(" Base URL", label_style(wiz.field == 2)),
        Span::styled(url_display, value_style(wiz.field == 2)),
        Span::styled(
            if wiz.field == 2 { " ▌" } else { "" },
            Style::default().fg(Color::Cyan),
        ),
    ]));

    // Spacer.
    lines.push(Line::from(""));

    // Submit hint.
    let submit_ready = !wiz.name.trim().is_empty() && !wiz.api_key.trim().is_empty();
    if submit_ready {
        lines.push(Line::from(Span::styled(
            " ✓ Press Enter to save",
            Style::default().fg(Color::Green),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            " Fill in Name and API Key, then press Enter",
            Style::default().fg(Color::DarkGray),
        )));
    }

    // Default for base_url.
    if wiz.base_url.is_empty() || wiz.base_url == "https://api.openai.com/v1" {
        lines.push(Line::from(Span::styled(
            " (defaults to https://api.openai.com/v1)",
            Style::default().fg(Color::DarkGray),
        )));
    }

    frame.render_widget(Paragraph::new(lines).block(block), popup);
}


fn draw_transcript(frame: &mut Frame, app: &App, area: Rect) {
    // Flatten the transcript into styled lines, then show the tail that fits so
    // the newest content always stays in view (mature-TUI "stick to bottom").
    let mut lines: Vec<Line> = Vec::new();
    // Flat line index -> message index, for each clickable "thought" line.
    let mut thought_rows: Vec<(usize, usize)> = Vec::new();

    // Flat line index -> message index, for each clickable tool line.
    let mut tool_rows: Vec<(usize, usize)> = Vec::new();

    for (msg_idx, message) in app.messages.iter().enumerate() {
        let color = role_color(message.role);
        let dim = matches!(message.role, Role::ToolOutput | Role::System);

        // Reasoning block (assistant only): collapsed summary or expanded text.
        if !message.reasoning.is_empty() {
            if message.reasoning_collapsed {
                let secs = message.reasoning_secs.unwrap_or(0);
                thought_rows.push((lines.len(), msg_idx));
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(
                        format!("▸ thought for {secs}s"),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        "  (click to expand)",
                        Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                    ),
                ]));
            } else {
                // Expanded: a clickable header row, then the full reasoning.
                let secs = message.reasoning_secs;
                let header = match secs {
                    Some(s) => format!("▾ thought for {s}s"),
                    None => "▾ thinking".to_string(),
                };
                thought_rows.push((lines.len(), msg_idx));
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(header, Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        "  (click to collapse)",
                        Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                    ),
                ]));
                for text in message.reasoning.lines() {
                    if text.trim().is_empty() {
                        continue;
                    }
                    lines.push(Line::from(vec![
                        Span::raw("     "),
                        Span::styled(text.to_string(), Style::default().fg(Color::DarkGray)),
                    ]));
                }
                // Translation (from TranslatePlugin), shown below the original.
                if let Some(translation) = &message.translation {
                    if !translation.is_empty() {
                        lines.push(Line::from(vec![
                            Span::raw("     "),
                            Span::styled("⟡ ", Style::default().fg(Color::Yellow)),
                            Span::styled(translation.as_str(), Style::default().fg(Color::Yellow)),
                        ]));
                    }
                }
            }
        }

        // An assistant turn that has reasoning but no content yet (streaming
        // hasn't produced visible text) — nothing to render, skip.
        if message.content.is_empty() && message.role == Role::Assistant {
            continue;
        }

        let raw: Vec<&str> = if message.content.is_empty() {
            vec![""]
        } else {
            message.content.lines().collect()
        };
        // Trim leading / trailing blank lines (streaming artifacts), but keep
        // internal blank lines — they're paragraph breaks in markdown.
        let first = raw.iter().position(|l| !l.trim().is_empty()).unwrap_or(0);
        let last = raw.iter().rposition(|l| !l.trim().is_empty()).unwrap_or(0);
        let content_lines = if first <= last {
            &raw[first..=last]
        } else {
            &[]
        };

        for (i, text) in content_lines.iter().enumerate() {
            lines.push(match message.role {
                Role::Tool => {
                    if message.tool_collapsed {
                        // Collapsed: skip actual content, will add summary below.
                        continue;
                    } else {
                        // Expanded: render with thin bar. The first line doubles as
                        // the collapse hitbox — we record its position.
                        if i == 0 {
                            tool_rows.push((lines.len(), msg_idx));
                        }
                        tool_call_line(i, text, color)
                    }
                }
                Role::ToolOutput => {
                    if message.output_collapsed && message.full_content.is_some() {
                        // Collapsed with full content available: skip preview entirely,
                        // will render a one-line summary below.
                        continue;
                    } else {
                        // Either expanded (showing full content) or short output
                        // (no full_content): render normally.
                        tool_output_line(text)
                    }
                }
                _ => top_level_line(i, text, color, dim, message.role),
            });
        }

        // ---- Tool explanation (from BashExplainPlugin) ----
        if message.role == Role::Tool && !message.tool_collapsed {
            if let Some(explanation) = &message.explanation {
                if !explanation.is_empty() {
                    lines.push(Line::from(vec![
                        Span::raw("     "),
                        Span::styled("⟡ ", Style::default().fg(Color::Yellow)),
                        Span::styled(explanation.as_str(), Style::default().fg(Color::Yellow)),
                    ]));
                }
            }
        }

        // ---- Collapsible tool-call summary (Role::Tool, collapsed) ----
        if message.role == Role::Tool && message.tool_collapsed {
            let summary = message.content.lines().next().unwrap_or("");
            tool_rows.push((lines.len(), msg_idx));
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(
                    format!("▸ {summary}"),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    "  (click to expand)",
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                ),
            ]));
        }

        // ---- Collapsible tool-output (Role::ToolOutput) ----
        if message.role == Role::ToolOutput {
            if let Some(full) = &message.full_content {
                if message.output_collapsed {
                    // Collapsed: show a single summary line (no preview).
                    let line_count = full.lines().count();
                    tool_rows.push((lines.len(), msg_idx));
                    lines.push(Line::from(vec![
                        Span::raw("   "),
                        Span::styled(
                            format!("▸ output ({line_count} lines)"),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(
                            "  (click to expand)",
                            Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                        ),
                    ]));
                } else {
                    // Expanded: show collapse header + full output.
                    let line_count = full.lines().count();
                    tool_rows.push((lines.len(), msg_idx));
                    lines.push(Line::from(vec![
                        Span::raw("   "),
                        Span::styled(
                            format!("▾ output ({line_count} lines)"),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(
                            "  (click to collapse)",
                            Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                        ),
                    ]));
                    for text in full.lines() {
                        lines.push(Line::from(vec![
                            Span::raw("     "),
                            Span::styled(text.to_string(), Style::default().fg(Color::DarkGray)),
                        ]));
                    }
                }
            }
        }

        // Breathing room: no blank lines anywhere — dense output.
        // The ▌ bar on user messages provides the only visual turn separation.
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
    let term_w = area.width.max(1) as u16; // terminal width for wrapping calculations

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
    // position minus the scroll offset. No need to iterate over preceding lines
    // every frame; `visual_pos` has already done that work.
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
        if let Some((top, bottom)) = sel {
            if screen_y >= top && screen_y <= bottom {
                for span in line.spans.iter_mut() {
                    span.style = span.style.add_modifier(Modifier::REVERSED);
                }
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
    // With the single-source-of-truth wrapping above this must always hold; the
    // assert turns any future regression into an immediate, localized failure in
    // debug builds while staying silent (a warn log) in release.
    debug_assert!(
        visual_scroll + height >= total_visual || app.is_scrolled_back(),
        "transcript not pinned to bottom: v_scroll={visual_scroll} + h={height} < total={total_visual}"
    );
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

/// A small "✓ copied" badge in the top-right corner.
fn draw_toast(frame: &mut Frame, area: Rect) {
    let label = " ✓ copied ";
    let w = UnicodeWidthStr::width(label) as u16;
    if area.width < w + 1 {
        return;
    }
    let rect = Rect {
        x: area.x + area.width - w - 1,
        y: area.y,
        width: w,
        height: 1,
    };
    frame.render_widget(Clear, rect);
    let badge = Paragraph::new(Line::from(Span::styled(
        label,
        Style::default()
            .fg(Color::Black)
            .bg(Color::Green)
            .add_modifier(Modifier::BOLD),
    )));
    frame.render_widget(badge, rect);
}

/// A top-level user/assistant/system line: colored bar on the first line
/// (only for user messages), aligned indent on continuation lines.
fn top_level_line<'a>(index: usize, text: &'a str, color: Color, dim: bool, role: Role) -> Line<'a> {
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
                Span::styled(text, content_style),
            ])
        } else {
            Line::from(vec![
                Span::raw("  "),
                Span::styled(text, content_style),
            ])
        }
    } else {
        // Match first-line indent for this role: ▌ + space = 3 for user, 2 otherwise.
        let indent = if role == Role::User { "   " } else { "  " };
        Line::from(vec![Span::raw(indent), Span::styled(text, content_style)])
    }
}

/// A tool invocation: `  ▎ name  args`, name emphasized, args dim.
fn tool_call_line<'a>(index: usize, text: &'a str, color: Color) -> Line<'a> {
    if index > 0 {
        return Line::from(vec![Span::raw("     "), Span::styled(text, Style::default().fg(Color::DarkGray))]);
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
        Span::styled(name, Style::default().fg(color).add_modifier(Modifier::BOLD)),
    ];
    if !rest.is_empty() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(rest, Style::default().fg(Color::DarkGray)));
    }
    Line::from(spans)
}

/// Dim, indented tool output.
fn tool_output_line(text: &str) -> Line<'_> {
    Line::from(vec![
        Span::raw("     "),
        Span::styled(text, Style::default().fg(Color::DarkGray)),
    ])
}

fn draw_prompt(frame: &mut Frame, app: &App, area: Rect) {
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
            "Type a message…  (Alt+Enter / Shift+Enter newline · ↑↓ history)",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ))]
    } else {
        let mut raw_lines: Vec<&str> = app.input.lines().collect();
        // str::lines() drops a trailing empty line; add it back so the
        // input box shows a blank line after the user presses Alt+Enter.
        if app.input.ends_with('\n') {
            raw_lines.push("");
        }
        raw_lines
            .into_iter()
            .map(|l| Line::from(Span::raw(l.to_string())))
            .collect()
    };

    frame.render_widget(
        Paragraph::new(text).block(block).wrap(Wrap { trim: false }),
        area,
    );
}

/// Compute the (x, y) cursor position inside the bordered input box, placing it
/// at the end of the last visual line of input text (accounting for wrapping).
fn input_cursor_pos(app: &App, area: Rect) -> (u16, u16) {
    let block = Block::default().borders(Borders::ALL);
    let inner = block.inner(area);
    let inner_w = inner.width.max(1);

    let text: &str = app.input.as_str();
    if text.is_empty() {
        return (inner.x, inner.y);
    }

    // Split by \n but preserve the trailing empty line when input ends with \n.
    let mut logical: Vec<&str> = text.lines().collect();
    if text.ends_with('\n') {
        logical.push("");
    }

    // The last logical line determines the cursor column.
    let last = logical.pop().unwrap(); // safe: at least one element
    let col = UnicodeWidthStr::width(last) as u16;

    // All preceding logical lines contribute full wrapped rows.
    let mut visual_row = 0u16;
    for &line in &logical {
        let w = UnicodeWidthStr::width(line).max(1) as u16;
        visual_row += (w + inner_w - 1) / inner_w;
    }
    // The last line itself may have wrapped; cursor sits at end of its last row.
    visual_row += col / inner_w;
    let cursor_col = col % inner_w;

    (inner.x + cursor_col, inner.y + visual_row)
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
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

fn role_color(role: Role) -> Color {
    match role {
        Role::User => Color::Cyan,
        Role::Assistant => Color::Green,
        Role::Tool => Color::Yellow,
        Role::ToolOutput => Color::DarkGray,
        Role::System => Color::DarkGray,
    }
}

/// Centered egg ASCII art with a slow pulse animation. Shown at startup until
/// the user sends the first message.
fn draw_splash(frame: &mut Frame, app: &App, area: Rect) {
    let elapsed_ms = app.booted_at.elapsed().as_millis();
    let phase = (elapsed_ms as f64 / 2000.0 * std::f64::consts::TAU).sin();
    let lum = ((phase + 1.0) * 100.0) as u8 + 55; // 55..255
    let color = Color::Rgb(lum, lum, lum.saturating_add(20));

    let mut lines: Vec<Line> = Vec::new();
    let art_height = EGG_ART.len() as u16 + 2; // art + tagline + blank
    let v_pad = area.height.saturating_sub(art_height) / 2;
    for _ in 0..v_pad {
        lines.push(Line::from(""));
    }

    // EGG_ART rows are 20 columns wide.
    let h_pad = area.width.saturating_sub(20) / 2;
    let pad = " ".repeat(h_pad as usize);
    for row in &EGG_ART {
        lines.push(Line::from(vec![
            Span::raw(&pad),
            Span::styled(*row, Style::default().fg(color).add_modifier(Modifier::BOLD)),
        ]));
    }

    lines.push(Line::from(""));
    let tag = "egg-agent — ask me to code, search, or run commands";
    let tag_w = UnicodeWidthStr::width(tag) as u16;
    let tag_pad = area.width.saturating_sub(tag_w) / 2;
    lines.push(Line::from(vec![
        Span::raw(" ".repeat(tag_pad as usize)),
        Span::styled(tag, Style::default().fg(Color::DarkGray)),
    ]));

    frame.render_widget(Paragraph::new(lines), area);
}

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
}
