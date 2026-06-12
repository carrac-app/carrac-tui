//! Plays view — table of active plays with progress + row cursor.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Paragraph};

use crate::app::state::App;
use crate::types::plays::Play;

pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    let all_plays: &[Play] = match &app.plays {
        None => {
            let (msg, style) = super::loading_or_error_line(app, "plays", "  loading plays…");
            let para = Paragraph::new(Line::from(Span::styled(msg, style)));
            frame.render_widget(para, area);
            return;
        }
        Some(p) => &p.plays,
    };

    // Apply search filter.
    let filtered_indices = app.filtered_plays();
    let plays: Vec<&Play> = filtered_indices
        .iter()
        .filter_map(|&i| all_plays.get(i))
        .collect();

    let search_tag = app
        .search
        .as_ref()
        .filter(|s| s.is_active())
        .map(|s| format!("  ·  /{} ({} shown)", s.query, plays.len()))
        .unwrap_or_default();
    let title = format!(
        " Plays — {} active  ·  ↑↓ select · Enter detail{}",
        all_plays.len(),
        search_tag,
    );

    if plays.is_empty() {
        let msg = if app
            .search
            .as_ref()
            .is_some_and(crate::app::state::SearchState::is_active)
        {
            "  (no matches)"
        } else {
            "  (no active plays)"
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
            "    {:>4}  {:<6}  {:<5}  {:<6}  {:<5}  {}",
            "ID", "Asset", "Dir", "Status", "Prog", "Title"
        ),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM | Modifier::BOLD),
    ));

    let rows: Vec<ListItem> = plays.iter().map(|p| play_row_item(p)).collect();

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
    frame.render_stateful_widget(list, chunks[2], &mut app.plays_cursor);
}

fn play_row_item(p: &Play) -> ListItem<'static> {
    let dir = p.direction.clone().unwrap_or_else(|| "—".into());
    let dir_color = super::direction_color(&dir);
    let asset = p.primary_asset.clone().unwrap_or_else(|| "—".into());
    let status = p.status.clone().unwrap_or_else(|| "—".into());
    let prog = p.progress();
    let (met, total) = p.condition_counts();
    let prog_color = if total > 0 && met == total {
        Color::Green
    } else if met > 0 {
        Color::Yellow
    } else {
        Color::Reset
    };
    let invalidated = p.invalidation.as_ref().is_some_and(|i| i.met);
    let title_style = if invalidated {
        Style::default().fg(Color::Red).add_modifier(Modifier::DIM)
    } else {
        Style::default()
    };

    ListItem::new(Line::from(vec![
        Span::raw(format!("  {:>4}  ", p.id)),
        Span::raw(format!("{asset:<6}  ")),
        Span::styled(format!("{dir:<5}  "), Style::default().fg(dir_color)),
        Span::raw(format!("{status:<6}  ")),
        Span::styled(format!("{prog:<5}  "), Style::default().fg(prog_color)),
        Span::styled(truncate(&p.title, 60), title_style),
    ]))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render_to_text(app: &mut App, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render(f, area, app);
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                text.push_str(buf.cell((x, y)).map_or(" ", ratatui::buffer::Cell::symbol));
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn none_with_no_error_shows_loading() {
        let mut app = App::new();
        assert!(app.plays.is_none());
        let text = render_to_text(&mut app, 60, 4);
        assert!(text.contains("loading"), "expected loading text:\n{text}");
    }

    #[test]
    fn none_with_fetch_error_surfaces_error_not_loading() {
        // Regression: plays view used to render "loading…" forever when the fetch
        // failed. With an error recorded it must surface the error instead.
        let mut app = App::new();
        app.fetch_errors
            .insert("plays", "connection refused".to_string());
        let text = render_to_text(&mut app, 70, 4);
        assert!(
            text.contains("connection refused"),
            "expected fetch error surfaced:\n{text}"
        );
        assert!(
            !text.contains("loading"),
            "loading text must be replaced by the error:\n{text}"
        );
    }
}
