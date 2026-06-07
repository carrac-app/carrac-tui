pub mod asset_bar;
pub mod chart;
pub mod menubar;
pub mod overlay_add_alert;
pub mod overlay_asset;
pub mod overlay_detail;
pub mod overlay_help;
pub mod overlay_search;
pub mod view_alerts;
pub mod view_chart;
pub mod view_details;
pub mod view_news;
pub mod view_plays;
pub mod view_positions;
pub mod view_report;

use crate::app::state::App;
use crate::types::positions::Direction;
use ratatui::style::{Color, Style};

/// The one-liner shown for every Carrac-only surface when running the keyless
/// Binance demo. Single source of truth (TUI views + CLI commands).
pub const GATED_DEMO_NOTICE: &str = "needs a Carrac API key — carrac.cc/reg-tui";

/// Render the demo-gate notice into `area`: a single centered dim line. Used by
/// the Carrac-only views (positions / plays / alerts / report / news / details)
/// when `app.demo` is true.
pub fn render_gated_notice(frame: &mut ratatui::Frame, area: ratatui::layout::Rect) {
    use ratatui::layout::Alignment;
    use ratatui::widgets::Paragraph;
    let mid = area.y + area.height / 2;
    let line_area = ratatui::layout::Rect {
        x: area.x,
        y: mid.min(area.y + area.height.saturating_sub(1)),
        width: area.width,
        height: 1,
    };
    let p = Paragraph::new(GATED_DEMO_NOTICE)
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Center);
    frame.render_widget(p, line_area);
}

/// Color for a direction label: long=green, short=red, anything else=reset.
/// Single source of truth — widgets call this instead of each matching the
/// raw "long"/"short" string literals (the typo trap the audit flagged).
pub fn direction_color(label: &str) -> Color {
    match Direction::from_label(label) {
        Some(Direction::Long) => Color::Green,
        Some(Direction::Short) => Color::Red,
        None => Color::Reset,
    }
}

/// Placeholder line shown by list views (positions / plays / alerts) when their
/// data Option is still `None`. If the last fetch for `source` failed it surfaces
/// the error (dim red) instead of pinning on "loading…" forever; otherwise it
/// shows the genuine still-loading text. `loading` should read e.g.
/// "  loading plays…".
pub fn loading_or_error_line(app: &App, source: &'static str, loading: &str) -> (String, Style) {
    match app.fetch_errors.get(source) {
        Some(err) => (
            format!("  {} error: {}", source, err),
            Style::default()
                .fg(Color::Red)
                .add_modifier(ratatui::style::Modifier::DIM),
        ),
        None => (loading.to_string(), Style::default().fg(Color::DarkGray)),
    }
}
