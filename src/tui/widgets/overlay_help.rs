//! Help overlay — keybindings cheatsheet.
//!
//! Sized dynamically to fit the current terminal. When the content is taller
//! than the popup, `app.help_scroll` shifts which lines are visible (j/k or
//! ↑/↓ in the event handler). Any other key dismisses the overlay.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::app::state::App;
use crate::tui::layout::centered_rect;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let lines = build_lines();
    let content_h = lines.len() as u16;

    // Popup grows to fit, capped at (terminal height - 2) so the borders are
    // always visible.
    let desired_w: u16 = 64;
    let desired_h: u16 = content_h + 2; // +2 for borders
    let max_h = area.height.saturating_sub(2).max(6);
    let popup_h = desired_h.min(max_h);
    let popup_w = desired_w.min(area.width.saturating_sub(2)).max(20);

    let popup = centered_rect(popup_w, popup_h, area);
    frame.render_widget(Clear, popup);

    let scrollable = content_h + 2 > popup_h;
    let title = if scrollable {
        format!(
            " carrac tui — keys  ({}/{}  j/k to scroll)",
            (app.help_scroll + 1).min(content_h),
            content_h
        )
    } else {
        " carrac tui — keys ".to_string()
    };

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

    // Clamp scroll so the last line stays visible.
    let inner_h = inner.height;
    let max_scroll = content_h.saturating_sub(inner_h);
    let scroll = app.help_scroll.min(max_scroll);

    let para = Paragraph::new(lines).scroll((scroll, 0));
    frame.render_widget(para, inner);
}

fn build_lines() -> Vec<Line<'static>> {
    let sections: Vec<(&'static str, Vec<(&'static str, &'static str)>)> = vec![
        (
            "GLOBAL",
            vec![
                (
                    "1..7",
                    "switch view: chart · pos · plays · alerts · report · news · details",
                ),
                ("Tab / Shift+Tab", "cycle to next / previous tab"),
                ("Space", "cycle to next tab (wraps around)"),
                ("a", "open asset selector"),
                (
                    "I",
                    "open your AI agent ('Intelligence') with this view's context — takes over full-screen; quit it to return (Ctrl-D / /exit for Claude, Ctrl-C for others)",
                ),
                (", / .", "cycle favourite asset (prev / next)"),
                ("r", "force-refresh all data"),
                ("?", "this help (j/k scrolls, any other key dismisses)"),
                ("q", "quit"),
            ],
        ),
        (
            "CHART VIEW",
            vec![
                ("t / T", "cycle timeframe (next / prev: 5m→15m→1H→4H→1D→1W)"),
                ("d / w", "jump to 1D / 1W"),
                ("h / l", "pan left / right (small step)"),
                ("H / L", "pan left / right (half-window)"),
                ("j / k", "y-zoom out / in"),
                ("↑↓ / Alt+k j", "y-pan: shift the price window up / down"),
                (
                    "] / [",
                    "more / fewer candles (zoom) — also sets candle width",
                ),
                ("0", "reset pan + zoom (x and y)"),
                ("c", "cycle CPR set: monthly → weekly → daily → off"),
                (
                    "e E p v R o m",
                    "toggle EMA21 · EMA-Ribbon · position · vol · RSI · overlays · MACD",
                ),
                ("z", "toggle market-session strip (intraday TFs only)"),
                ("s", "toggle candle style (sextant ⇄ line)"),
                (
                    "P",
                    "render PNG chart (server or client-side) — display inline via kitten icat",
                ),
                ("left-click", "pin crosshair to that column"),
                ("right-click / x", "clear crosshair"),
            ],
        ),
        (
            "LIST VIEWS (positions, plays, alerts)",
            vec![
                ("j / k / ↑↓", "move cursor in list"),
                ("Enter / l", "open detail card for selected row"),
                (
                    "h / Esc / any",
                    "dismiss detail overlay (j/k scrolls inside)",
                ),
            ],
        ),
        (
            "POSITIONS VIEW",
            vec![("f / s / b", "filter futures · spot · both")],
        ),
        (
            "ALERTS VIEW",
            vec![(
                "A",
                "create new alert (form: Tab cycles fields, Enter submits)",
            )],
        ),
        (
            "REPORT VIEW",
            vec![
                ("[ / ]", "toggle morning / afternoon report (jumps to top)"),
                ("w", "save current report as .md in the working directory"),
                ("j / k / ↑↓", "scroll one line"),
                ("PgDn / PgUp", "scroll 5 lines"),
                ("g", "jump to top"),
                ("G", "jump to bottom"),
                ("/", "open Find search prompt"),
                ("n / N", "next / previous match"),
            ],
        ),
        (
            "SEARCH (positions, plays, alerts, report)",
            vec![
                (
                    "/",
                    "open search prompt (Filter in list views, Find in report)",
                ),
                ("type", "filter/search as you type (live)"),
                ("Backspace", "delete last char"),
                ("Enter", "close prompt but keep query active"),
                ("Esc (prompt open)", "close prompt AND clear query"),
                ("Esc (prompt closed)", "clear active query"),
                ("n / N", "report only: jump to next / previous match"),
                ("", "switching views clears the active search"),
            ],
        ),
        (
            "ASSET SELECTOR",
            vec![
                ("type", "filter (substring match)"),
                ("↑ ↓", "navigate"),
                ("Enter", "select · with a query typed: server symbol search"),
                ("Esc", "cancel"),
            ],
        ),
        (
            "NEWS VIEW",
            vec![
                (
                    "[ / ]",
                    "cycle feed preset: default → all-crypto → all-macro → all",
                ),
                ("j / k / ↑↓", "move cursor"),
                ("PgDn / PgUp", "page scroll (5 items)"),
                ("g / G", "jump to top / bottom"),
                ("Enter / l", "open link in browser (xdg-open / $BROWSER)"),
                ("r", "force-refresh news (global key)"),
            ],
        ),
        (
            "DETAILS VIEW  (7)",
            vec![
                ("j / k / ↑↓", "move cursor up / down"),
                ("PgDn / PgUp", "cursor ±5 rows"),
                ("g / G", "jump to top / bottom"),
                ("Enter / l", "switch to chart view for selected symbol"),
                (
                    "P",
                    "render PNG chart for cursor-row symbol at 1D — display inline via kitten icat",
                ),
                ("r", "force-refresh (global)"),
                (
                    "",
                    "refreshes every 30 s · shows first 25 watchlist symbols",
                ),
            ],
        ),
        (
            "HELP OVERLAY",
            vec![
                ("j / k / ↑↓", "scroll line-by-line"),
                ("PgUp / PgDn", "scroll by 5"),
                ("any other key", "dismiss"),
            ],
        ),
    ];

    let mut lines: Vec<Line> = Vec::new();
    for (title, rows) in sections {
        lines.push(Line::from(Span::styled(
            title,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        for (k, v) in rows {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<13} ", k), Style::default().fg(Color::Cyan)),
                Span::raw(v),
            ]));
        }
        lines.push(Line::from(""));
    }
    lines
}
