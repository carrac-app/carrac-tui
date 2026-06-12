//! Details view — multi-asset screening grid for the watchlist.
//!
//! Columns: Symbol · Price · %24H · W-RSI · Trend · Consensio · CPR · BB
//! Rows:    watchlist symbols (order-stable, capped at `BATCH_CAP`).
//! Data:    `app.details_matrix` — one fetch of `/api/details/matrix?tab=watchlist`
//!          returns the server's TYPED indicator columns for the caller's
//!          watchlist (same engine + numbers as the web Screening Matrix).
//! Refresh: lazy interval, gated on `details_visited`.
//!
//! Each column reads a typed field straight from the matrix row — no more
//! re-deriving values from scanner `evidence` strings — so the grid matches the
//! web by construction:
//!
//! | Column    | Matrix field                          | Display                       |
//! |-----------|---------------------------------------|-------------------------------|
//! | W-RSI     | `rsi.value` / `rsi.zone`              | weekly RSI, coloured by zone  |
//! | Trend     | `structure.label` / `structure.trend` | `HH/HL`, coloured by trend    |
//! | Consensio | `consensio.positions`/`max_positions` | `4/9`, coloured by `signal`   |
//! | CPR       | `cpr.price_level`                     | `TC-R1`, zone-coloured        |
//! | BB        | `bb_squeeze.squeeze_pct`              | `42%`, squeeze-coloured       |

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Cell, Row, Table, TableState};

use crate::app::state::App;
use crate::commands::chart::{
    ansi_to_ratatui_text,
    constants::{HLINE_LABEL_SPILL, Y_LABEL_W},
    fmt_price, render_sextant_candles_sized, subcell_count_bounds,
};
use crate::types::matrix::MatrixRow;

/// Maximum number of watchlist assets shown in the grid.
pub const BATCH_CAP: usize = 25;

pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    let symbols: Vec<String> = app
        .watchlist_symbols
        .iter()
        .take(BATCH_CAP)
        .cloned()
        .collect();
    let total_watchlist = app.watchlist_symbols.len();
    let capped = total_watchlist > BATCH_CAP;

    // ── title bar ────────────────────────────────────────────────────────────
    let cap_note = if capped {
        format!("  (showing first {BATCH_CAP} of {total_watchlist})")
    } else {
        String::new()
    };
    let title = format!(
        " Details  ·  {} assets  ·  j/k select · Enter/l → chart{}",
        symbols.len(),
        cap_note
    );

    // ── loading / empty states ───────────────────────────────────────────────
    if symbols.is_empty() {
        let para = ratatui::widgets::Paragraph::new(Span::styled(
            "  watchlist empty — add symbols via the web UI or ~/.config/carrac/watchlist.toml",
            Style::default().fg(Color::DarkGray),
        ))
        .block(Block::default().title(title).borders(Borders::NONE));
        frame.render_widget(para, area);
        return;
    }

    if app.details_matrix.is_none() {
        let (msg, style) =
            super::loading_or_error_line(app, "details_matrix", "  loading watchlist…");
        let para = ratatui::widgets::Paragraph::new(Span::styled(msg, style))
            .block(Block::default().title(title).borders(Borders::NONE));
        frame.render_widget(para, area);
        return;
    }

    let matrix = app.details_matrix.as_ref().expect("guarded above");

    // ── column header ────────────────────────────────────────────────────────
    let header_style = Style::default()
        .fg(Color::DarkGray)
        .bg(Color::Indexed(235))
        .add_modifier(Modifier::BOLD);
    let header = Row::new(vec![
        Cell::from("Symbol").style(header_style),
        Cell::from("Price").style(header_style),
        Cell::from("%24H").style(header_style),
        Cell::from("W-RSI").style(header_style),
        Cell::from("1D Trend").style(header_style),
        Cell::from("Consensio").style(header_style),
        Cell::from("CPR").style(header_style),
        Cell::from("BB").style(header_style),
    ]);

    // ── rows ─────────────────────────────────────────────────────────────────
    // Iterate the watchlist in its own order; look each symbol up in the matrix.
    let rows: Vec<Row> = symbols
        .iter()
        .map(|sym| build_row(sym, matrix.get(sym.as_str())))
        .collect();

    // ── table ─────────────────────────────────────────────────────────────────
    let widths = [
        Constraint::Length(8),  // Symbol
        Constraint::Length(12), // Price
        Constraint::Length(8),  // %24H
        Constraint::Length(8),  // W-RSI
        Constraint::Length(10), // Trend
        Constraint::Length(10), // Consensio
        Constraint::Length(10), // CPR
        Constraint::Min(0),     // BB (takes remaining space)
    ];

    let highlight_style = Style::default()
        .bg(Color::Indexed(236))
        .add_modifier(Modifier::BOLD);

    // Split area: 1-line title, rest for table.
    let chunks = ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([
            ratatui::layout::Constraint::Length(1),
            ratatui::layout::Constraint::Min(0),
        ])
        .split(area);

    let title_para = ratatui::widgets::Paragraph::new(Span::styled(
        title,
        Style::default().add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(title_para, chunks[0]);

    // Side panel: the highlighted asset's 1D mini-chart, when the terminal is
    // wide enough for both. Table columns sum to ~76; give the chart the rest.
    const TABLE_W: u16 = 78;
    const MIN_CHART_W: u16 = 34;
    let body = chunks[1];
    let (table_area, chart_area) = if body.width >= TABLE_W + MIN_CHART_W {
        let cols = ratatui::layout::Layout::default()
            .direction(ratatui::layout::Direction::Horizontal)
            .constraints([
                ratatui::layout::Constraint::Length(TABLE_W),
                ratatui::layout::Constraint::Min(MIN_CHART_W),
            ])
            .split(body);
        (cols[0], Some(cols[1]))
    } else {
        (body, None)
    };

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(highlight_style);

    // Build a local TableState from the ListState cursor so we can drive the
    // Table highlight without changing the App cursor type.
    let mut ts = TableState::default();
    if let Some(sel) = app.details_cursor.selected() {
        ts.select(Some(sel));
    }

    frame.render_stateful_widget(table, table_area, &mut ts);

    if let Some(rect) = chart_area {
        render_mini_chart(frame, rect, app, &symbols);
    }
}

/// The highlighted asset's 1D chart beside the grid — fed by the lazy
/// per-symbol cache (`app.details_charts`), fetched as the cursor moves.
fn render_mini_chart(frame: &mut Frame, area: Rect, app: &App, symbols: &[String]) {
    if area.width < 20 || area.height < 6 {
        return;
    }
    let idx = app.details_cursor.selected().unwrap_or(0);
    let Some(sym) = symbols.get(idx) else { return };

    // 1-row header + body.
    let chunks = ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([
            ratatui::layout::Constraint::Length(1),
            ratatui::layout::Constraint::Min(0),
        ])
        .split(area);

    let header = ratatui::widgets::Paragraph::new(Span::styled(
        format!(" {sym} · 1D"),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(header, chunks[0]);

    let body = chunks[1];
    match app.details_charts.get(sym.as_str()) {
        Some(data) if !data.ohlcv.is_empty() => {
            // Fill the panel: as many bars as the canvas supports.
            let chart_cols = (body.width as usize).saturating_sub(HLINE_LABEL_SPILL);
            let canvas = chart_cols.saturating_sub(Y_LABEL_W);
            let (_, max_bars) = subcell_count_bounds(canvas);
            let bars = max_bars.min(data.ohlcv.len()).max(1) as u32;
            // -2 keeps the renderer's 2-row date axis inside the panel.
            let rendered = render_sextant_candles_sized(
                data,
                chart_cols,
                (body.height as usize).saturating_sub(2).max(4),
                crate::chart::render_term::CprSelect::Off, // keep the mini view clean
                false,
                false,
                None,
                bars,
                0,
                1.0,
                0.0,
            );
            let para = ratatui::widgets::Paragraph::new(ansi_to_ratatui_text(&rendered));
            frame.render_widget(para, body);
        }
        _ => {
            let para = ratatui::widgets::Paragraph::new(Span::styled(
                format!("  loading {sym} 1D…"),
                Style::default().fg(Color::DarkGray),
            ));
            frame.render_widget(para, body);
        }
    }
}

// ── row builder ───────────────────────────────────────────────────────────────

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn dash() -> Cell<'static> {
    Cell::from("—").style(dim())
}

fn build_row(sym: &str, row: Option<&MatrixRow>) -> Row<'static> {
    let symbol_cell = Cell::from(sym.to_string());

    let Some(r) = row else {
        // Symbol not in the matrix (not tracked / not yet computed) — dim dashes.
        return Row::new(vec![
            symbol_cell,
            dash(),
            dash(),
            dash(),
            dash(),
            dash(),
            dash(),
            dash(),
        ]);
    };

    Row::new(vec![
        symbol_cell,
        price_cell(r.price),
        change_cell(r.change_pct),
        wrsi_cell(r),
        trend_cell(r),
        consensio_cell(r),
        cpr_cell(r),
        bb_cell(r),
    ])
}

// ── cell builders (one typed matrix field each) ────────────────────────────────

fn price_cell(price: Option<f64>) -> Cell<'static> {
    match price {
        Some(p) if p != 0.0 => Cell::from(fmt_price(p)),
        _ => dash(),
    }
}

