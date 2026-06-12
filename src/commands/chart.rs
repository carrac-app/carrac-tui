/// `carrac chart <SYMBOL>` — sextant candlestick chart, works in any terminal.
///
/// Public facade for the chart subsystem: this module is the single entry point
/// callers import; it re-exports the implementation that lives in `chart::*`.
///
/// This file is a thin shim: it re-exports the public API surface so that all
/// existing callers (`main.rs`, TUI widgets, `tui.rs`) continue to work without
/// path changes. The implementation lives in the `chart::*` sibling modules.
///
/// - Data types + fetch helpers → `chart::data`
/// - Terminal render functions  → `chart::render_term`
/// - PNG render                 → `chart::render`
/// - App state + TUI event loop → `chart::app`
/// - KGP inline image display   → `chart::kgp`
use crate::commands::chart::constants::{MAX_CANDLE_WIDTH, Y_LABEL_W};

// ─── Layout / indicator constants ────────────────────────────────────────────

pub(crate) mod constants {
    /// Width (chars) reserved on the left for price-axis labels.
    pub(crate) const Y_LABEL_W: usize = 10;
    /// Height (terminal rows) of the volume sub-pane when visible.
    pub(crate) const VOL_PANE_H: u16 = 5;
    /// Height (terminal rows) of the RSI sub-pane when visible.
    pub(crate) const RSI_PANE_H: u16 = 7;
    // (Static OSC_DISPLAY_MIN/MAX removed — RSI now auto-zooms to the visible
    // data range, anchored to keep 30 and 70 always in view.)
    /// Maximum number of visible candles rendered at once.
    /// Also caps the `chart_cols.saturating_sub(Y_LABEL_W).min(MAX_VISIBLE_BARS)` clamp
    /// in the TUI layout so the chart body never grows past this many columns.
    pub(crate) const MAX_VISIBLE_BARS: usize = 200;
    /// Right-edge headroom interactive callers must leave when sizing
    /// `chart_cols`: `build_chart_rows` appends hline labels (CPR "TC·M 104664",
    /// live "<< 104664", position ">> ...") PAST the canvas — 1 space + up to
    /// 12 label chars (the CPR set tag adds "·M"). Without this margin every
    /// hline row overflows the pane.
    pub(crate) const HLINE_LABEL_SPILL: usize = 13;
    /// Maximum terminal columns allocated per candle.
    /// 1 col body + 1 col gap = 2 cols. Beyond this the gap between candles
    /// dominates and the chart looks sparse on wide terminals. When the
    /// canvas is wider than `visible_bars * MAX_CANDLE_WIDTH`, the renderer
    /// auto-fills with more bars instead of widening per-candle spacing.
    pub(crate) const MAX_CANDLE_WIDTH: usize = 2;
    /// Period for the RSI Wilder-smoothing calculation (RSI-14).
    pub(crate) const RSI_PERIOD: usize = 14;
    /// Header pane height (TUI layout).
    pub(crate) const HEADER_H: u16 = 1;
    /// Footer pane height (TUI layout, includes top border).
    pub(crate) const FOOTER_H: u16 = 3;
}

// ─── CLI args ────────────────────────────────────────────────────────────────

#[derive(clap::Args)]
pub struct ChartArgs {
    /// Symbol to chart (e.g. BTC, ETH, NVDA)
    pub symbol: String,

    /// Timeframe: 1H, 4H, 1D, 1W
    #[arg(long, default_value = "4H")]
    pub tf: String,

    /// Number of visible candles (render-time slice)
    #[arg(long, default_value_t = 80)]
    pub bars: u32,

    /// Comma-separated indicator overlays: CPR, EMA21
    #[arg(long, value_delimiter = ',')]
    pub indicators: Vec<String>,

    /// Overlay a position's SL/Entry/TP lines (by position ID)
    #[arg(long)]
    pub position: Option<u64>,

    /// Render mode: sextant (default) or line (close-price dot)
    #[arg(long, default_value = "sextant")]
    pub style: String,

    /// Static one-shot output — no TUI (good for scripts/piping)
    #[arg(long)]
    pub once: bool,

    /// Render as PNG instead of terminal characters. Inline via Kitty Graphics
    /// Protocol on kitty/ghostty/wezterm; halfblock fallback elsewhere.
    #[arg(long)]
    pub png: bool,

    /// Write PNG to this path instead of displaying inline (implies --png).
    #[arg(long, value_name = "PATH")]
    pub out: Option<String>,
}

// ─── Re-exports: data layer ───────────────────────────────────────────────────
// Existing consumers import these from `crate::commands::chart`.

#[allow(unused_imports)]
pub(crate) use crate::chart::data::{
    Candle, ChartResponse, CprData, EmaRibbon, Indicators, RsiEntry, SeriesPoint, VolumeEntry,
    enrich_cpr_with_sr_levels, fetch_days_for_tf,
};

// ─── Re-exports: render layer ─────────────────────────────────────────────────
// TUI widgets and `tui.rs` import these from `crate::commands::chart`.

pub(crate) use crate::chart::render_term::{
    ansi_to_ratatui_text, fmt_price_short, pane_slice, render_line_sized, render_rsi_to_buf,
    render_sessions_to_buf, render_sextant_candles_sized, render_volume_to_buf, render_x_axis_2col,
    subcell_count_bounds,
};

// ─── Entry point (re-exported from app) ──────────────────────────────────────

pub use crate::chart::app::run;

// ─── Candle-at-column hit-test ────────────────────────────────────────────────

/// Given the chart's on-screen layout and a screen column, return the index
/// (into `data.ohlcv`) of the candle visually at that column. Used by the
/// mouse crosshair → OHLCV tooltip path. Returns None if the column is
/// outside the chart canvas, in the y-label gutter, or past the rightmost
/// rendered candle.
pub(crate) fn candle_at_screen_col(
    data: &ChartResponse,
    chart_cols: usize,
    area_x: u16,
    visible_bars: u32,
    offset_from_right: usize,
    screen_col: u16,
) -> Option<usize> {
    let canvas_cols = chart_cols.saturating_sub(Y_LABEL_W);
    let total = data.ohlcv.len();
    if total == 0 || canvas_cols == 0 {
        return None;
    }
    let effective_bars = (visible_bars as usize).min(total / 2 + 1).max(20);
    let candle_width = (canvas_cols / effective_bars).clamp(1, MAX_CANDLE_WIDTH);
    let slice_n = (canvas_cols / candle_width).min(total);
    let end = total.saturating_sub(offset_from_right);
    let start = end.saturating_sub(slice_n);

    let canvas_start = area_x.saturating_add(Y_LABEL_W as u16);
    if screen_col < canvas_start {
        return None;
    }
    let canvas_col = (screen_col - canvas_start) as usize;
    if canvas_col >= canvas_cols {
        return None;
    }
    let ci = canvas_col / candle_width;
    let abs_idx = start + ci;
    if abs_idx >= end {
        return None;
    }
    Some(abs_idx)
}

// ─── Price formatting ─────────────────────────────────────────────────────────
// Shared by render_term, render, and app (build_header).

pub(crate) fn fmt_price(v: f64) -> String {
    let abs = v.abs();
    if abs >= 10_000.0 {
        format!("{v:.0}")
    } else if abs >= 1_000.0 {
        format!("{v:.1}")
    } else if abs >= 100.0 {
        format!("{v:.2}")
    } else if abs >= 1.0 {
        format!("{v:.3}")
    } else {
        format!("{v:.5}")
    }
}
