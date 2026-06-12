//! Alerts view — triggered alerts list with row cursor.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Paragraph};

use crate::app::state::App;
use crate::types::alerts::Alert;

pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    let all_alerts: &[Alert] = match &app.alerts {
        None => {
            let (msg, style) = super::loading_or_error_line(app, "alerts", "  loading alerts…");
            let para = Paragraph::new(Line::from(Span::styled(msg, style)));
            frame.render_widget(para, area);
            return;
        }
        Some(a) => &a.alerts,
    };

    // Apply search filter.
    let filtered_indices = app.filtered_alerts();
    let alerts: Vec<&Alert> = filtered_indices
        .iter()
        .filter_map(|&i| all_alerts.get(i))
        .collect();

    let search_tag = app
        .search
        .as_ref()
        .filter(|s| s.is_active())
        .map(|s| format!("  ·  /{} ({} shown)", s.query, alerts.len()))
        .unwrap_or_default();
    let title = format!(
        " Alerts — {} triggered  ·  ↑↓ select · Enter detail · [A] new alert{}",
        all_alerts.len(),
        search_tag,
    );

    if alerts.is_empty() {
        let msg = if app
            .search
            .as_ref()
            .is_some_and(crate::app::state::SearchState::is_active)
        {
            "  (no matches)"
        } else {
            "  (no triggered alerts)"
        };
        let para = Paragraph::new(vec![
            Line::from(Span::styled(
                title,
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(msg, Style::default().fg(Color::DarkGray))),
        ]);
        frame.render_widget(para, area);
        return;
    }

    let header_line = Line::from(Span::styled(
        format!(
            "    {:<7}  {:<7}  {:<12}  {:>10}  {}",
            "Symbol", "Type", "Triggered", "Price", "Note"
        ),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM | Modifier::BOLD),
    ));

    let rows: Vec<ListItem> = alerts.iter().map(|a| alert_row_item(a)).collect();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        ))),
        chunks[0],
    );
    frame.render_widget(Paragraph::new(header_line), chunks[1]);

    let list = List::new(rows)
        .highlight_style(
            Style::default()
                .bg(Color::Indexed(236))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(list, chunks[2], &mut app.alerts_cursor);
}

fn alert_row_item(a: &Alert) -> ListItem<'static> {
    let sym = a.symbol.clone().unwrap_or_else(|| "—".into());
    let kind = a.alert_type.clone().unwrap_or_else(|| "—".into());
    let when = short_ts(a.triggered_at.as_deref());
    let price = a
        .triggered_price
        .or(a.price)
        .map_or_else(|| "         —".into(), |v| format!("{v:>10.4}"));
    let note = a.note.clone().unwrap_or_default();

    ListItem::new(Line::from(vec![
        Span::raw(format!("  {sym:<7}  ")),
        Span::raw(format!("{kind:<7}  ")),
        Span::styled(format!("{when:<12}  "), Style::default().fg(Color::Yellow)),
        Span::raw(format!("{price}  ")),
        Span::styled(truncate(&note, 50), Style::default().fg(Color::DarkGray)),
    ]))
}

fn short_ts(s: Option<&str>) -> String {
    match s {
        None => "—".to_string(),
        Some(t) => t.chars().take(16).collect(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
    }
}