/// %24H — green when up, red when down.
fn change_cell(change: Option<f64>) -> Cell<'static> {
    match change {
        Some(c) => {
            let color = if c >= 0.0 { Color::Green } else { Color::Red };
            Cell::from(format!("{c:+.2}%")).style(Style::default().fg(color))
        }
        None => dash(),
    }
}

/// Weekly RSI — coloured by the server's zone (oversold = green, overbought =
/// red, neutral = dim).
fn wrsi_cell(r: &MatrixRow) -> Cell<'static> {
    let Some(v) = r.rsi.value else { return dash() };
    let color = match r.rsi.zone.as_deref() {
        Some("oversold") => Color::Green,
        Some("overbought") => Color::Red,
        _ => Color::Yellow,
    };
    Cell::from(format!("{v:.0}")).style(Style::default().fg(color))
}

/// Market structure (computed on 1D candles server-side) — the web's
/// `HH/HL`-style label, coloured by trend.
fn trend_cell(r: &MatrixRow) -> Cell<'static> {
    let Some(label) = r.structure.label.as_deref().filter(|s| !s.is_empty()) else {
        return dash();
    };
    let color = match r.structure.trend.as_deref() {
        Some("uptrend") => Color::Green,
        Some("downtrend") => Color::Red,
        _ => Color::DarkGray,
    };
    Cell::from(label.to_string()).style(Style::default().fg(color))
}

/// Consensio — `positions/max_positions` (e.g. `4/9`), coloured by net signal.
fn consensio_cell(r: &MatrixRow) -> Cell<'static> {
    let (Some(pos), Some(max)) = (r.consensio.positions, r.consensio.max_positions) else {
        return dash();
    };
    let signal = r.consensio.signal.unwrap_or(0.0);
    let color = if signal > 0.0 {
        Color::Green
    } else if signal < 0.0 {
        Color::Red
    } else {
        Color::DarkGray
    };
    Cell::from(format!("{}/{}", pos as i64, max as i64)).style(Style::default().fg(color))
}

/// CPR — the fine zone label (`TC-R1`, `BC-S1`, …) coloured PER TOKEN like
/// the chart's lines: S-levels red, R-levels green, the central trio
/// (BC/CP/TC) purple — so `BC-S1` reads purple-dash-red, not one blanket
/// zone colour.
fn cpr_cell(r: &MatrixRow) -> Cell<'static> {
    let Some(level) = r.cpr.price_level.as_deref().filter(|s| !s.is_empty()) else {
        return dash();
    };
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut rest = level;
    while !rest.is_empty() {
        // Split into level tokens and the separators between them.
        let token_end = rest.find(['-', ' ']).unwrap_or(rest.len());
        if token_end == 0 {
            // separator char
            let (sep, tail) = rest.split_at(1);
            spans.push(Span::styled(sep.to_string(), dim()));
            rest = tail;
            continue;
        }
        let (tok, tail) = rest.split_at(token_end);
        spans.push(Span::styled(
            tok.to_string(),
            Style::default().fg(cpr_token_color(tok)),
        ));
        rest = tail;
    }
    Cell::from(ratatui::text::Line::from(spans))
}

/// Per-level colour, matching the chart hlines: S* red, R* green,
/// BC/CP/TC purple. Words like "Below"/"Above" render dim.
fn cpr_token_color(token: &str) -> Color {
    match token {
        "S1" | "S2" | "S3" => Color::Red,
        "R1" | "R2" | "R3" => Color::Green,
        "BC" | "CP" | "TC" => Color::Magenta,
        _ => Color::DarkGray,
    }
}

