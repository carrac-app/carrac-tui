//! Asset selector overlay — popup list with type-to-filter.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::app::state::App;
use crate::tui::layout::centered_rect;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let Some(sel) = &app.asset_selector else {
        return;
    };

    // Favourite matches (watchlist filter).
    let fav_matches = app.asset_selector_matches();
    let fav_count = fav_matches.len();

    // Combined list: favourites first, then server search results (deduped).
    let combined = app.asset_selector_combined();
    let n = combined.len();

    let has_search = !app.symbol_search_results.is_empty();
    // Extra lines: separator header when the search section is present.
    let search_header_lines: u16 = u16::from(has_search && fav_count < n);

    // Popup sizing: a bit wider when search results are present (symbol + name).
    let width: u16 = if has_search { 44 } else { 32 };
    let height: u16 = (n as u16 + 4 + search_header_lines).clamp(6, 20);
    let popup = centered_rect(width, height, area);

    frame.render_widget(Clear, popup);

    let title = format!(
        " asset selector — {} match{} ",
        n,
        if n == 1 { "" } else { "es" }
    );
    let block = Block::default()
        .title(Line::from(Span::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        )))
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(Color::Yellow));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut lines: Vec<Line> = Vec::new();
    // Filter input line (with Enter-to-search hint when query is non-empty).
    let hint = if sel.filter.is_empty() {
        ""
    } else {
        " [Enter=search]"
    };
    lines.push(Line::from(vec![
        Span::styled(
            "> ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(sel.filter.clone()),
        Span::styled(
            "▏",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::SLOW_BLINK),
        ),
        Span::styled(hint, Style::default().fg(Color::DarkGray)),
    ]));
    lines.push(Line::from(""));

    // Compute visible window over the combined list. The "── server search ──"
    // divider consumes one row, but only when the fav/search boundary actually
    // falls inside the window — reserving it unconditionally clipped the last
    // item whenever the window had scrolled past the boundary.
    let avail = (inner.height as usize).saturating_sub(2);
    let start = sel.cursor.saturating_sub(avail / 2);
    let divider_visible = has_search
        && fav_count < n
        && fav_count >= start
        && fav_count < (start + avail.saturating_sub(1)).min(n);
    let max_visible = if divider_visible {
        avail.saturating_sub(1)
    } else {
        avail
    };
    let end = (start + max_visible).min(n);

    if n == 0 {
        lines.push(Line::from(Span::styled(
            "  (no matches)",
            Style::default().fg(Color::DarkGray),
        )));
        // Hint to press Enter to trigger a server search.
        if !sel.filter.is_empty() {
            lines.push(Line::from(Span::styled(
                "  Press Enter to search server",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )));
        }
    } else {
        // Build symbol→name lookup for the search section.
        let sym_to_name: std::collections::HashMap<&str, &str> = app
            .symbol_search_results
            .iter()
            .map(|h| (h.symbol.as_str(), h.name.as_str()))
            .collect();

        for (i, sym) in combined[start..end].iter().enumerate() {
            let global_i = start + i;

            // Insert the "— search —" divider before the first search result.
            if divider_visible && global_i == fav_count {
                lines.push(Line::from(Span::styled(
                    "── server search ──",
                    Style::default().fg(Color::Cyan),
                )));
            }

            let is_cursor = global_i == sel.cursor;
            let style = if is_cursor {
                Style::default()
                    .bg(Color::Yellow)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let marker = if is_cursor { "▶ " } else { "  " };

            // For search results (index >= fav_count), show symbol + name.
            if global_i >= fav_count {
                let name = sym_to_name.get(sym.as_str()).copied().unwrap_or("");
                let name_style = if is_cursor {
                    style
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                lines.push(Line::from(vec![
                    Span::styled(marker, style),
                    Span::styled(sym.clone(), style),
                    Span::styled(
                        if name.is_empty() {
                            String::new()
                        } else {
                            format!("  {name}")
                        },
                        name_style,
                    ),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::styled(marker, style),
                    Span::styled(sym.clone(), style),
                ]));
            }
        }
    }

    let para = Paragraph::new(lines);
    frame.render_widget(para, inner);
}
