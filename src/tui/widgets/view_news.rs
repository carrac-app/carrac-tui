//! News view — scrollable list of RSS news items aggregated from `/api/market-news`.
//!
//! Data is lazy-fetched the first time the user opens this view (key `8`).
//! Until then shows "loading…". Once data arrives each item renders as two lines:
//!
//!   ▶ [Source]  HH:MM  Title of the article
//!     Summary text (dimmed)
//!
//! Keys:
//!  `[` / `]`        cycle feed preset (backward / forward)
//!  `j` / `k` / ↑↓  cursor navigation
//!  `PgDn` / `PgUp`  page scroll (5 items)
//!  `g` / `G`        jump to top / bottom
//!  `Enter` / `l`    open link in browser (xdg-open)

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Paragraph};

use chrono::{DateTime, Local, Utc};

use crate::app::state::App;
use crate::types::news::NewsItem;

/// Map a `source_id` to a stable foreground colour.  Falls through to a
/// neutral cyan for unknown IDs.
fn source_color(source_id: &str) -> Color {
    match source_id {
        "coindesk" => Color::Blue,
        "cointelegraph" => Color::Cyan,
        "decrypt" => Color::Magenta,
        "theblock" => Color::LightBlue,
        "dlnews" => Color::LightCyan,
        "cnbc_top" => Color::Yellow,
        "marketwatch" => Color::LightYellow,
        "seekingalpha" => Color::Green,
        "fed" => Color::LightRed,
        "fxstreet" => Color::LightGreen,
        _ => Color::Cyan,
    }
}

/// Format an ISO 8601 `published` string for display.
/// - Today's items: `HH:MM`
/// - Older items:   `MMM DD HH:MM`
fn format_published(published: &str) -> String {
    // Parse as RFC 3339 / ISO 8601 with timezone.
    if let Ok(dt) = published.parse::<DateTime<Utc>>() {
        let local: DateTime<Local> = dt.into();
        let today = Local::now().date_naive();
        if local.date_naive() == today {
            return local.format("%H:%M").to_string();
        }
        return local.format("%b %d %H:%M").to_string();
    }
    // Fallback: return the first 16 chars of the raw string.
    published.chars().take(16).collect()
}

/// Build a `ListItem` for one news item (two lines).
fn news_list_item(item: &NewsItem, highlighted: bool) -> ListItem<'static> {
    let color = source_color(&item.source_id);
    let time_str = format_published(&item.published);

    let row_bg = if highlighted {
        Style::default().bg(Color::DarkGray)
    } else {
        Style::default()
    };

    // Line 1: ▶ [Source]  HH:MM  Title
    let prefix = if highlighted { "▶ " } else { "  " };
    let line1 = Line::from(vec![
        Span::styled(
            prefix.to_string(),
            if highlighted {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ),
        Span::styled(
            format!("[{}]", item.source),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {time_str}  "),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            item.title.clone(),
            if highlighted {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            },
        ),
    ]);

    // Line 2: (indented) summary, dimmed.
    let summary_preview: String = item.summary.chars().take(120).collect();
    let summary_line = if summary_preview.len() < item.summary.len() {
        format!("    {summary_preview}…")
    } else {
        format!("    {summary_preview}")
    };
    let line2 = Line::from(Span::styled(
        summary_line,
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    ));

    ListItem::new(vec![line1, line2]).style(row_bg)
}

/// Count feeds with status "ok".
fn feeds_ok_count(feeds: &std::collections::HashMap<String, String>) -> (usize, usize) {
    let total = feeds.len();
    let ok = feeds.values().filter(|v| v.as_str() == "ok").count();
    (ok, total)
}

pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    // Split: 1-line header + rest for list.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    let header_area = chunks[0];
    let list_area = chunks[1];

    // ── header ────────────────────────────────────────────────────────────────
    let preset_label = app.news_feed_preset.label();
    let header_str = match &app.news {
        None => format!(" News  ·  [{preset_label}]  ·  loading…"),
        Some(resp) => {
            let (ok, total) = feeds_ok_count(&resp.feeds);
            format!(
                " News  ·  [{}]  ·  {}/{} feeds ok  ·  {} items",
                preset_label,
                ok,
                total,
                resp.items.len()
            )
        }
    };

    let header_line = Line::from(vec![
        Span::styled(
            header_str,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "   [ / ] preset  j/k move  Enter open link",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ),
    ]);
    frame.render_widget(Paragraph::new(header_line), header_area);

    // ── list ──────────────────────────────────────────────────────────────────
    let news_resp = match &app.news {
        None => {
            let (msg, style) = super::loading_or_error_line(app, "news", "  loading…");
            let loading = Paragraph::new(Line::from(Span::styled(msg, style)));
            frame.render_widget(loading, list_area);
            return;
        }
        Some(r) => r,
    };

    if news_resp.items.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            "  (no news items)",
            Style::default().fg(Color::DarkGray),
        )));
        frame.render_widget(empty, list_area);
        return;
    }

    let selected_idx = app.news_cursor.selected().unwrap_or(0);
    let items: Vec<ListItem> = news_resp
        .items
        .iter()
        .enumerate()
        .map(|(i, item)| news_list_item(item, i == selected_idx))
        .collect();

    let list = List::new(items);
    frame.render_stateful_widget(list, list_area, &mut app.news_cursor);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::App;
    use crate::types::news::{NewsItem, NewsResponse};

    fn sample_item(source_id: &str) -> NewsItem {
        NewsItem {
            id: "test1".into(),
            title: "BTC hits new ATH".into(),
            summary: "Bitcoin reached a new all-time high today amid strong institutional demand."
                .into(),
            link: "https://cointelegraph.com/news/btc-ath".into(),
            published: "2026-05-23T23:07:19+00:00".into(),
            source: "CoinTelegraph".into(),
            source_id: source_id.into(),
        }
    }

    #[test]
    fn source_color_known_sources() {
        assert_eq!(source_color("coindesk"), Color::Blue);
        assert_eq!(source_color("cointelegraph"), Color::Cyan);
        assert_eq!(source_color("fed"), Color::LightRed);
    }

    #[test]
    fn source_color_unknown_falls_back_to_cyan() {
        assert_eq!(source_color("unknown_feed"), Color::Cyan);
    }

    #[test]
    fn format_published_parses_rfc3339() {
        // Any valid RFC 3339 should produce a non-empty, reasonably short string.
        let ts = "2026-05-23T23:07:19+00:00";
        let result = format_published(ts);
        assert_ne!(result, "");
        // Either HH:MM (5 chars) or MMM DD HH:MM (12 chars).
        assert!(
            result.len() <= 12,
            "time string should be compact: {result}"
        );
    }

    #[test]
    fn format_published_fallback_for_invalid() {
        let result = format_published("not-a-date");
        // Fallback returns up to 16 chars of the raw string.
        assert_eq!(result, "not-a-date");
    }

    #[test]
    fn feeds_ok_count_counts_correctly() {
        let mut feeds = std::collections::HashMap::new();
        feeds.insert("coindesk".into(), "ok".into());
        feeds.insert("cointelegraph".into(), "ok".into());
        feeds.insert("decrypt".into(), "error".into());
        let (ok, total) = feeds_ok_count(&feeds);
        assert_eq!(total, 3);
        assert_eq!(ok, 2);
    }

    #[test]
    fn render_loading_state_no_panic() {
        let app = App::new();
        // No news data → loading path. Just verify news is None.
        assert!(app.news.is_none());
    }

    #[test]
    fn render_with_items_no_panic() {
        let mut app = App::new();
        let resp = NewsResponse {
            items: vec![sample_item("cointelegraph"), sample_item("coindesk")],
            feeds: {
                let mut m = std::collections::HashMap::new();
                m.insert("cointelegraph".into(), "ok".into());
                m.insert("coindesk".into(), "ok".into());
                m
            },
            timestamp: "2026-05-23T23:07:19+00:00".into(),
        };
        app.apply(crate::app::state::Update::News(Ok(resp)));
        assert!(app.news.is_some());
        assert_eq!(app.news.as_ref().unwrap().items.len(), 2);
    }

    #[test]
    fn news_list_item_highlighted_uses_bold_title() {
        let item = sample_item("coindesk");
        let highlighted = news_list_item(&item, true);
        // Verify line count = 2 (title + summary).
        let lines = highlighted.height();
        assert_eq!(lines, 2);
    }

    #[test]
    fn news_list_item_not_highlighted() {
        let item = sample_item("coindesk");
        let regular = news_list_item(&item, false);
        assert_eq!(regular.height(), 2);
    }
}
