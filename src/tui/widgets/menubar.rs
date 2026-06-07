//! Top menubar — view tabs only, with `?:help` as the visual cue.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::state::{App, View};

const VIEWS: [(usize, View, &str); 7] = [
    (1, View::Chart, "chart"),
    (2, View::Positions, "pos"),
    (3, View::Plays, "plays"),
    (4, View::Alerts, "alerts"),
    (5, View::Report, "report"),
    (6, View::News, "news"),
    (7, View::Details, "details"),
];

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans: Vec<Span> = Vec::new();

    for (key, v, label) in VIEWS.iter() {
        let active = app.view == *v;
        let style = if active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        spans.push(Span::styled(format!(" {}·{} ", key, label), style));
    }

    // Pad to the right so the help cue sits at end-of-line.
    let used_width: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let help_label = " ?:help ";
    let pad = (area.width as usize)
        .saturating_sub(used_width)
        .saturating_sub(help_label.chars().count());
    if pad > 0 {
        spans.push(Span::raw(" ".repeat(pad)));
    }
    spans.push(Span::styled(
        help_label,
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));

    let para = Paragraph::new(Line::from(spans));
    frame.render_widget(para, area);
}
