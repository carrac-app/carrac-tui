//! Asset row — current symbol · timeframe · chart fetch status.
//!
//! When the chart crosshair is pinned to a candle, the right side of the row
//! replaces the refresh status with that candle's date · O/H/L/C/V so the
//! user gets numeric values for the visual point they clicked.

use chrono::{DateTime, Utc};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::state::{App, View};
use crate::commands::chart::{
    candle_at_screen_col, constants::MAX_VISIBLE_BARS, constants::Y_LABEL_W, fmt_price,
    fmt_price_short,
};

const CHART_REFRESH_SECS: u64 = 30;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(
            app.selected_asset.clone(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ·  ", Style::default().fg(Color::DarkGray)),
        Span::styled(app.chart.tf.clone(), Style::default().fg(Color::Cyan)),
    ];

    // Show active search query indicator (even after prompt is closed).
    if let Some(s) = &app.search
        && s.is_active()
    {
        let indicator = match s.mode {
            crate::app::state::SearchMode::Filter => {
                format!("  [/{}]", s.query)
            }
            crate::app::state::SearchMode::Find => {
                if s.matches.is_empty() {
                    format!("  [find: {} · 0 matches]", s.query)
                } else {
                    format!(
                        "  [find: {} · {}/{}]",
                        s.query,
                        s.current_match + 1,
                        s.matches.len()
                    )
                }
            }
        };
        spans.push(Span::styled(
            indicator,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }

    // Crosshair lookup — if pinned, show that candle's OHLCV. This wins over
    // the refresh countdown so the user sees what they clicked.
    if let Some(extras) = crosshair_ohlcv(app) {
        spans.push(Span::styled("  ·  ", Style::default().fg(Color::DarkGray)));
        spans.extend(extras);
    } else {
        let stale = chart_payload_stale(app);
        let age = app.secs_since("chart");
        let (text, style) = match (stale, age) {
            (true, _) => (
                "  fetching…".to_string(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            (false, Some(s)) => {
                let next = CHART_REFRESH_SECS.saturating_sub(s);
                (
                    format!("  next in {}s", next),
                    Style::default().fg(Color::DarkGray),
                )
            }
            (false, None) => (
                "  —".to_string(),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ),
        };
        spans.push(Span::styled(text, style));

        // Visible-slice range stats inline (chart view, fresh data only) —
        // replaces the dedicated footer row the chart briefly had. Yields to
        // the crosshair OHLCV above, which is the more specific lookup.
        if app.view == View::Chart
            && !stale
            && let Some((high, low, pct)) = app.chart_range
        {
            let dim = Style::default().fg(Color::DarkGray);
            spans.push(Span::styled("  ·  H ", dim));
            spans.push(Span::raw(fmt_price_short(high)));
            spans.push(Span::styled("  L ", dim));
            spans.push(Span::raw(fmt_price_short(low)));
            spans.push(Span::raw("  "));
            let pct_color = if pct < 0.0 { Color::Red } else { Color::Green };
            spans.push(Span::styled(
                format!("{:+.2}%", pct),
                Style::default().fg(pct_color).add_modifier(Modifier::BOLD),
            ));
        }
    }

    let para = Paragraph::new(Line::from(spans));
    frame.render_widget(para, area);
}

/// True when the cached chart_data doesn't match the user's current
/// selected_asset + tf — i.e. a re-fetch is pending or in flight.
pub fn chart_payload_stale(app: &App) -> bool {
    match &app.chart_data {
        None => true,
        Some(d) => {
            !d.symbol.eq_ignore_ascii_case(&app.selected_asset)
                || !d.timeframe.eq_ignore_ascii_case(&app.chart.tf)
        }
    }
}

/// Build the OHLCV span list for the candle currently under the crosshair,
/// or None when no crosshair is set / no candle resolves at that column.
fn crosshair_ohlcv(app: &App) -> Option<Vec<Span<'static>>> {
    let xhair = app.chart.crosshair_col?;
    let data = app.chart_data.as_ref()?;
    let rect = app.last_chart_rect?;
    let chart_cols = (rect.width as usize)
        .saturating_sub(Y_LABEL_W)
        .min(MAX_VISIBLE_BARS);
    let idx = candle_at_screen_col(
        data,
        chart_cols,
        rect.x,
        app.chart.visible_bars as u32,
        app.chart.offset_from_right,
        xhair,
    )?;
    let c = data.ohlcv.get(idx)?;
    let v = data
        .volume
        .iter()
        .find(|v| v.time == c.time)
        .map(|v| v.value);

    let dt = DateTime::<Utc>::from_timestamp(c.time, 0)
        .map(|d| d.format("%m-%d %H:%M").to_string())
        .unwrap_or_else(|| c.time.to_string());

    let is_bull = c.close >= c.open;
    let body_color = if is_bull { Color::Green } else { Color::Red };

    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::styled(
        format!("{}  ", dt),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(label_value("O", &fmt_price(c.open), Color::Reset));
    spans.push(Span::raw("  "));
    spans.push(label_value("H", &fmt_price(c.high), Color::Reset));
    spans.push(Span::raw("  "));
    spans.push(label_value("L", &fmt_price(c.low), Color::Reset));
    spans.push(Span::raw("  "));
    spans.push(label_value("C", &fmt_price(c.close), body_color));
    if let Some(v) = v {
        spans.push(Span::raw("  "));
        spans.push(label_value("V", &fmt_volume(v), Color::Reset));
    }
    Some(spans)
}

fn label_value(label: &str, value: &str, color: Color) -> Span<'static> {
    Span::styled(format!("{} {}", label, value), Style::default().fg(color))
}

fn fmt_volume(v: f64) -> String {
    if v >= 1_000_000.0 {
        format!("{:.2}M", v / 1_000_000.0)
    } else if v >= 1_000.0 {
        format!("{:.2}K", v / 1_000.0)
    } else {
        format!("{:.2}", v)
    }
}