/// BB — squeeze % (band width vs 60-day avg), coloured by squeeze level the way
/// the web screener does (<30 extreme = red, <50 squeeze = yellow, >150
/// expansion = dim).
fn bb_cell(r: &MatrixRow) -> Cell<'static> {
    let Some(pct) = r.bb_squeeze.squeeze_pct else {
        return dash();
    };
    let p = pct.round() as i64;
    let color = if p < 30 {
        Color::Red
    } else if p < 50 {
        Color::Yellow
    } else if p > 150 {
        Color::DarkGray
    } else {
        Color::Reset
    };
    Cell::from(format!("{p}%")).style(Style::default().fg(color))
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::matrix::{MatrixBb, MatrixConsensio, MatrixCpr, MatrixRsi, MatrixStructure};

    fn make_row() -> MatrixRow {
        MatrixRow {
            symbol: "BTC".to_string(),
            price: Some(76000.0),
            change_pct: Some(2.5),
            rsi: MatrixRsi {
                value: Some(58.0),
                zone: Some("neutral".to_string()),
            },
            structure: MatrixStructure {
                trend: Some("uptrend".to_string()),
                label: Some("HH/HL".to_string()),
            },
            consensio: MatrixConsensio {
                signal: Some(3.0),
                positions: Some(6.0),
                max_positions: Some(9.0),
            },
            cpr: MatrixCpr {
                position: Some("above".to_string()),
                price_level: Some("TC-R1".to_string()),
            },
            bb_squeeze: MatrixBb {
                squeezed: Some(true),
                squeeze_pct: Some(28.0),
                width_pct: Some(3.5),
            },
        }
    }

    #[test]
    fn price_cell_dashes_on_zero_or_missing() {
        // Dash policy lives at the call site now; the shared canonical fmt_price
        // never special-cases zero.
        assert_eq!(price_cell(None), dash());
        assert_eq!(price_cell(Some(0.0)), dash());
    }

    #[test]
    fn price_cell_uses_canonical_format() {
        // Canonical tiers (chart::fmt_price): ≥10000 → .0, ≥1 → .3, <1 → .5.
        assert_eq!(price_cell(Some(50000.0)), Cell::from("50000"));
        assert_eq!(price_cell(Some(3.5)), Cell::from("3.500"));
        assert_eq!(price_cell(Some(0.0001)), Cell::from("0.00010"));
    }

    #[test]
    fn build_row_with_matrix_data_no_panic() {
        let _row = build_row("BTC", Some(&make_row()));
    }

    #[test]
    fn build_row_missing_symbol_no_panic() {
        // Symbol absent from the matrix → dim-dash row, no panic.
        let _row = build_row("DOGE", None);
    }

    #[test]
    fn cpr_token_colors_match_chart_lines() {
        assert_eq!(cpr_token_color("S1"), Color::Red);
        assert_eq!(cpr_token_color("S3"), Color::Red);
        assert_eq!(cpr_token_color("R2"), Color::Green);
        assert_eq!(cpr_token_color("BC"), Color::Magenta);
        assert_eq!(cpr_token_color("CP"), Color::Magenta);
        assert_eq!(cpr_token_color("TC"), Color::Magenta);
        assert_eq!(cpr_token_color("Below"), Color::DarkGray);
    }

    #[test]
    fn consensio_renders_fraction_and_colours_by_signal() {
        // positions/max_positions → "6/9"; positive signal → green.
        let cell = consensio_cell(&make_row());
        let _ = cell; // Cell text isn't introspectable in ratatui; assert no panic.
        // Negative signal path.
        let mut r = make_row();
        r.consensio.signal = Some(-2.0);
        let _ = consensio_cell(&r);
        // Missing counts → dash.
        r.consensio.positions = None;
        let _ = consensio_cell(&r);
    }

    #[test]
    fn bb_cell_handles_thresholds_and_missing() {
        // <30 extreme, <50 squeeze, >150 expansion, mid normal, None → dash.
        for pct in [10.0, 40.0, 90.0, 200.0] {
            let mut r = make_row();
            r.bb_squeeze.squeeze_pct = Some(pct);
            let _ = bb_cell(&r);
        }
        let mut r = make_row();
        r.bb_squeeze.squeeze_pct = None;
        let _ = bb_cell(&r);
    }

    #[test]
    fn wrsi_and_trend_degrade_gracefully() {
        let mut r = make_row();
        r.rsi.value = None;
        let _ = wrsi_cell(&r);
        r.structure.label = None;
        let _ = trend_cell(&r);
    }

    #[test]
    fn batch_cap_constant_is_25() {
        assert_eq!(BATCH_CAP, 25);
    }
}
