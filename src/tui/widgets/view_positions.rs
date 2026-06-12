//! Positions view — native ratatui table with row cursor.
//!
//! Replaced the ansi-bridge version (which couldn't expose row identity for
//! cursor highlighting) with a direct table render. Enter on a row opens the
//! card-style detail overlay.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};

use crate::app::state::{App, PositionFilter};
use crate::commands::chart::fmt_price;
use crate::types::positions::{Direction, Position, RiskStatus};

pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    let all_positions: &[Position] = match &app.positions {
        None => {
            let (msg, style) =
                super::loading_or_error_line(app, "positions", "  loading positions…");
            let para = ratatui::widgets::Paragraph::new(Line::from(Span::styled(msg, style)))
                .block(Block::default().title(" Positions ").borders(Borders::NONE));
            frame.render_widget(para, area);
            return;
        }
        Some(p) => &p.positions,
    };

    // Apply search filter — collect filtered indices then resolve to rows.
    let filtered_indices = app.filtered_positions();
    let positions: Vec<&Position> = filtered_indices
        .iter()
        .filter_map(|&i| all_positions.get(i))
        .collect();

    let filter_tag = match app.position_filter {
        PositionFilter::Both => "both",
        PositionFilter::Futures => "futures",
        PositionFilter::Spot => "spot",
    };
    // Show search status in title when a query is active.
    let search_tag = app
        .search
        .as_ref()
        .filter(|s| s.is_active())
        .map(|s| format!("  ·  /{} ({} shown)", s.query, positions.len()))
        .unwrap_or_default();
    let title = format!(
        " Positions [{}]  ·  {} open  ·  ↑↓ select · Enter detail · f/s/b filter{}",
        filter_tag,
        all_positions.len(),
        search_tag,
    );

    if positions.is_empty() {
        let msg = if app
            .search
            .as_ref()
            .is_some_and(crate::app::state::SearchState::is_active)
        {
            "  (no matches)"
        } else {
            "  (no open positions)"
        };
        let para = ratatui::widgets::Paragraph::new(Line::from(Span::styled(
            msg,
            Style::default().fg(Color::DarkGray),
        )))
        .block(Block::default().title(title).borders(Borders::NONE));
        frame.render_widget(para, area);
        return;
    }

    // Column header line.
    let header_line = Line::from(Span::styled(
        format!(
            "    {:>4}  {:<6}  {:<5}  {:>10}  {:>10}  {:>7}  {:>6}  {:<10}",
            "ID", "Asset", "Dir", "Entry", "Last", "PnL%", "RR", "Risk"
        ),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM | Modifier::BOLD),
    ));

    let rows: Vec<ListItem> = positions
        .iter()
        .map(|p| ListItem::new(position_row_line(p)))
        .collect();

    // [section title row, column header row, list rows rest]
    let chunks = ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([
            ratatui::layout::Constraint::Length(1),
            ratatui::layout::Constraint::Length(1),
            ratatui::layout::Constraint::Min(0),
        ])
        .split(area);

    let title_para = ratatui::widgets::Paragraph::new(Line::from(Span::styled(
        title,
        Style::default().add_modifier(Modifier::BOLD),
    )));
    frame.render_widget(title_para, chunks[0]);

    let header_para = ratatui::widgets::Paragraph::new(header_line);
    frame.render_widget(header_para, chunks[1]);

    let list = List::new(rows)
        .highlight_style(
            Style::default()
                .bg(Color::Indexed(236))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(list, chunks[2], &mut app.positions_cursor);
}

fn position_row_line(p: &Position) -> Line<'static> {
    let entry = p.entry_price.map_or_else(|| "—".into(), fmt_price);
    let last = p.current_price.map_or_else(|| "—".into(), fmt_price);
    let (pnl_str, pnl_color) = match p.unrealized_pnl_pct {
        Some(v) if v > 0.0 => (format!("{v:+.2}%"), Color::Green),
        Some(v) if v < 0.0 => (format!("{v:+.2}%"), Color::Red),
        Some(v) => (format!("{v:+.2}%"), Color::Reset),
        None => ("—".to_string(), Color::DarkGray),
    };
    let rr = p
        .current_rr
        .map_or_else(|| "—".into(), |v| format!("{v:.2}"));
    let dir_color = if p.direction == Direction::Long {
        Color::Green
    } else {
        Color::Red
    };
    let (risk_str, risk_color) = match &p.risk_status {
        Some(RiskStatus::AtRisk) => ("at-risk".to_string(), Color::Yellow),
        Some(RiskStatus::StoppedOut) => ("stopped".to_string(), Color::Red),
        Some(RiskStatus::Healthy) => ("healthy".to_string(), Color::Green),
        Some(RiskStatus::Unknown) => ("unknown".to_string(), Color::DarkGray),
        None => ("—".to_string(), Color::DarkGray),
    };
    Line::from(vec![
        Span::raw(format!("  {:>4}  ", p.id)),
        Span::raw(format!("{:<6}  ", p.asset)),
        Span::styled(
            format!("{:<5}  ", p.direction),
            Style::default().fg(dir_color),
        ),
        Span::raw(format!("{entry:>10}  {last:>10}  ")),
        Span::styled(format!("{pnl_str:>7}  "), Style::default().fg(pnl_color)),
        Span::raw(format!("{rr:>6}  ")),
        Span::styled(format!("{risk_str:<10}"), Style::default().fg(risk_color)),
    ])
}
