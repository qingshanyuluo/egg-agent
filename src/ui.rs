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

use crate::app::{App, ModelPicker, Overlay, Role};

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
                let is_current = **model == app.model;
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


fn draw_transcript(frame: &mut Frame, app: &App, area: Rect) {
    // Flatten the transcript into styled lines, then show the tail that fits so
    // the newest content always stays in view (mature-TUI "stick to bottom").
    let mut lines: Vec<Line> = Vec::new();
    // Flat line index -> message index, for each clickable "thought" line.
    let mut thought_rows: Vec<(usize, usize)> = Vec::new();

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
                Role::Tool => tool_call_line(i, text, color),
                Role::ToolOutput => tool_output_line(text),
                _ => top_level_line(i, text, color, dim, message.role),
            });
        }
        // Tool explanation (from BashExplainPlugin), shown below the tool call.
        if message.role == Role::Tool {
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
    // Each logical Line may wrap to ceil(display_width / term_w) screen rows.
    // `visual_pos[i]` is the 0-based visual row where logical line `i` starts;
    // `total_visual` is the full rendered height in visual rows.
    let mut visual_pos: Vec<usize> = Vec::with_capacity(lines.len());
    let mut total_visual = 0usize;
    for line in &lines {
        visual_pos.push(total_visual);
        let w = line_display_width(line).max(1) as u16;
        total_visual += ((w + term_w - 1) / term_w) as usize;
    }

    // Store for scroll_up() / scroll_down() clamping (via Cell interior mutability).
    app.total_rows.set(total_visual);
    app.view_height.set(height);

    // Auto-scroll: when following new content, show the bottom `height` rows.
    // `scroll_back` is in visual-row units, keeping everything in one coordinate space.
    let auto_scroll = total_visual.saturating_sub(height);
    let visual_scroll = if app.is_scrolled_back() {
        auto_scroll.saturating_sub(app.scroll_back)
    } else {
        auto_scroll
    };

    // Log every message that has reasoning (to diagnose missing hitboxes).
    for (i, msg) in app.messages.iter().enumerate() {
        if !msg.reasoning.is_empty() {
            log::debug!(
                "render: msg[{i}] role={:?} reasoning_len={} collapsed={} content_len={}",
                msg.role,
                msg.reasoning.len(),
                msg.reasoning_collapsed,
                msg.content.len(),
            );
        }
    }

    // --- Hitbox computation ---
    // Compute each thought-row's screen-y directly from its pre-computed visual
    // position minus the scroll offset. No need to iterate over preceding lines
    // every frame; `visual_pos` has already done that work.
    let thought_count = thought_rows.len();
    let mut hitboxes = app.thought_hitboxes.borrow_mut();
    hitboxes.clear();

    for (flat_idx, msg_idx) in &thought_rows {
        let screen_y =
            area.y as isize + visual_pos[*flat_idx] as isize - visual_scroll as isize;
        if screen_y >= 0 && (screen_y as u16) < area.y + area.height {
            hitboxes.push((screen_y as u16, *msg_idx));
        }
    }
    log::debug!(
        "hitboxes: {thought_count} thought_rows -> {} visible (total_visual={total_visual} height={height} term_w={term_w}) hitboxes={:?}",
        hitboxes.len(),
        hitboxes,
    );
    drop(hitboxes);

    // --- Record plaintext per screen row (for drag-select copy) + apply selection highlight ---
    let mut row_text = app.row_text.borrow_mut();
    row_text.clear();
    let sel = app.selection_rows();

    for (flat_idx, line) in lines.iter_mut().enumerate() {
        let screen_y =
            area.y as isize + visual_pos[flat_idx] as isize - visual_scroll as isize;
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

    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((visual_scroll as u16, 0));
    frame.render_widget(paragraph, area);
}

/// Display width of a line in terminal columns (handles CJK via unicode-width).
fn line_display_width(line: &Line) -> usize {
    line.spans
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum()
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
        app.input
            .lines()
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

    // The last logical line and its display width.
    let last_line: &str = text.rfind('\n').map_or(text, |p| &text[p + 1..]);
    let col = UnicodeWidthStr::width(last_line) as u16;

    // Count visual rows consumed by all logical lines before the last.
    let last_line_idx = text.lines().count().saturating_sub(1);
    let mut visual_row = 0u16;
    for (i, line) in text.lines().enumerate() {
        if i == last_line_idx {
            break;
        }
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
        format!(" · scrolled ↑{}", app.scroll_back)
    } else {
        String::new()
    };
    let text = format!(
        " {} · {} · {}{} · ↑↓ history · Enter send · Ctrl+C quit",
        app.provider, app.model, state, scroll
    );
    let status = Paragraph::new(Line::from(Span::styled(
        text,
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(status, area);
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
