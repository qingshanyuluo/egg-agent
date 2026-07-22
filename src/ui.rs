use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};

use crate::app::{App, Role};

pub fn draw(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(3)])
        .split(frame.area());

    let items: Vec<ListItem> = app
        .messages
        .iter()
        .map(|message| {
            let (label, color) = match message.role {
                Role::User => ("You", Color::Cyan),
                Role::Assistant => ("Egg", Color::Green),
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{label}: "),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(message.content.clone()),
            ]))
        })
        .collect();

    let messages = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" egg-agent "));
    frame.render_widget(messages, chunks[0]);

    let input = Paragraph::new(app.input.as_str())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" input (Enter to send, Esc to quit) "),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(input, chunks[1]);

    frame.set_cursor_position((
        chunks[1].x + app.input.chars().count() as u16 + 1,
        chunks[1].y + 1,
    ));
}
