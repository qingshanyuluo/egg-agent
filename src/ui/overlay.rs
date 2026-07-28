//! Modal overlays drawn on top of the transcript: the `/` command menu, the
//! live model picker, and the interactive connect-provider wizard.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::SPINNER;
use crate::app::{App, ConnectWizard, FilePopup, ModelPicker, Overlay};

/// Max file rows shown in the `@`-completion popup (feature D).
const FILE_POPUP_ROWS: u16 = 8;

/// Centered rectangle `pct_w`% × up to `max_h` rows within `area`.
fn centered(area: Rect, pct_w: u16, max_h: u16) -> Rect {
    let w = (area.width * pct_w / 100).clamp(20, area.width.saturating_sub(2));
    let h = max_h.min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect { x, y, width: w, height: h }
}

pub(super) fn draw_overlay(frame: &mut Frame, app: &App, overlay: &Overlay, area: Rect) {
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
            draw_model_ready(frame, app, all, filter, *selected, popup, block);
        }
    }
}
/// The loaded-model list body of the picker (extracted so `draw_model_picker`
/// stays a thin dispatch over `ModelPicker` states).
fn draw_model_ready(
    frame: &mut Frame,
    app: &App,
    all: &[String],
    filter: &str,
    selected: usize,
    popup: Rect,
    block: Block,
) {
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
        Span::raw(filter.to_string()),
    ]));

    for (vis_row, (i, model)) in matches
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .enumerate()
    {
        let is_sel = i == selected;
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

/// The `@`-file completion popup (feature D). Floats just above `input_area`,
/// showing the fuzzy-ranked matches with the selected row highlighted. Styled
/// like [`draw_command_menu`] for visual consistency.
pub(super) fn draw_file_popup(frame: &mut Frame, popup: &FilePopup, input_area: Rect) {
    // Height: one row per match (capped) + top/bottom border. Never taller
    // than the space above the input box.
    let rows = (popup.matches.len() as u16)
        .clamp(1, FILE_POPUP_ROWS)
        .min(input_area.y.saturating_sub(1));
    if rows == 0 {
        return; // no vertical room above the input — skip rather than overlap.
    }
    let height = rows + 2;
    let width = input_area.width;
    let area = Rect {
        x: input_area.x,
        y: input_area.y.saturating_sub(height),
        width,
        height,
    };
    frame.render_widget(Clear, area);

    // Scroll so the selected row stays visible within the capped window.
    let visible = rows as usize;
    let start = popup.selected.saturating_sub(visible.saturating_sub(1));

    let mut lines: Vec<Line> = Vec::new();
    if popup.matches.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no matching file",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for (i, path) in popup.matches.iter().enumerate().skip(start).take(visible) {
            let is_sel = i == popup.selected;
            let marker = if is_sel { "❯ " } else { "  " };
            let style = if is_sel {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(format!("{marker}{path}"), style)));
        }
    }

    let title = format!(" @{} ", popup.query);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(title)
        .title_bottom(" ↑↓ · Enter/Tab · Esc ");
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

