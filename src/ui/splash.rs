//! Startup splash (pulsing egg + tagline) and the transient "✓ copied" toast.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::EGG_ART;
use crate::app::App;

/// A small "✓ copied" badge in the top-right corner.
pub(super) fn draw_toast(frame: &mut Frame, area: Rect) {
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

/// Centered egg ASCII art with a slow pulse animation. Shown at startup until
/// the user sends the first message.
pub(super) fn draw_splash(frame: &mut Frame, app: &App, area: Rect) {
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
