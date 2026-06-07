//! Full-screen chart view — the default `carrac tui` landing surface.
//!
//! Renders the same block-candle chart as `carrac chart <SYM>` but driven from
//! the shared `App::chart` + `App::chart_data` state. Header/footer that the
//! standalone command draws are dropped — the cockpit's statusline owns those.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::Text;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::state::App;
use crate::commands::chart::{
    ansi_to_ratatui_text,
    constants::{
        HLINE_LABEL_SPILL, MACD_PANE_H, MAX_VISIBLE_BARS, RSI_PANE_H, VOL_PANE_H, Y_LABEL_W,
    },
    pane_slice, render_line_sized, render_macd_to_buf, render_rsi_to_buf, render_sessions_to_buf,
    render_sextant_candles_sized, render_volume_to_buf, render_x_axis_2col, subcell_count_bounds,
};

/// The count-driven sub-cell styles (braille, sextant) share one pitch across
/// candles, volume, RSI and the x-axis so everything lines up.
fn is_subcell(style: &str) -> bool {
    style == "sextant"
}
use crate::tui::widgets::asset_bar::chart_payload_stale;

pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    // Sub-pane heights — same recipe as the standalone command.
    // A 1-row separator is inserted between each pair of panes so the
    // pane boundaries are visible at a glance.
    let xaxis_h: u16 = 1;
    // Session strip: 1 row, only when toggled AND the server actually sent
    // session data (intraday TFs). On 1D+ `sessions` is empty so the strip
    // auto-hides — no blank row reserved.
    let mut session_h: u16 = if app.chart.show_sessions
        && app
            .chart_data
            .as_ref()
            .map(|d| !d.sessions.is_empty())
            .unwrap_or(false)
    {
        1
    } else {
        0
    };
    let mut vol_h: u16 = if app.chart.show_volume { VOL_PANE_H } else { 0 };
    let mut rsi_h: u16 = if app.chart.show_rsi { RSI_PANE_H } else { 0 };
    let mut macd_h: u16 = if app.chart.show_macd { MACD_PANE_H } else { 0 };

    // Chart-body rows left after the sub-panes take their cut (one separator
    // before the x-axis plus one before each shown pane).
    let body_budget = |s: u16, v: u16, r: u16, m: u16| {
        let separators_h: u16 =
            1 + (s > 0) as u16 + (v > 0) as u16 + (r > 0) as u16 + (m > 0) as u16;
        area.height
            .saturating_sub(xaxis_h + s + v + r + m + separators_h)
    };

    // Small terminals: with volume+RSI on by default the fixed overhead is
    // 16-17 rows, which silently collapsed the candles to a 0-row blank on
    // short panes. Auto-hide the least essential panes (MACD → RSI → volume
    // → sessions) until the body gets at least MIN_CHART_H rows. Presentation
    // only — the user's toggles in `app.chart` are not touched.
    const MIN_CHART_H: u16 = 6;
    if body_budget(session_h, vol_h, rsi_h, macd_h) < MIN_CHART_H {
        macd_h = 0;
    }
    if body_budget(session_h, vol_h, rsi_h, macd_h) < MIN_CHART_H {
        rsi_h = 0;
    }
    if body_budget(session_h, vol_h, rsi_h, macd_h) < MIN_CHART_H {
        vol_h = 0;
    }
    if body_budget(session_h, vol_h, rsi_h, macd_h) < MIN_CHART_H {
        session_h = 0;
    }
    let chart_h = body_budget(session_h, vol_h, rsi_h, macd_h);

    // Build (constraint, is_separator) so we can identify spacer rows for
    // rendering the divider line after layout.
    let mut blocks: Vec<(Constraint, bool)> = Vec::new();
    blocks.push((Constraint::Length(chart_h), false)); // chart body
    blocks.push((Constraint::Length(1), true)); // separator before xaxis
    blocks.push((Constraint::Length(xaxis_h), false)); // xaxis
    if session_h > 0 {
        blocks.push((Constraint::Length(1), true)); // separator before sessions
        blocks.push((Constraint::Length(session_h), false));
    }
    if vol_h > 0 {
        blocks.push((Constraint::Length(1), true)); // separator before vol
        blocks.push((Constraint::Length(vol_h), false));
    }
    if rsi_h > 0 {
        blocks.push((Constraint::Length(1), true)); // separator before rsi
        blocks.push((Constraint::Length(rsi_h), false));
    }
    if macd_h > 0 {
        blocks.push((Constraint::Length(1), true)); // separator before macd
        blocks.push((Constraint::Length(macd_h), false));
    }

    let constraints: Vec<Constraint> = blocks.iter().map(|(c, _)| *c).collect();
    let chunks_all = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    // Map (pane-name → rect). Skip separators in the chunks indexing below.
    let mut pane_idx_for: Vec<usize> = Vec::new();
    for (i, (_, is_sep)) in blocks.iter().enumerate() {
        if !*is_sep {
            pane_idx_for.push(i);
        }
    }
    // chunks_all[pane_idx_for[0]] = chart body, [1] = xaxis, [2] = vol (if shown), [3] = rsi.
    // Re-bind to keep the downstream code readable.
    let chunks: Vec<Rect> = pane_idx_for.iter().map(|&i| chunks_all[i]).collect();

    // Paint separator rows with a dim ─.
    {
        let buf = frame.buffer_mut();
        let sep_style = ratatui::style::Style::default()
            .fg(ratatui::style::Color::DarkGray)
            .add_modifier(ratatui::style::Modifier::DIM);
        for (i, (_, is_sep)) in blocks.iter().enumerate() {
            if !*is_sep {
                continue;
            }
            let r = chunks_all[i];
            for col in 0..r.width {
                let x = r.x + col;
                let y = r.y;
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char('─');
                    cell.set_style(sep_style);
                }
            }
        }
    }

    let width = area.width as usize;
    let chart_rows = chart_h as usize;
    // Renderers take the TOTAL width and subtract the y-label gutter
    // internally — but hline labels (CPR / live price) spill PAST the canvas,
    // so leave HLINE_LABEL_SPILL columns of right headroom or every hline row
    // overflows the pane (wrapped rows shoved the chart apart).
    let chart_cols = width
        .saturating_sub(HLINE_LABEL_SPILL)
        .min(MAX_VISIBLE_BARS + Y_LABEL_W);

    // Clamp the pan offset to the available data so `h`/`H` can't scroll past
    // the oldest candle into a blank chart with no obvious way back. (Read the
    // length first so the immutable borrow is released before we mutate.)
    app.chart.count_bounds = None;
    if let Some(total) = app.chart_data.as_ref().map(|d| d.ohlcv.len()) {
        let cc = chart_cols.saturating_sub(Y_LABEL_W);
        let subcell = is_subcell(&app.chart.style.to_lowercase());
        let (s0, e0, _) = pane_slice(total, cc, app.chart.visible_bars as u32, 0, subcell);
        let count = e0.saturating_sub(s0);
        let max_offset = total.saturating_sub(count);
        app.chart.offset_from_right = app.chart.offset_from_right.min(max_offset);
        // Publish the canvas-derived [/] clamp range so the zoom keys can't
        // drift outside what this width can actually render, and snap the
        // accumulator into range — a default bar count above the canvas max
        // otherwise costs one dead keypress before anything moves.
        if subcell {
            let (lo, hi) = subcell_count_bounds(cc);
            app.chart.set_count_bounds(lo, hi.min(total.max(1)));
        }
    }

    // If the cached payload doesn't match the user's current symbol+tf, treat
    // it as missing so we don't paint stale BTC candles after switching to ETH.
    let stale = chart_payload_stale(app);

    let mut range: Option<(f64, f64, f64)> = None;
    let (chart_text, xaxis_text) = match (&app.chart_data, stale) {
        (Some(data), false) => {
            let canvas_cols = chart_cols.saturating_sub(Y_LABEL_W);
            let total = data.ohlcv.len();
            let style = app.chart.style.to_lowercase();
            let subcell = is_subcell(&style);

            // One slice for everything: the sub-cell styles share a count-driven
            // pitch, block/line fall back to block geometry. This keeps the
            // x-axis labels (and the volume/RSI bars below) aligned with the candles.
            let (start, end, candle_width) = pane_slice(
                total,
                canvas_cols,
                app.chart.visible_bars as u32,
                app.chart.offset_from_right,
                subcell,
            );
            let candle_slice = &data.ohlcv[start..end];

            let body = match style.as_str() {
                "line" => render_line_sized(
                    data,
                    chart_cols,
                    chart_rows,
                    app.chart.cpr_sel,
                    app.chart.show_ema,
                    None,
                    app.chart.visible_bars as u32,
                    app.chart.offset_from_right,
                    app.chart.y_zoom,
                    app.chart.y_offset,
                ),
                _ => render_sextant_candles_sized(
                    data,
                    chart_cols,
                    chart_rows,
                    app.chart.cpr_sel,
                    app.chart.show_ema,
                    app.chart.show_ema_ribbon,
                    None,
                    app.chart.visible_bars as u32,
                    app.chart.offset_from_right,
                    app.chart.y_zoom,
                    app.chart.y_offset,
                ),
            };
            let xaxis =
                render_x_axis_2col(candle_slice, canvas_cols, candle_width, Y_LABEL_W, data);
            // Range stats of the RENDERED slice — surfaced inline in the
            // asset bar (H / L / change%), not as a dedicated footer row.
            range = crate::chart::stats::compute_range_stats(candle_slice)
                .map(|st| (st.high, st.low, st.change_pct));
            (ansi_to_ratatui_text(&body), ansi_to_ratatui_text(&xaxis))
        }
        _ => {
            let msg = if app.fetch_errors.contains_key("chart") {
                format!(
                    "  chart fetch error: {}",
                    app.fetch_errors.get("chart").unwrap()
                )
            } else {
                format!("  loading {} {}…", app.selected_asset, app.chart.tf)
            };
            (
                Text::styled(msg, Style::default().fg(Color::DarkGray)),
                Text::raw(""),
            )
        }
    };
    app.chart_range = range;

    // Chart art must CLIP at the pane edge, never wrap — a wrapped hline
    // label lands at the left of the next row and shoves the chart apart.
    let chart_para = Paragraph::new(chart_text).block(Block::default().borders(Borders::NONE));
    frame.render_widget(chart_para, chunks[0]);

    let xaxis_para = Paragraph::new(xaxis_text).block(Block::default().borders(Borders::NONE));
    frame.render_widget(xaxis_para, chunks[1]);

    // Sub-panes are written directly into the buffer. Only when the cached
    // payload actually matches the user's selection — otherwise we'd paint
    // stale volume / RSI bars under the "loading…" header.
    let subcell = is_subcell(&app.chart.style.to_lowercase());
    let mut idx = 2usize;
    if session_h > 0 {
        if let (Some(data), false) = (&app.chart_data, stale) {
            let buf = frame.buffer_mut();
            render_sessions_to_buf(
                data,
                chart_cols,
                chunks[idx],
                buf,
                app.chart.visible_bars as u32,
                app.chart.offset_from_right,
                subcell,
            );
        }
        idx += 1;
    }
    if vol_h > 0 {
        if let (Some(data), false) = (&app.chart_data, stale) {
            let buf = frame.buffer_mut();
            render_volume_to_buf(
                data,
                chart_cols,
                chunks[idx],
                buf,
                app.chart.visible_bars as u32,
                app.chart.offset_from_right,
                subcell,
            );
        }
        idx += 1;
    }
    if rsi_h > 0 {
        if let (Some(data), false) = (&app.chart_data, stale) {
            let buf = frame.buffer_mut();
            render_rsi_to_buf(
                data,
                chart_cols,
                chunks[idx],
                buf,
                app.chart.visible_bars as u32,
                app.chart.offset_from_right,
                subcell,
            );
        }
        idx += 1;
    }
    if macd_h > 0 {
        if let (Some(data), false) = (&app.chart_data, stale) {
            let buf = frame.buffer_mut();
            render_macd_to_buf(
                data,
                chart_cols,
                chunks[idx],
                buf,
                app.chart.visible_bars as u32,
                app.chart.offset_from_right,
                subcell,
            );
        }
        idx += 1;
    }

    // Record the on-screen rect occupied by the chart + sub-panes so the
    // mouse handler can decide whether a click is "in the chart".
    let last_pane_idx = idx.saturating_sub(1);
    let last_y = chunks[last_pane_idx].y + chunks[last_pane_idx].height;
    app.last_chart_rect = Some(Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: last_y.saturating_sub(area.y),
    });

    // Crosshair overlay — vertical bar across chart body + x-axis + sub-panes.
    if let Some(xhair_col) = app.chart.crosshair_col {
        let buf = frame.buffer_mut();
        let pane_top = chunks[0].y;
        let pane_bot = last_y;
        let xhair_style = Style::default().fg(Color::Indexed(93)); // bright yellow
        for y in pane_top..pane_bot {
            if xhair_col >= area.x
                && xhair_col < area.x + area.width
                && let Some(cell) = buf.cell_mut((xhair_col, y))
            {
                cell.set_symbol("│");
                cell.set_style(xhair_style);
            }
        }
    }
}
