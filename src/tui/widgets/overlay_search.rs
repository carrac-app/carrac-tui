//! Search prompt overlay — a slim bordered popup that renders near the bottom
//! of the screen when `app.search` is Some and the prompt is open.
//!
//! Layout: 3 rows tall, full terminal width, positioned 4 rows from the bottom
//! of the main area (above the statusline / asset bar).
//!
//! Appearance (Find mode):
//!   ┌ Find ──────────────────────────────────────────────┐
//!   │ /query_                                             │
//!   └─────────────────────────────────────────────────────┘
//!
//! Appearance (Filter mode):
//!   ┌ Filter ─────────────────────────────────────────────┐
//!   │ /query_                                              │
//!   └──────────────────────────────────────────────────────┘
//!
//! The match counter is appended to the title line when matches exist.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::state::{App, SearchMode};

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let Some(search) = &app.search else {
        return;
    };
    if !search.is_open() {
        return;
    }

    // Popup: full width, 3 rows tall, placed 4 rows from the bottom of `area`.
    let popup_h: u16 = 3;
    let popup_w = area.width.clamp(30, 80);
    let x = area.x + (area.width.saturating_sub(popup_w)) / 2;
    let y = area.y + area.height.saturating_sub(popup_h + 1);
    let popup = Rect {
        x,
        y,
        width: popup_w,
        height: popup_h,
    };

    frame.render_widget(Clear, popup);

    // Title: mode label + optional match counter.
    let (mode_label, title_color) = match search.mode {
        SearchMode::Find => ("Find", Color::Cyan),
        SearchMode::Filter => ("Filter", Color::Green),
    };

    let match_info = if !search.query.is_empty() {
        match search.mode {
            SearchMode::Find => {
                if search.matches.is_empty() {
                    " · 0 matches".to_string()
                } else {
                    format!(" · {}/{}", search.current_match + 1, search.matches.len())
                }
            }
            SearchMode::Filter => String::new(),
        }
    } else {
        String::new()
    };

    let title = format!(" {} {} ", mode_label, match_info);

    let block = Block::default()
        .title(Line::from(Span::styled(
            title,
            Style::default()
                .fg(title_color)
                .add_modifier(Modifier::BOLD),
        )))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(title_color));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    // Input line: "/query▏"
    let cursor = Span::styled(
        "▏",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::SLOW_BLINK),
    );
    let input_line = Line::from(vec![
        Span::styled(
            "/",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(search.query.clone()),
        cursor,
    ]);

    let para = Paragraph::new(input_line);
    frame.render_widget(para, inner);
}
