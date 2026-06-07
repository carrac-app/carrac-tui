/// Terminal (ANSI/block) chart renderers.
///
/// All functions here are pure: they take data structs and return `String` or
/// write directly into a ratatui `Buffer`. No I/O, no network.
use chrono::{DateTime, Utc};

use crate::chart::canvas::{
    BrailleCanvas, Canvas, DotGrid, HLine, OscillatorLine, SextantCanvas, YAxis,
};
use crate::chart::data::{CprData, SessionEntry};
use crate::commands::chart::constants::{MAX_CANDLE_WIDTH, RSI_PERIOD, Y_LABEL_W};
use crate::commands::chart::{Candle, ChartResponse, fmt_price};
use crate::format as f;
use crate::types::positions::Position;

// ─── Visible-window arithmetic ──────────────────────────────────────────────────

/// Visible candle window + per-candle column width for the block renderers.
///
/// Single source of truth for the slice/zoom math: the candle body, the x-axis
/// labels (rendered separately by the embedded `view_chart`), and the volume /
/// RSI sub-panes must all derive their slice identically or they drift out of
/// horizontal alignment. Returns `(start, end, candle_width)` — `start..end`
/// indexes into `data.ohlcv`.
pub(crate) fn visible_candle_slice(
    total: usize,
    canvas_cols: usize,
    visible_bars: u32,
    offset_from_right: usize,
) -> (usize, usize, usize) {
    let effective_bars = (visible_bars as usize).min(total / 2 + 1).max(20);
    let candle_width = (canvas_cols / effective_bars).clamp(1, MAX_CANDLE_WIDTH);
    let slice_n = (canvas_cols / candle_width).min(total);
    let end = total.saturating_sub(offset_from_right);
    let start = end.saturating_sub(slice_n);
    (start, end, candle_width)
}

/// Slice + per-candle column pitch shared by ALL panes (candles, volume, RSI,
/// x-axis) so the same bars line up in the same columns across the whole stack.
///
/// `subcell = true` (the count-driven braille/sextant styles) fits
/// `visible_bars` candles across the full canvas width and lets the body
/// auto-size to the resulting pitch (clamped so a candle is 1–10 cols wide). So
/// fewer bars ⇒ wider candles, more bars ⇒ thinner — `[`/`]` act as a
/// horizontal zoom. `false` uses the block-candle geometry. Returns
/// `(start, end, candle_width)`.
pub(crate) fn pane_slice(
    total: usize,
    canvas_cols: usize,
    visible_bars: u32,
    offset_from_right: usize,
    subcell: bool,
) -> (usize, usize, usize) {
    if subcell {
        let min_pitch = subcell_min_pitch();
        let max_pitch = subcell_max_pitch();
        let max_count = (canvas_cols / min_pitch).max(1);
        let min_count = (canvas_cols / max_pitch).max(1);
        let count = (visible_bars as usize)
            .clamp(min_count, max_count)
            .min(total)
            .max(1);
        let pitch = (canvas_cols / count).clamp(min_pitch, max_pitch);
        let end = total.saturating_sub(offset_from_right);
        let start = end.saturating_sub(count);
        (start, end, pitch)
    } else {
        visible_candle_slice(total, canvas_cols, visible_bars, offset_from_right)
    }
}

// ─── Price / volume formatting ────────────────────────────────────────────────

pub(crate) fn fmt_price_short(v: f64) -> String {
    let abs = v.abs();
    if abs >= 100_000.0 {
        format!("{:.0}k", v / 1_000.0)
    } else if abs >= 10_000.0 {
        format!("{:.1}k", v / 1_000.0)
    } else if abs >= 1_000.0 {
        format!("{:.2}k", v / 1_000.0)
    } else if abs >= 1.0 {
        format!("{:.2}", v)
    } else {
        format!("{:.4}", v)
    }
}

pub(crate) fn fmt_volume(v: f64) -> String {
    if v >= 1_000_000_000.0 {
        format!("{:.1}B", v / 1_000_000_000.0)
    } else if v >= 1_000_000.0 {
        format!("{:.1}M", v / 1_000_000.0)
    } else if v >= 1_000.0 {
        format!("{:.1}K", v / 1_000.0)
    } else {
        format!("{:.0}", v)
    }
}

// ─── EMA timestamp-tolerance helpers ─────────────────────────────────────────

/// Return the typical candle interval (seconds) from OHLCV data.
pub(crate) fn ohlcv_interval(data: &ChartResponse) -> i64 {
    if data.ohlcv.len() >= 2 {
        let dt = data.ohlcv[1].time - data.ohlcv[0].time;
        if dt > 0 {
            return dt;
        }
    }
    match data.timeframe.to_uppercase().as_str() {
        "1M" | "1MIN" => 60,
        "5M" | "5MIN" => 300,
        "15M" | "15MIN" => 900,
        "1H" => 3_600,
        "4H" => 14_400,
        "1D" => 86_400,
        "1W" => 604_800,
        _ => 14_400,
    }
}

/// Look up an EMA value for candle timestamp `t` with nearest-neighbor matching
/// within a ±`dt/2` tolerance window.
pub(crate) fn ema_lookup_tolerant(
    ema_map: &std::collections::HashMap<i64, f64>,
    sorted_ema: &[(i64, f64)],
    t: i64,
    dt: i64,
) -> Option<f64> {
    if let Some(&v) = ema_map.get(&t) {
        return Some(v);
    }
    let tolerance = dt / 2;
    if sorted_ema.is_empty() || tolerance <= 0 {
        return None;
    }
    let idx = sorted_ema.partition_point(|(ts, _)| *ts < t);
    let mut best: Option<(i64, f64)> = None;
    for &i in &[idx.wrapping_sub(1), idx] {
        if let Some(&(ts, v)) = sorted_ema.get(i) {
            let dist = (ts - t).abs();
            if dist <= tolerance && best.is_none_or(|(bd, _)| dist < bd) {
                best = Some((dist, v));
            }
        }
    }
    best.map(|(_, v)| v)
}

// ─── Helpers shared by TUI + static renders ──────────────────────────────────

/// One CPR set — bounds the drawn lines to the matching period
/// (day / ISO week / month) and tags the line labels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CprKind {
    Daily,
    Weekly,
    Monthly,
}

impl CprKind {
    /// One-letter label tag: `P·M`, `S2·W`, …
    pub(crate) fn tag(self) -> &'static str {
        match self {
            CprKind::Daily => "D",
            CprKind::Weekly => "W",
            CprKind::Monthly => "M",
        }
    }
}

/// Which CPR set the chart draws. EXPLICIT — the web chart shows whichever
/// sets the user enables, so an invisible timeframe-based auto-pick made the
/// two screens disagree on "the" CPR levels. `c` cycles the variants.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CprSelect {
    Off,
    /// Default: matches the web chart's primary (monthly) CPR overlay.
    #[default]
    Monthly,
    Weekly,
    Daily,
}

impl CprSelect {
    pub(crate) fn kind(self) -> Option<CprKind> {
        match self {
            CprSelect::Off => None,
            CprSelect::Monthly => Some(CprKind::Monthly),
            CprSelect::Weekly => Some(CprKind::Weekly),
            CprSelect::Daily => Some(CprKind::Daily),
        }
    }

    /// `c`-key order: Monthly → Weekly → Daily → Off → Monthly.
    pub fn cycle(self) -> Self {
        match self {
            CprSelect::Monthly => CprSelect::Weekly,
            CprSelect::Weekly => CprSelect::Daily,
            CprSelect::Daily => CprSelect::Off,
            CprSelect::Off => CprSelect::Monthly,
        }
    }

    /// Short status label, e.g. for indicator readouts and toasts.
    pub fn label(self) -> &'static str {
        match self {
            CprSelect::Off => "off",
            CprSelect::Monthly => "monthly",
            CprSelect::Weekly => "weekly",
            CprSelect::Daily => "daily",
        }
    }
}

/// The selected CPR set from the payload, if present.
pub(crate) fn active_cpr_with_kind(
    data: &ChartResponse,
    sel: CprSelect,
) -> Option<(&CprData, CprKind)> {
    let kind = sel.kind()?;
    let set = match kind {
        CprKind::Daily => data.daily_cpr.as_ref(),
        CprKind::Weekly => data.weekly_cpr.as_ref(),
        CprKind::Monthly => data.cpr.as_ref(),
    };
    set.map(|c| (c, kind))
}

pub(crate) fn active_cpr(data: &ChartResponse, sel: CprSelect) -> Option<&CprData> {
    active_cpr_with_kind(data, sel).map(|(c, _)| c)
}

/// Start (UNIX seconds, UTC) of the period the active CPR set applies to:
/// today for the daily set, this ISO week's Monday for the weekly, the current
/// month for the monthly — derived from the newest candle's timestamp. The
/// monthly set prefers the server-sent `periodStart`. Used to bound the drawn
/// CPR lines on the left, like the web chart: a virgin CPR must not be painted
/// across candles that predate its period (it would read as price having
/// traded through levels that did not exist yet).
pub(crate) fn active_cpr_period_start(data: &ChartResponse, sel: CprSelect) -> Option<i64> {
    use chrono::{Datelike, Days};
    let (cpr, kind) = active_cpr_with_kind(data, sel)?;
    let last = data.ohlcv.last()?.time;
    let dt = DateTime::<Utc>::from_timestamp(last, 0)?;
    let date = dt.date_naive();
    let day0 = match kind {
        CprKind::Daily => date,
        CprKind::Weekly => {
            date.checked_sub_days(Days::new(date.weekday().num_days_from_monday() as u64))?
        }
        CprKind::Monthly => {
            if let Some(ps) = cpr.period_start {
                return Some(ps);
            }
            date.with_day(1)?
        }
    };
    Some(day0.and_hms_opt(0, 0, 0)?.and_utc().timestamp())
}

pub(crate) fn collect_extra_levels(
    extra: &mut Vec<f64>,
    pos: Option<&Position>,
    cpr_sel: CprSelect,
    data: &ChartResponse,
) {
    if let Some(p) = pos {
        if let Some(v) = p.entry_price {
            extra.push(v);
        }
        if let Some(v) = p.stop_price {
            extra.push(v);
        }
        if let Some(v) = p.target_price {
            extra.push(v);
        }
    }
    if let Some(cpr) = active_cpr(data, cpr_sel) {
        if let Some(v) = cpr.pivot {
            extra.push(v);
        }
        if let Some(v) = cpr.tc {
            extra.push(v);
        }
        if let Some(v) = cpr.bc {
            extra.push(v);
        }
    }
}

pub(crate) fn collect_hlines(
    pos: Option<&Position>,
    cpr_sel: CprSelect,
    data: &ChartResponse,
) -> Vec<HLine> {
    let (mut hlines, live) = collect_hlines_split(pos, cpr_sel, data);
    if let Some(hl) = live {
        hlines.push(hl);
    }
    hlines
}

/// Split hlines into (regular lines, live-price line).
pub(crate) fn collect_hlines_split(
    pos: Option<&Position>,
    cpr_sel: CprSelect,
    data: &ChartResponse,
) -> (Vec<HLine>, Option<HLine>) {
    let mut hlines = Vec::new();
    if let Some((cpr, kind)) = active_cpr_with_kind(data, cpr_sel) {
        // Bound every CPR line to its period start so virgin levels are not
        // drawn across candles that predate them; tag labels with the set
        // (P·M / P·W / P·D) so it is never ambiguous which CPR is shown.
        let cpr_start = active_cpr_period_start(data, cpr_sel);
        let set = kind.tag();
        let mut push = |v: Option<f64>, tag: &str, color: &'static str| {
            if let Some(v) = v {
                hlines.push(HLine {
                    price: v,
                    label: format!("{}\u{B7}{} {}", tag, set, fmt_price(v)),
                    ansi_color: color,
                    dashed: true,
                    start_time: cpr_start,
                });
            }
        };
        push(cpr.pivot, "P", "35");
        push(cpr.tc, "TC", "35");
        push(cpr.bc, "BC", "35");
        push(cpr.s1, "S1", "31");
        push(cpr.s2, "S2", "31");
        push(cpr.s3, "S3", "31");
        push(cpr.r1, "R1", "32");
        push(cpr.r2, "R2", "32");
        push(cpr.r3, "R3", "32");
    }

    let has_live_price = pos.and_then(|p| p.current_price).is_some();

    if let Some(p) = pos {
        if let Some(v) = p.stop_price {
            hlines.push(HLine {
                price: v,
                label: format!("SL {}", fmt_price(v)),
                ansi_color: "31",
                dashed: false,
                start_time: None,
            });
        }
        if let Some(v) = p.entry_price {
            hlines.push(HLine {
                price: v,
                label: format!("EN {}", fmt_price(v)),
                ansi_color: "34",
                dashed: false,
                start_time: None,
            });
        }
        if let Some(v) = p.target_price {
            hlines.push(HLine {
                price: v,
                label: format!("TP {}", fmt_price(v)),
                ansi_color: "32",
                dashed: false,
                start_time: None,
            });
        }
        if let Some(v) = p.current_price {
            hlines.push(HLine {
                price: v,
                label: format!(">> {}", fmt_price(v)),
                ansi_color: "35",
                dashed: false,
                start_time: None,
            });
        }
    }

    let live_hline = if !has_live_price {
        data.ohlcv.last().map(|last| {
            let is_bull_last = last.close >= last.open;
            let price_color = if is_bull_last { "32" } else { "31" };
            HLine {
                price: last.close,
                label: format!("<< {}", fmt_price(last.close)),
                ansi_color: price_color,
                dashed: false,
                start_time: None,
            }
        })
    } else {
        None
    };

    (hlines, live_hline)
}

// ─── Draw helpers ─────────────────────────────────────────────────────────────

// ─── Sub-cell candle geometry ─────────────────────────────────────────────────
//
// Sub-cell candles (braille, sextant) are COUNT-driven: the chart fits
// `visible_bars` candles into the full canvas width, and the body auto-sizes to
// the column pitch leaving a fixed `SUBCELL_GAP_COLS` gap. So `[` / `]` (which
// change visible_bars) zoom horizontally — fewer candles = wider, more = thinner
// — and the chart always fills the width without an awkward half-empty gap.
// Bodies are cell-aligned (an even number of sub-cell dot-columns starting on a
// cell boundary) so each cell belongs to exactly one candle — that's what lets
// each candle keep its own red/green color despite the one-color-per-cell limit.
pub(crate) const SUBCELL_WICK_COLS: usize = 1;
pub(crate) const SUBCELL_GAP_COLS: usize = 1;
pub(crate) const SUBCELL_BODY_MIN: usize = 1;
pub(crate) const SUBCELL_BODY_MAX: usize = 10;

fn subcell_min_pitch() -> usize {
    SUBCELL_BODY_MIN + SUBCELL_GAP_COLS
}
fn subcell_max_pitch() -> usize {
    SUBCELL_BODY_MAX + SUBCELL_GAP_COLS
}

/// Effective `[min, max]` candle count the sub-cell styles can actually render
/// at this canvas width — the same law `pane_slice` applies internally. The
/// TUI clamps the `[`/`]` accumulator to this range so every keypress lands in
/// the visible range instead of drifting through values the renderer clamps
/// away invisibly (the "press `[` five times before anything changes" bug).
pub(crate) fn subcell_count_bounds(canvas_cols: usize) -> (usize, usize) {
    let min_count = (canvas_cols / subcell_max_pitch()).max(1);
    let max_count = (canvas_cols / subcell_min_pitch()).max(1);
    (min_count, max_count)
}

/// Body width (terminal cols) implied by a column pitch — always ODD.
/// Even widths have no center CELL: the wick's ▏ ink sat on the cell
/// boundary while ┼ junctions and doji crosses center in their cell, so the
/// vertical jogged half a cell at every hline crossing ("broken" zigzag).
/// Odd bodies give every candle a true middle cell; one cell goes to the
/// gap instead. Volume/RSI bars and EMA dots share this width, so all the
/// per-candle art stays aligned.
pub(crate) fn subcell_body(pitch: usize) -> usize {
    let b = pitch.saturating_sub(SUBCELL_GAP_COLS).max(1);
    if b > 1 && b.is_multiple_of(2) {
        b - 1
    } else {
        b
    }
}

/// Left dot-column of candle `i`.
fn subcell_body_base(i: usize, pitch: usize) -> usize {
    i * pitch * 2
}
/// Center dot-column of a candle (where the wick + EMA dot sit).
fn subcell_center_dot(base: usize, pitch: usize) -> usize {
    base + subcell_body(pitch)
}

pub(crate) fn draw_candles<G: DotGrid>(
    grid: &mut G,
    candles: &[Candle],
    yax: &YAxis,
    pitch: usize,
) {
    let sub = grid.sub_rows();
    let body = subcell_body(pitch);
    let body_dots = body * 2;
    let wick_dots = SUBCELL_WICK_COLS * 2;
    let wick_off = body_dots.saturating_sub(wick_dots) / 2;
    for (i, c) in candles.iter().enumerate() {
        let base = subcell_body_base(i, pitch);
        let color: &'static str = if c.close >= c.open { "32" } else { "31" };
        let high_row = yax.to_subrow(c.high, sub);
        let low_row = yax.to_subrow(c.low, sub);
        // Fully off-range candles (y-zoom/pan moved them off-screen) are
        // skipped — clamping squashed them into fake doji dashes pinned to
        // the chart edge.
        if c.low > yax.max || c.high < yax.min {
            continue;
        }
        // Body FIRST: the wick pass needs to know which cells the body owns.
        // Sextant bodies are cell-quantized rectangles (sharp edges); braille
        // keeps exact sub-row fill via the trait default.
        let body_visible = c.open.min(c.close) <= yax.max && c.open.max(c.close) >= yax.min;
        let body_top_row = yax.to_subrow(c.open.max(c.close), sub); // smaller row = higher price
        let body_bot_row = yax.to_subrow(c.open.min(c.close), sub);
        if body_visible {
            grid.body_span(base..base + body_dots, body_top_row, body_bot_row, color);
        }
        // Wick: centered span, full high→low. Braille keeps dot wicks; the
        // sextant canvas draws a thin │ through empty cells with ╵/╷ tips.
        // Bodies are always odd-width (subcell_body), so │ sits in the true
        // middle cell and lines up exactly with ┼ junctions + doji crosses.
        grid.wick_span(
            base + wick_off..base + wick_off + wick_dots,
            high_row,
            low_row,
            color,
            '│',
        );
    }
}

/// The classic `e` overlay: the server's EMA21 series as one yellow dot per
/// candle (sub-cell precise) at the candle center.
pub(crate) fn draw_ema_dots<G: DotGrid>(
    grid: &mut G,
    candles: &[Candle],
    data: &ChartResponse,
    yax: &YAxis,
    pitch: usize,
) {
    let raw_pts: Vec<(i64, f64)> = data
        .indicators
        .as_ref()
        .and_then(|ind| ind.ema_ribbon.as_ref())
        .and_then(|r| r.fast.as_ref())
        .map(|pts| pts.iter().map(|p| (p.time, p.value)).collect())
        .unwrap_or_default();

    let ema_map: std::collections::HashMap<i64, f64> = raw_pts.iter().copied().collect();
    let mut sorted_ema = raw_pts;
    sorted_ema.sort_unstable_by_key(|(t, _)| *t);

    let sub = grid.sub_rows();
    let dt = ohlcv_interval(data);
    for (i, c) in candles.iter().enumerate() {
        if let Some(ema_val) = ema_lookup_tolerant(&ema_map, &sorted_ema, c.time, dt) {
            let bx = subcell_center_dot(subcell_body_base(i, pitch), pitch);
            let by = yax.to_subrow(ema_val, sub);
            grid.set_indicator_dot(bx, by, "33");
        }
    }
}

/// Wilder-smoothed EMA over closes. `None` until the period warms up.
pub(crate) fn ema_series(candles: &[Candle], period: usize) -> Vec<Option<f64>> {
    let mut out = vec![None; candles.len()];
    if candles.len() < period || period == 0 {
        return out;
    }
    let k = 2.0 / (period as f64 + 1.0);
    let seed: f64 = candles[..period].iter().map(|c| c.close).sum::<f64>() / period as f64;
    let mut ema = seed;
    out[period - 1] = Some(ema);
    for i in period..candles.len() {
        ema = candles[i].close * k + ema * (1.0 - k);
        out[i] = Some(ema);
    }
    out
}

/// Wilder ATR. `None` until warmed up.
pub(crate) fn atr_series(candles: &[Candle], period: usize) -> Vec<Option<f64>> {
    let mut out = vec![None; candles.len()];
    if candles.len() <= period || period == 0 {
        return out;
    }
    let tr = |i: usize| -> f64 {
        let c = &candles[i];
        if i == 0 {
            return c.high - c.low;
        }
        let pc = candles[i - 1].close;
        (c.high - c.low)
            .max((c.high - pc).abs())
            .max((c.low - pc).abs())
    };
    let mut atr: f64 = (0..period).map(tr).sum::<f64>() / period as f64;
    out[period - 1] = Some(atr);
    // `i` indexes `out` and feeds `tr(i)` (true-range at bar i) — not a plain map.
    #[allow(clippy::needless_range_loop)]
    for i in period..candles.len() {
        atr = (atr * (period as f64 - 1.0) + tr(i)) / period as f64;
        out[i] = Some(atr);
    }
    out
}

/// Web-parity EMA Ribbon: EMA30 + EMA60 polylines, per-bar trend coloring
/// (fast-slow vs ±ATR60×0.30 → bull green / bear orange / neutral yellow),
/// drawn as thin ─ ╱ ╲ chars via `DotGrid::set_line_char` so candles always
/// win the cell. Series are computed over the FULL payload (warmup uses
/// history left of the visible slice `start..end`).
pub(crate) fn draw_ema_ribbon<G: DotGrid>(
    grid: &mut G,
    data: &ChartResponse,
    start: usize,
    end: usize,
    yax: &YAxis,
    pitch: usize,
) {
    const FAST: usize = 30;
    const SLOW: usize = 60;
    const ATR_LEN: usize = 60;
    const ATR_MULT: f64 = 0.30;

    let all = &data.ohlcv;
    let fast = ema_series(all, FAST);
    let slow = ema_series(all, SLOW);
    let atr = atr_series(all, ATR_LEN);

    // Trend palette per bar — web chart parity, in 256-color
    // approximations of the web hexes so the ribbon reads distinctly from
    // candle green/red and the EMA21 dots.
    //   bull    #7cb518 → 112
    //   bear    #fb6107 → 208
    //   neutral #f3de2c → 220
    let color_at = |i: usize| -> &'static str {
        match (fast[i], slow[i], atr[i]) {
            (Some(f), Some(sl), Some(a)) => {
                let diff = f - sl;
                let thr = a * ATR_MULT;
                if diff > thr {
                    "38;5;112"
                } else if diff < -thr {
                    "38;5;208"
                } else {
                    "38;5;220"
                }
            }
            _ => "38;5;220",
        }
    };

    // Two dotted polylines: per DOT column the two EMA values are linearly
    // interpolated between candle centers, one indicator dot each. No band
    // fill between the lines — in cell resolution it read as noise on top of
    // the candles. Everything stays behind the candles.
    let sub = grid.sub_rows();
    let mut prev: Option<(usize, f64, f64)> = None; // (dot_col, fast, slow)
    for (vis, i) in (start..end).enumerate() {
        let (Some(fv), Some(sv)) = (fast[i], slow[i]) else {
            prev = None;
            continue;
        };
        let dx = subcell_center_dot(subcell_body_base(vis, pitch), pitch);
        let line_c = color_at(i);
        let mut column = |x: usize, f_val: f64, s_val: f64| {
            for v in [f_val, s_val] {
                if v >= yax.min && v <= yax.max {
                    grid.set_indicator_dot(x, yax.to_subrow(v, sub), line_c);
                }
            }
        };
        if let Some((px, pf, ps)) = prev {
            let span = dx.saturating_sub(px).max(1);
            for step in 1..=span {
                let x = px + step;
                let t = step as f64 / span as f64;
                column(x, pf + (fv - pf) * t, ps + (sv - ps) * t);
            }
        } else {
            column(dx, fv, sv);
        }
        prev = Some((dx, fv, sv));
    }
}

/// Draw a continuous close-price line spanning the full canvas width into a
/// braille `Canvas`. Each braille dot-column samples the closes by linear
/// interpolation, and consecutive samples are joined vertically — so the line
/// stays continuous and uses the whole width no matter how few candles map
/// onto how many columns. (The old `bx = i` mapping crammed every point into
/// the leftmost columns.)
pub(crate) fn draw_close_line(
    canvas: &mut Canvas,
    candles: &[Candle],
    yax: &YAxis,
    canvas_cols: usize,
) {
    let n = candles.len();
    if n == 0 {
        return;
    }
    if n == 1 {
        canvas.set_dot(0, yax.to_brow(candles[0].close), "36");
        return;
    }
    let dot_w = (canvas_cols * 2).max(1);
    let mut prev_brow: Option<usize> = None;
    for x in 0..dot_w {
        let pos = x as f64 * (n - 1) as f64 / (dot_w - 1).max(1) as f64;
        let i0 = (pos.floor() as usize).min(n - 1);
        let i1 = (i0 + 1).min(n - 1);
        let frac = pos - i0 as f64;
        let close = candles[i0].close * (1.0 - frac) + candles[i1].close * frac;
        let by = yax.to_brow(close);
        let color: &'static str = if candles[i1].close >= candles[i0].close {
            "32"
        } else {
            "31"
        };
        if let Some(pb) = prev_brow {
            canvas.vline(x, pb.min(by), pb.max(by), color);
        } else {
            canvas.set_dot(x, by, color);
        }
        prev_brow = Some(by);
    }
}

/// Resolve each hline's `start_time` to a canvas CELL column for the visible
/// slice. `None` (and starts at/before the window) → 0 = full width; a start
/// past the last visible candle hides the line entirely (== `canvas_cols`).
pub(crate) fn hline_start_cols(
    hlines: &[HLine],
    candles: &[Candle],
    col_of_vis: impl Fn(usize) -> usize,
    canvas_cols: usize,
) -> Vec<usize> {
    hlines
        .iter()
        .map(|hl| match hl.start_time {
            None => 0,
            Some(t) => candles
                .iter()
                .position(|c| c.time >= t)
                .map(|vis| {
                    if vis == 0 {
                        0
                    } else {
                        col_of_vis(vis).min(canvas_cols)
                    }
                })
                .unwrap_or(canvas_cols),
        })
        .collect()
}

pub(crate) fn build_chart_rows<G: DotGrid>(
    grid: &G,
    hlines: &[HLine],
    start_cols: &[usize],
    yax: &YAxis,
    canvas_cols: usize,
    chart_rows: usize,
    y_label_w: usize,
) -> String {
    // Row → (line, first column it is drawn from). Missing start_cols entries
    // default to 0 (full width) so callers without time-bounded lines can pass
    // an empty slice.
    let mut hline_at_row: std::collections::HashMap<usize, (&HLine, usize)> =
        std::collections::HashMap::new();
    for (i, hl) in hlines.iter().enumerate() {
        if hl.price < yax.min || hl.price > yax.max {
            continue;
        }
        let start = start_cols.get(i).copied().unwrap_or(0);
        if start >= canvas_cols {
            continue; // period starts past the visible window — nothing to draw
        }
        let row = yax.to_row(hl.price);
        hline_at_row.insert(row, (hl, start));
    }

    let price_step = (yax.max - yax.min) / chart_rows as f64;
    let color_on = f::color_on_pub();
    let mut out = String::new();

    for row in 0..chart_rows {
        let row_price = yax.max - row as f64 * price_step;
        let label = if row % 4 == 0 {
            format!("{:>9} ", fmt_price_short(row_price))
        } else {
            " ".repeat(y_label_w)
        };
        out.push_str(&f::dim(&label));

        // Composite each column: a candle dot (if any) wins over the reference
        // line, so candles stay in front of CPR / live-price lines instead of
        // being painted over.
        let row_line = hline_at_row.get(&row).copied();
        for col in 0..canvas_cols {
            // The line only exists from its start column (period-bounded CPR);
            // left of that the column renders as if there were no hline.
            let hl = row_line.filter(|&(_, start)| col >= start).map(|(h, _)| h);
            // Layering: candles > hlines > indicator dots. A ribbon dot on an
            // hline row yields to the line — otherwise the ribbon shredded the
            // CPR/live-price lines into invisibility.
            let cell = if hl.is_some() && grid.is_indicator_cell(col, row) {
                None
            } else {
                grid.cell(col, row)
            };
            if let Some((ch, color)) = cell {
                // Wicks crossing an hline stay plain │/╵/╷ — the line reads
                // as passing BEHIND the candle (symmetric half-cell gaps,
                // since odd-width bodies center the wick). Junction glyphs
                // (┼/┴/┬) painted candle-colored horizontal ink into the
                // line, which read as a broken line segment.
                if color_on && !color.is_empty() {
                    out.push_str(&format!("\x1b[{}m{}\x1b[0m", color, ch));
                } else {
                    out.push(ch);
                }
            } else if let Some(hl) = hl {
                let ch = if hl.dashed { '╌' } else { '─' };
                if color_on {
                    out.push_str(&format!("\x1b[{}m{}\x1b[0m", hl.ansi_color, ch));
                } else {
                    out.push(ch);
                }
            } else {
                out.push(' ');
            }
        }

        if let Some((hl, _)) = row_line {
            if color_on {
                out.push_str(&format!(" \x1b[{}m{}\x1b[0m", hl.ansi_color, &hl.label));
            } else {
                out.push(' ');
                out.push_str(&hl.label);
            }
        }
        out.push('\n');
    }

    out
}

// ─── X-axis label formatting ──────────────────────────────────────────────────

pub(crate) fn x_axis_label(tf: &str, dt: DateTime<Utc>, prev_dt: Option<DateTime<Utc>>) -> String {
    let tf_upper = tf.to_uppercase();
    match tf_upper.as_str() {
        "1M" | "5M" | "15M" => {
            let new_day = prev_dt
                .map(|p| p.date_naive() != dt.date_naive())
                .unwrap_or(false);
            if new_day {
                dt.format("%m-%d %H:%M").to_string()
            } else {
                dt.format("%H:%M").to_string()
            }
        }
        "1H" | "4H" => {
            let new_day = prev_dt
                .map(|p| p.date_naive() != dt.date_naive())
                .unwrap_or(false);
            if new_day {
                dt.format("%m-%d").to_string()
            } else {
                dt.format("%H:00").to_string()
            }
        }
        _ => dt.format("%m-%d").to_string(),
    }
}

pub(crate) fn render_x_axis_2col(
    candles: &[Candle],
    canvas_cols: usize,
    candle_width: usize,
    y_label_w: usize,
    data: &ChartResponse,
) -> String {
    if candles.is_empty() {
        return String::new();
    }

    let tf = data.timeframe.as_str();
    let n_candles = candles.len();
    let tick_every_candles = (n_candles / 4).max(5);

    let mut ticks: Vec<(usize, String)> = Vec::new();
    let mut ci = 0usize;
    let mut prev_dt: Option<DateTime<Utc>> = None;
    while ci < n_candles {
        let col_idx = ci * candle_width;
        if col_idx >= canvas_cols {
            break;
        }

        let ts = candles[ci].time;
        #[allow(deprecated)]
        let dt = DateTime::<Utc>::from_timestamp(ts, 0).unwrap_or_default();

        let label = x_axis_label(tf, dt, prev_dt);
        ticks.push((col_idx, label));
        prev_dt = Some(dt);
        ci += tick_every_candles;
    }

    let mut row: Vec<char> = vec![' '; canvas_cols];
    let mut reserved_until = 0usize;

    for (col, label) in &ticks {
        let col = *col;
        if col < reserved_until {
            continue;
        }
        row[col] = '|';
        let label_start = col + 1;
        let label_end = (label_start + label.len()).min(canvas_cols);
        for (i, ch) in label.chars().enumerate() {
            let target = label_start + i;
            if target >= canvas_cols {
                break;
            }
            row[target] = ch;
        }
        reserved_until = label_end;
    }

    let mut out: String = " ".repeat(y_label_w);
    out.extend(row.iter());
    out.push('\n');
    out
}

pub(crate) fn render_x_axis(
    candles: &[Candle],
    canvas_cols: usize,
    y_label_w: usize,
    data: &ChartResponse,
) -> String {
    if candles.is_empty() {
        return String::new();
    }

    let tf = data.timeframe.as_str();
    let tick_every = (canvas_cols / 4).max(10);
    let n_candles = candles.len();

    let mut axis = vec![' '; canvas_cols];
    let mut labels: Vec<(usize, String)> = Vec::new();

    let mut col = 0usize;
    let mut prev_dt: Option<DateTime<Utc>> = None;
    while col < canvas_cols {
        let ci = col.min(n_candles - 1);
        let ts = candles[ci].time;
        #[allow(deprecated)]
        let dt = DateTime::<Utc>::from_timestamp(ts, 0).unwrap_or_default();

        let label = x_axis_label(tf, dt, prev_dt);

        if col + label.len() <= canvas_cols {
            labels.push((col, label));
        }
        axis[col] = '|';
        prev_dt = Some(dt);
        col += tick_every;
    }

    let mut axis_str: String = " ".repeat(y_label_w);
    axis_str.extend(axis.iter());
    axis_str.push('\n');

    let mut label_row: String = " ".repeat(y_label_w);
    let mut cur = 0usize;
    for (pos, label) in labels {
        if pos >= cur {
            let pad = pos - cur;
            label_row.extend(std::iter::repeat_n(' ', pad));
            label_row.push_str(&label);
            cur = pos + label.len();
        }
    }
    axis_str.push_str(&label_row);
    axis_str.push('\n');
    axis_str
}

// ─── Sub-cell candle renderers (fixed-size, used by TUI) ─────────────────────

/// Build the chart body for a sub-cell style. Candles fill the full width at
/// the count-driven pitch from `pane_slice` (so volume/RSI/x-axis line up).
#[allow(clippy::too_many_arguments)] // chart render fn; a params struct adds churn without clarity
fn render_subcell_candles_sized(
    data: &ChartResponse,
    chart_cols: usize,
    chart_rows: usize,
    cpr_sel: CprSelect,
    show_ema: bool,
    show_ema_ribbon: bool,
    pos: Option<&Position>,
    visible_bars: u32,
    offset_from_right: usize,
    y_zoom: f32,
    y_offset: f32,
) -> String {
    let canvas_cols = chart_cols.saturating_sub(Y_LABEL_W);

    let mut extra_levels: Vec<f64> = Vec::new();
    collect_extra_levels(&mut extra_levels, pos, cpr_sel, data);

    let total = data.ohlcv.len();
    let (start, end, pitch) = pane_slice(total, canvas_cols, visible_bars, offset_from_right, true);
    let candles: &[Candle] = &data.ohlcv[start..end];

    if candles.is_empty() {
        return "  (no data)\n".to_string();
    }

    let yax = YAxis::new_zoomed(candles, &extra_levels, chart_rows, y_zoom, y_offset);
    let hlines = collect_hlines(pos, cpr_sel, data);

    let mut g = SextantCanvas::new(canvas_cols, chart_rows);
    draw_candles(&mut g, candles, &yax, pitch);
    if show_ema_ribbon {
        // Web-parity EMA Ribbon (EMA30/EMA60, ATR60×0.30 trend coloring),
        // drawn as a fine dotted curve behind the candles ('E').
        draw_ema_ribbon(&mut g, data, start, end, &yax, pitch);
    }
    if show_ema {
        // The classic EMA21 dot overlay ('e').
        draw_ema_dots(&mut g, candles, data, &yax, pitch);
    }
    // Sextant cells are 2 dot-columns wide; a candle's cell column is its
    // body-base dot halved.
    let start_cols = hline_start_cols(
        &hlines,
        candles,
        |vis| subcell_body_base(vis, pitch) / 2,
        canvas_cols,
    );
    let chart = build_chart_rows(
        &g,
        &hlines,
        &start_cols,
        &yax,
        canvas_cols,
        chart_rows,
        Y_LABEL_W,
    );

    chart + &render_x_axis(candles, canvas_cols, Y_LABEL_W, data)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_sextant_candles_sized(
    data: &ChartResponse,
    chart_cols: usize,
    chart_rows: usize,
    cpr_sel: CprSelect,
    show_ema: bool,
    show_ema_ribbon: bool,
    pos: Option<&Position>,
    visible_bars: u32,
    offset_from_right: usize,
    y_zoom: f32,
    y_offset: f32,
) -> String {
    render_subcell_candles_sized(
        data,
        chart_cols,
        chart_rows,
        cpr_sel,
        show_ema,
        show_ema_ribbon,
        pos,
        visible_bars,
        offset_from_right,
        y_zoom,
        y_offset,
    )
}

#[allow(clippy::too_many_arguments)] // chart render fn; a params struct adds churn without clarity
pub(crate) fn render_line_sized(
    data: &ChartResponse,
    chart_cols: usize,
    chart_rows: usize,
    cpr_sel: CprSelect,
    _show_ema: bool,
    pos: Option<&Position>,
    visible_bars: u32,
    offset_from_right: usize,
    y_zoom: f32,
    y_offset: f32,
) -> String {
    let canvas_cols = chart_cols.saturating_sub(Y_LABEL_W);
    let max_candles = canvas_cols * 2;
    let total = data.ohlcv.len();
    let slice_n = (visible_bars as usize).min(max_candles).min(total);
    let end = total.saturating_sub(offset_from_right);
    let start = end.saturating_sub(slice_n);
    let candles: &[Candle] = &data.ohlcv[start..end];

    if candles.is_empty() {
        return "  (no data)\n".to_string();
    }

    let mut extra_levels: Vec<f64> = Vec::new();
    collect_extra_levels(&mut extra_levels, pos, cpr_sel, data);

    let yax = YAxis::new_zoomed(candles, &extra_levels, chart_rows, y_zoom, y_offset);
    let mut canvas = Canvas::new(canvas_cols, chart_rows);

    draw_close_line(&mut canvas, candles, &yax, canvas_cols);

    let hlines = collect_hlines(pos, cpr_sel, data);

    // Mirror draw_close_line's index→dot-column mapping (braille cells are
    // 2 dot-columns wide).
    let n = candles.len();
    let dot_w = (canvas_cols * 2).max(1);
    let start_cols = hline_start_cols(
        &hlines,
        candles,
        |vis| (vis * (dot_w - 1) / (n - 1).max(1)) / 2,
        canvas_cols,
    );
    build_chart_rows(
        &canvas,
        &hlines,
        &start_cols,
        &yax,
        canvas_cols,
        chart_rows,
        Y_LABEL_W,
    ) + &render_x_axis(candles, canvas_cols, Y_LABEL_W, data)
}

// ─── Block candle renderer ─────────────────────────────────────────────────────

// ─── Volume sub-pane ──────────────────────────────────────────────────────────

// Test-only scaffolding for a not-yet-integrated stacked volume/RSI pane;
// exercised by the unit tests below, not wired into the live render path.
#[cfg(test)]
pub(crate) fn render_volume_pane(
    data: &ChartResponse,
    chart_cols: usize,
    pane_rows: usize,
    visible_bars: u32,
    offset_from_right: usize,
) -> String {
    if pane_rows == 0 {
        return String::new();
    }

    let canvas_cols = chart_cols.saturating_sub(Y_LABEL_W);
    let total = data.ohlcv.len();
    let (start, end, candle_width) =
        visible_candle_slice(total, canvas_cols, visible_bars, offset_from_right);
    let candles: &[Candle] = &data.ohlcv[start..end];

    if candles.is_empty() {
        return String::new();
    }

    let vol_map: std::collections::HashMap<i64, f64> =
        data.volume.iter().map(|v| (v.time, v.value)).collect();

    let volumes: Vec<f64> = candles
        .iter()
        .map(|c| vol_map.get(&c.time).copied().unwrap_or(0.0))
        .collect();

    let max_vol = volumes.iter().copied().fold(0.0_f64, f64::max);
    if max_vol <= 0.0 {
        return String::new();
    }

    let color_enabled = f::color_on_pub();
    let n_candles = candles.len();

    struct BarGeo {
        h_full: usize,
        h_partial_present: bool,
    }
    let bar_geo: Vec<BarGeo> = volumes
        .iter()
        .map(|&v| {
            if v <= 0.0 {
                return BarGeo {
                    h_full: 0,
                    h_partial_present: false,
                };
            }
            let frac = (v / max_vol).clamp(0.0, 1.0);
            let h_sub_f = frac * pane_rows as f64 * 2.0;
            let h_sub = (h_sub_f as usize).min(pane_rows * 2);
            let h_full = h_sub / 2;
            let h_partial_present = (h_sub % 2 == 1) && h_full < pane_rows;
            BarGeo {
                h_full,
                h_partial_present,
            }
        })
        .collect();

    let nonzero_vols: Vec<f64> = volumes.iter().copied().filter(|&v| v > 0.0).collect();
    let avg_vol = if nonzero_vols.is_empty() {
        0.0
    } else {
        let window = nonzero_vols.len().min(20);
        nonzero_vols[nonzero_vols.len() - window..]
            .iter()
            .sum::<f64>()
            / window as f64
    };

    let avg_row: Option<usize> = if avg_vol > 0.0 && avg_vol <= max_vol {
        let frac = (avg_vol / max_vol).clamp(0.0, 1.0);
        let fill_rows = (frac * pane_rows as f64).round() as usize;
        let r = pane_rows.saturating_sub(fill_rows);
        if r < pane_rows { Some(r) } else { None }
    } else {
        None
    };

    let mut out = String::new();

    for row in 0..pane_rows {
        let label = if row == 0 {
            let vol_str = fmt_volume(max_vol);
            format!("  Vol {:>6} ", vol_str)
        } else {
            " ".repeat(Y_LABEL_W)
        };
        let mut row_str = f::dim(&label);

        let is_avg_row = avg_row == Some(row);

        for col_idx in 0..canvas_cols {
            let ci = col_idx / candle_width;
            let is_gap = col_idx % candle_width != 0;

            if ci >= n_candles || is_gap {
                if is_avg_row && !is_gap {
                    if color_enabled {
                        row_str.push_str("\x1b[93m┄\x1b[0m");
                    } else {
                        row_str.push('┄');
                    }
                } else {
                    row_str.push(' ');
                }
                continue;
            }

            let c = &candles[ci];
            let geo = &bar_geo[ci];

            if volumes[ci] <= 0.0 {
                if is_avg_row {
                    if color_enabled {
                        row_str.push_str("\x1b[93m┄\x1b[0m");
                    } else {
                        row_str.push('┄');
                    }
                } else {
                    row_str.push(' ');
                }
                continue;
            }

            let full_start = pane_rows - geo.h_full;
            let partial_row = if geo.h_partial_present {
                Some(full_start - 1)
            } else {
                None
            };

            let is_bull = c.close >= c.open;
            let color = if is_bull { "32" } else { "31" };

            let glyph_opt: Option<char> = if row >= full_start {
                Some('█')
            } else if partial_row == Some(row) {
                Some('▄')
            } else {
                None
            };

            if let Some(glyph) = glyph_opt {
                if color_enabled {
                    row_str.push_str(&format!("\x1b[{}m{}\x1b[0m", color, glyph));
                } else {
                    row_str.push(glyph);
                }
            } else if is_avg_row {
                if color_enabled {
                    row_str.push_str("\x1b[93m┄\x1b[0m");
                } else {
                    row_str.push('┄');
                }
            } else {
                row_str.push(' ');
            }
        }

        if is_avg_row {
            let avg_label = format!(" avg {}", fmt_volume(avg_vol));
            if color_enabled {
                row_str.push_str(&format!("\x1b[93m{}\x1b[0m", avg_label));
            } else {
                row_str.push_str(&avg_label);
            }
        }

        if color_enabled {
            row_str.push_str("\x1b[0m");
        }

        out.push_str(&row_str);
        out.push('\n');
    }

    out
}

// ─── RSI sub-pane ──────────────────────────────────────────────────────────────

pub(crate) fn compute_rsi14(candles: &[Candle]) -> Vec<Option<f64>> {
    let period = RSI_PERIOD;
    let n = candles.len();
    let mut result = vec![None; n];

    if n < period + 1 {
        return result;
    }

    let mut avg_gain = 0.0_f64;
    let mut avg_loss = 0.0_f64;
    for i in 1..=period {
        let diff = candles[i].close - candles[i - 1].close;
        if diff > 0.0 {
            avg_gain += diff;
        } else {
            avg_loss += diff.abs();
        }
    }
    avg_gain /= period as f64;
    avg_loss /= period as f64;

    let rs = if avg_loss == 0.0 {
        f64::INFINITY
    } else {
        avg_gain / avg_loss
    };
    result[period] = Some(100.0 - 100.0 / (1.0 + rs));

    for i in (period + 1)..n {
        let diff = candles[i].close - candles[i - 1].close;
        let gain = if diff > 0.0 { diff } else { 0.0 };
        let loss = if diff < 0.0 { diff.abs() } else { 0.0 };
        avg_gain = (avg_gain * (period as f64 - 1.0) + gain) / period as f64;
        avg_loss = (avg_loss * (period as f64 - 1.0) + loss) / period as f64;
        let rs = if avg_loss == 0.0 {
            f64::INFINITY
        } else {
            avg_gain / avg_loss
        };
        result[i] = Some(100.0 - 100.0 / (1.0 + rs));
    }

    result
}

#[cfg(test)]
pub(crate) fn render_rsi_pane(
    data: &ChartResponse,
    chart_cols: usize,
    pane_rows: usize,
    visible_bars: u32,
    offset_from_right: usize,
) -> String {
    if pane_rows == 0 {
        return String::new();
    }

    let canvas_cols = chart_cols.saturating_sub(Y_LABEL_W);
    let total = data.ohlcv.len();
    let (start, end, candle_width) =
        visible_candle_slice(total, canvas_cols, visible_bars, offset_from_right);
    let candles: &[Candle] = &data.ohlcv[start..end];

    if candles.is_empty() {
        return String::new();
    }

    let rsi_vals: Vec<Option<f64>> = if !data.rsi.is_empty() {
        let rsi_map: std::collections::HashMap<i64, f64> =
            data.rsi.iter().map(|r| (r.time, r.value)).collect();
        let mapped: Vec<Option<f64>> = candles
            .iter()
            .map(|c| rsi_map.get(&c.time).copied())
            .collect();
        let hit_count = mapped.iter().filter(|v| v.is_some()).count();
        if hit_count * 2 >= candles.len() {
            mapped
        } else {
            let full_rsi = compute_rsi14(&data.ohlcv);
            full_rsi[start..end].to_vec()
        }
    } else {
        let full_rsi = compute_rsi14(&data.ohlcv);
        full_rsi[start..end].to_vec()
    };

    let current_rsi = rsi_vals.iter().rev().find_map(|v| *v);

    let color_enabled = f::color_on_pub();
    let n_candles = candles.len();

    let rsi_color = |rsi: f64| -> &'static str {
        if rsi > 70.0 {
            "91"
        } else if rsi > 50.0 {
            "93"
        } else if rsi >= 30.0 {
            "36"
        } else {
            "92"
        }
    };

    let ref_row = |rsi: f64| -> usize {
        let frac = 1.0 - (rsi / 100.0).clamp(0.0, 1.0);
        ((frac * pane_rows as f64) as usize).min(pane_rows - 1)
    };
    let row_70 = ref_row(70.0);
    let row_50 = ref_row(50.0);
    let row_30 = ref_row(30.0);

    struct RsiBar {
        h_full: usize,
        h_partial_present: bool,
        color: &'static str,
    }
    let bars: Vec<Option<RsiBar>> = rsi_vals
        .iter()
        .map(|&v| {
            v.map(|rsi| {
                let h_sub_f = (rsi / 100.0).clamp(0.0, 1.0) * pane_rows as f64 * 2.0;
                let h_sub = (h_sub_f as usize).min(pane_rows * 2);
                let h_full = h_sub / 2;
                let h_partial_present = (h_sub % 2 == 1) && h_full < pane_rows;
                RsiBar {
                    h_full,
                    h_partial_present,
                    color: rsi_color(rsi),
                }
            })
        })
        .collect();

    let mut out = String::new();
    for row in 0..pane_rows {
        let label = if row == row_70 {
            format!("{:<10}", "RSI")
        } else {
            " ".repeat(Y_LABEL_W)
        };
        let mut row_str = f::dim(&label);

        let is_ref_row = row == row_70 || row == row_50 || row == row_30;

        for col_idx in 0..canvas_cols {
            let ci = col_idx / candle_width;
            let is_gap = col_idx % candle_width != 0;

            if ci >= n_candles || is_gap {
                if !is_gap && is_ref_row {
                    if color_enabled {
                        row_str.push_str("\x1b[38;5;238m╌\x1b[0m");
                    } else {
                        row_str.push('╌');
                    }
                } else {
                    row_str.push(' ');
                }
                continue;
            }

            let bar = &bars[ci];

            match bar {
                None => {
                    if is_ref_row {
                        if color_enabled {
                            row_str.push_str("\x1b[38;5;238m╌\x1b[0m");
                        } else {
                            row_str.push('╌');
                        }
                    } else {
                        row_str.push(' ');
                    }
                }
                Some(b) => {
                    let full_start = pane_rows - b.h_full;
                    let partial_row_opt = if b.h_partial_present {
                        Some(full_start - 1)
                    } else {
                        None
                    };

                    let glyph_opt: Option<char> = if row >= full_start {
                        Some('█')
                    } else if partial_row_opt == Some(row) {
                        Some('▄')
                    } else {
                        None
                    };

                    if let Some(glyph) = glyph_opt {
                        if color_enabled {
                            row_str.push_str(&format!("\x1b[{}m{}\x1b[0m", b.color, glyph));
                        } else {
                            row_str.push(glyph);
                        }
                    } else if is_ref_row {
                        if color_enabled {
                            row_str.push_str("\x1b[38;5;238m╌\x1b[0m");
                        } else {
                            row_str.push('╌');
                        }
                    } else {
                        row_str.push(' ');
                    }
                }
            }
        }

        if row == pane_rows - 1
            && let Some(rsi) = current_rsi
        {
            let label_str = format!(" {:.1}", rsi);
            if color_enabled {
                row_str.push_str(&format!("\x1b[{}m{}\x1b[0m", rsi_color(rsi), label_str));
            } else {
                row_str.push_str(&label_str);
            }
        }

        out.push_str(&row_str);
        out.push('\n');
    }

    out
}

// ─── MACD sub-pane ─────────────────────────────────────────────────────────────

/// Render a MACD sub-pane as an ANSI string (matches the string-based output of
/// `render_volume_pane` / `render_rsi_pane`).
///
/// Layout:
/// - Histogram bars: green (≥ 0) / red (< 0), scaled to pane height with a zero
///   baseline at the vertical midpoint of the value range.
/// - MACD line (cyan) and signal line (yellow) as block-glyph column markers
///   aligned to the same x-columns as the histogram bars.
///
/// Parameters mirror `render_rsi_pane` exactly so integration can slot this in
/// right beside volume/RSI:
///   `data`              — full `ChartResponse` (MACD read from `data.macd`)
///   `chart_cols`        — total terminal width including the Y-label column
///   `pane_rows`         — height of the pane in terminal rows
///   `visible_bars`      — zoom parameter (same as passed to the candle pane)
///   `offset_from_right` — pan offset (same as passed to the candle pane)
pub(crate) fn render_macd_pane(
    data: &ChartResponse,
    chart_cols: usize,
    pane_rows: usize,
    visible_bars: u32,
    offset_from_right: usize,
) -> String {
    if pane_rows == 0 {
        return String::new();
    }

    let canvas_cols = chart_cols.saturating_sub(Y_LABEL_W);
    let total = data.ohlcv.len();
    let (start, end, candle_width) =
        visible_candle_slice(total, canvas_cols, visible_bars, offset_from_right);
    let candles: &[Candle] = &data.ohlcv[start..end];

    if candles.is_empty() {
        return String::new();
    }

    let macd = match &data.macd {
        Some(m) => m,
        None => return String::new(),
    };

    // Build timestamp→value maps for all three MACD series.
    let hist_map: std::collections::HashMap<i64, f64> =
        macd.histogram.iter().map(|p| (p.time, p.value)).collect();
    let line_map: std::collections::HashMap<i64, f64> =
        macd.line.iter().map(|p| (p.time, p.value)).collect();
    let sig_map: std::collections::HashMap<i64, f64> =
        macd.signal.iter().map(|p| (p.time, p.value)).collect();

    // Per-candle values; None = warmup / no data.
    let hist_vals: Vec<Option<f64>> = candles
        .iter()
        .map(|c| hist_map.get(&c.time).copied())
        .collect();
    let line_vals: Vec<Option<f64>> = candles
        .iter()
        .map(|c| line_map.get(&c.time).copied())
        .collect();
    let sig_vals: Vec<Option<f64>> = candles
        .iter()
        .map(|c| sig_map.get(&c.time).copied())
        .collect();

    // Scale: find the symmetric absolute max so the zero baseline sits in the
    // middle of the pane (same approach as histogram oscillators).
    let abs_max = hist_vals
        .iter()
        .filter_map(|v| *v)
        .chain(line_vals.iter().filter_map(|v| *v))
        .chain(sig_vals.iter().filter_map(|v| *v))
        .map(|v| v.abs())
        .fold(0.0_f64, f64::max);

    // If all data is empty / warmup, nothing to draw.
    if abs_max <= 0.0 {
        return String::new();
    }

    let display_min = -abs_max;
    let display_max = abs_max;

    // Map a value into a cell-row index [0, pane_rows) where 0 = top (highest).
    let to_row = |v: f64| -> usize {
        let span = display_max - display_min;
        let frac = 1.0 - ((v - display_min) / span).clamp(0.0, 1.0);
        ((frac * pane_rows as f64) as usize).min(pane_rows - 1)
    };
    let zero_row = to_row(0.0);

    // Precompute bar geometry for histogram (same half-sub-row precision as
    // the volume/RSI panes: each row = 2 sub-rows, partial-row gets ▄/▀).
    struct MacdBar {
        h_full: usize,
        h_partial: bool,
        positive: bool,
    }
    let bars: Vec<Option<MacdBar>> = hist_vals
        .iter()
        .map(|&v| {
            v.map(|hv| {
                let frac = (hv.abs() / abs_max).clamp(0.0, 1.0);
                let h_sub = (frac * pane_rows as f64 * 2.0) as usize;
                let h_sub = h_sub.min(pane_rows * 2);
                let h_full = h_sub / 2;
                let h_partial = (h_sub % 2 == 1) && h_full < pane_rows;
                MacdBar {
                    h_full,
                    h_partial,
                    positive: hv >= 0.0,
                }
            })
        })
        .collect();

    let n_candles = candles.len();

    // Continuous curves: interpolate line/signal across EVERY canvas column
    // (including the gap columns between candle centers) so the pane reads as
    // two solid curves — web parity — rather than per-candle dots.
    let interp_cols = |vals: &[Option<f64>]| -> Vec<Option<f64>> {
        (0..canvas_cols)
            .map(|c| {
                let u = c as f64 / candle_width as f64;
                let i0 = (u.floor() as usize).min(n_candles.saturating_sub(1));
                let frac = u - i0 as f64;
                let i1 = (i0 + 1).min(n_candles.saturating_sub(1));
                match (
                    vals.get(i0).copied().flatten(),
                    vals.get(i1).copied().flatten(),
                ) {
                    (Some(a), Some(b)) => Some(a + (b - a) * frac),
                    (Some(a), _) if frac <= f64::EPSILON => Some(a),
                    _ => None,
                }
            })
            .collect()
    };
    let to_rows = |cvals: &[Option<f64>]| -> Vec<Option<usize>> {
        cvals.iter().map(|v| v.map(to_row)).collect()
    };
    // Slope-aware glyph per column: ─ flat, ╱ rising, ╲ falling (rows grow downward).
    let slope_chars = |rows_v: &[Option<usize>]| -> Vec<char> {
        (0..rows_v.len())
            .map(
                |c| match (rows_v[c], rows_v.get(c + 1).copied().flatten()) {
                    (Some(a), Some(b)) if b > a => '╲',
                    (Some(a), Some(b)) if b < a => '╱',
                    _ => '─',
                },
            )
            .collect()
    };
    let line_col_rows = to_rows(&interp_cols(&line_vals));
    let sig_col_rows = to_rows(&interp_cols(&sig_vals));
    let line_col_chars = slope_chars(&line_col_rows);
    let sig_col_chars = slope_chars(&sig_col_rows);

    let current_line_val = line_vals.iter().rev().find_map(|v| *v);
    let color_enabled = f::color_on_pub();

    let mut out = String::new();
    for row in 0..pane_rows {
        let label = if row == 0 {
            format!("{:<10}", "MACD")
        } else {
            " ".repeat(Y_LABEL_W)
        };
        let mut row_str = f::dim(&label);

        let is_zero_row = row == zero_row;

        for col_idx in 0..canvas_cols {
            let ci = col_idx / candle_width;
            let is_gap = col_idx % candle_width != 0;

            // --- MACD + signal curves: drawn first (over bars, through gaps) ---
            if line_col_rows.get(col_idx).copied().flatten() == Some(row) {
                let ch = line_col_chars[col_idx];
                if color_enabled {
                    row_str.push_str(&format!("\x1b[38;5;81m{ch}\x1b[0m"));
                } else {
                    row_str.push(ch);
                }
                continue;
            }
            if sig_col_rows.get(col_idx).copied().flatten() == Some(row) {
                let ch = sig_col_chars[col_idx];
                if color_enabled {
                    row_str.push_str(&format!("\x1b[38;5;214m{ch}\x1b[0m"));
                } else {
                    row_str.push(ch);
                }
                continue;
            }

            if ci >= n_candles || is_gap {
                if !is_gap && is_zero_row {
                    if color_enabled {
                        row_str.push_str("\x1b[38;5;238m╌\x1b[0m");
                    } else {
                        row_str.push('╌');
                    }
                } else {
                    row_str.push(' ');
                }
                continue;
            }

            // --- Histogram bar ---
            let bar_drawn = if let Some(Some(bar)) = bars.get(ci) {
                let full_start;
                let full_end;
                let partial_row_opt: Option<usize>;

                if bar.positive {
                    // Positive bars grow upward from the zero row.
                    full_end = zero_row;
                    full_start = zero_row.saturating_sub(bar.h_full);
                    partial_row_opt = if bar.h_partial && full_start > 0 {
                        Some(full_start - 1)
                    } else {
                        None
                    };
                } else {
                    // Negative bars grow downward from just below zero row.
                    full_start = zero_row + 1;
                    full_end = (zero_row + bar.h_full).min(pane_rows - 1);
                    partial_row_opt = if bar.h_partial && full_end + 1 < pane_rows {
                        Some(full_end + 1)
                    } else {
                        None
                    };
                }

                let color = if bar.positive { "32" } else { "31" };
                let in_full = bar.h_full > 0 && row >= full_start && row <= full_end;
                let is_partial = partial_row_opt == Some(row);

                let glyph_opt: Option<char> = if in_full {
                    Some('█')
                } else if is_partial {
                    if bar.positive {
                        Some('▄')
                    } else {
                        Some('▀')
                    }
                } else {
                    None
                };

                if let Some(glyph) = glyph_opt {
                    if color_enabled {
                        row_str.push_str(&format!("\x1b[{}m{}\x1b[0m", color, glyph));
                    } else {
                        row_str.push(glyph);
                    }
                    true
                } else {
                    false
                }
            } else {
                false
            };

            if bar_drawn {
                continue;
            }

            // Zero baseline.
            if is_zero_row {
                if color_enabled {
                    row_str.push_str("\x1b[38;5;238m╌\x1b[0m");
                } else {
                    row_str.push('╌');
                }
            } else {
                row_str.push(' ');
            }
        }

        // Append current MACD line value label on the last row.
        if row == pane_rows - 1
            && let Some(v) = current_line_val
        {
            let lbl = format!(" {:.4}", v);
            if color_enabled {
                let color = if v >= 0.0 { "36" } else { "31" };
                row_str.push_str(&format!("\x1b[{}m{}\x1b[0m", color, lbl));
            } else {
                row_str.push_str(&lbl);
            }
        }

        out.push_str(&row_str);
        out.push('\n');
    }

    out
}

/// Render a MACD sub-pane directly into a ratatui `Buffer` (TUI path).
///
/// Mirrors `render_rsi_to_buf` exactly. Histogram bars are drawn as block
/// characters; MACD line (cyan) and signal line (yellow) are drawn as
/// continuous braille polylines via `draw_oscillator_lines`.
///
/// Signature:
///   `data`              — full `ChartResponse`
///   `chart_cols`        — total terminal width (same value passed to candle renderer)
///   `area`              — ratatui Rect for the pane (height = pane_rows)
///   `buf`               — ratatui Buffer to write into
///   `visible_bars`      — zoom (same value as candle pane)
///   `offset_from_right` — pan offset (same value as candle pane)
///   `subcell`           — true for braille/sextant candle styles, false for block
pub(crate) fn render_macd_to_buf(
    data: &ChartResponse,
    chart_cols: usize,
    area: ratatui::layout::Rect,
    buf: &mut ratatui::buffer::Buffer,
    visible_bars: u32,
    offset_from_right: usize,
    subcell: bool,
) {
    use ratatui::style::{Color, Modifier, Style};

    let pane_rows = area.height as usize;
    let pane_width = area.width as usize;
    if pane_rows == 0 || pane_width == 0 {
        return;
    }

    let canvas_cols = chart_cols.saturating_sub(Y_LABEL_W);
    let total = data.ohlcv.len();
    let (start, end, candle_width) =
        pane_slice(total, canvas_cols, visible_bars, offset_from_right, subcell);
    let candles: &[Candle] = &data.ohlcv[start..end];

    let write_cell =
        |buf: &mut ratatui::buffer::Buffer, col: u16, row: u16, ch: char, style: Style| {
            let x = area.x + col;
            let y = area.y + row;
            if x < area.x + area.width
                && y < area.y + area.height
                && let Some(cell) = buf.cell_mut((x, y))
            {
                cell.set_char(ch);
                cell.set_style(style);
            }
        };

    // Clear the pane area first.
    let blank = Style::default();
    for row in 0..area.height {
        for col in 0..area.width {
            write_cell(buf, col, row, ' ', blank);
        }
    }

    if candles.is_empty() {
        return;
    }

    let macd = match &data.macd {
        Some(m) => m,
        None => return,
    };

    let hist_map: std::collections::HashMap<i64, f64> =
        macd.histogram.iter().map(|p| (p.time, p.value)).collect();
    let line_map: std::collections::HashMap<i64, f64> =
        macd.line.iter().map(|p| (p.time, p.value)).collect();
    let sig_map: std::collections::HashMap<i64, f64> =
        macd.signal.iter().map(|p| (p.time, p.value)).collect();

    let hist_vals: Vec<Option<f64>> = candles
        .iter()
        .map(|c| hist_map.get(&c.time).copied())
        .collect();
    let line_vals: Vec<Option<f64>> = candles
        .iter()
        .map(|c| line_map.get(&c.time).copied())
        .collect();
    let sig_vals: Vec<Option<f64>> = candles
        .iter()
        .map(|c| sig_map.get(&c.time).copied())
        .collect();

    // Symmetric scale around zero.
    let abs_max = hist_vals
        .iter()
        .filter_map(|v| *v)
        .chain(line_vals.iter().filter_map(|v| *v))
        .chain(sig_vals.iter().filter_map(|v| *v))
        .map(|v| v.abs())
        .fold(0.0_f64, f64::max);

    if abs_max <= 0.0 {
        return;
    }

    let display_min = -abs_max;
    let display_max = abs_max;

    let to_row = |v: f64| -> usize {
        let span = display_max - display_min;
        let frac = 1.0 - ((v - display_min) / span).clamp(0.0, 1.0);
        ((frac * pane_rows as f64).round() as usize).min(pane_rows - 1)
    };
    let zero_row = to_row(0.0);

    let current_line_val = line_vals.iter().rev().find_map(|v| *v);

    let label_style = Style::default().add_modifier(Modifier::DIM);
    let ref_style = Style::default().fg(Color::Indexed(238));
    let bull_style = Style::default().fg(Color::Green);
    let bear_style = Style::default().fg(Color::Red);
    let macd_val_color = current_line_val
        .map(|v| {
            if v >= 0.0 {
                Color::Indexed(81)
            } else {
                Color::Red
            }
        })
        .unwrap_or(Color::Indexed(81));

    // Label: "MACD" at the top of the pane.
    let lbl = format!("{:<10}", "MACD");
    for (i, ch) in lbl.chars().take(Y_LABEL_W).enumerate() {
        write_cell(buf, i as u16, 0, ch, label_style);
    }

    // Current MACD line value at the bottom-right.
    if let Some(v) = current_line_val {
        let val_lbl = format!(" {:.4}", v);
        let start_col = (Y_LABEL_W + canvas_cols) as u16;
        let val_style = Style::default().fg(macd_val_color);
        for (i, ch) in val_lbl.chars().enumerate() {
            let col = start_col + i as u16;
            if col < area.width {
                write_cell(buf, col, (pane_rows - 1) as u16, ch, val_style);
            }
        }
    }

    // Zero-baseline dashed reference line.
    for col_idx in 0..canvas_cols {
        let screen_col = (Y_LABEL_W + col_idx) as u16;
        if screen_col >= area.width {
            break;
        }
        write_cell(buf, screen_col, zero_row as u16, '╌', ref_style);
    }

    // Histogram bars.
    let n_candles = candles.len();
    let bar_cols = if subcell {
        subcell_body(candle_width)
    } else {
        1
    };

    for col_idx in 0..canvas_cols {
        let screen_col = (Y_LABEL_W + col_idx) as u16;
        if screen_col >= area.width {
            break;
        }
        let ci = col_idx / candle_width;
        let is_gap = col_idx % candle_width >= bar_cols;
        if ci >= n_candles || is_gap {
            continue;
        }

        let hv = match hist_vals.get(ci).and_then(|v| *v) {
            Some(v) => v,
            None => continue,
        };

        let frac = (hv.abs() / abs_max).clamp(0.0, 1.0);
        let h_sub = ((frac * pane_rows as f64 * 2.0) as usize).min(pane_rows * 2);
        let h_full = h_sub / 2;
        let h_partial = (h_sub % 2 == 1) && h_full < pane_rows;

        let bar_style = if hv >= 0.0 { bull_style } else { bear_style };

        if hv >= 0.0 {
            // Positive: fill from (zero_row - h_full) up to zero_row.
            let full_start = zero_row.saturating_sub(h_full);
            let full_end = zero_row;
            for row in full_start..=full_end {
                if h_full > 0 {
                    write_cell(buf, screen_col, row as u16, '█', bar_style);
                }
            }
            if h_partial && full_start > 0 {
                write_cell(buf, screen_col, (full_start - 1) as u16, '▄', bar_style);
            }
        } else {
            // Negative: fill from zero_row+1 down to zero_row+h_full.
            let full_start = zero_row + 1;
            let full_end = (zero_row + h_full).min(pane_rows - 1);
            for row in full_start..=full_end {
                if h_full > 0 {
                    write_cell(buf, screen_col, row as u16, '█', bar_style);
                }
            }
            if h_partial && full_end + 1 < pane_rows {
                write_cell(buf, screen_col, (full_end + 1) as u16, '▀', bar_style);
            }
        }
    }

    // MACD line + signal line as continuous braille polylines.
    let canvas_area = ratatui::layout::Rect {
        x: area.x + Y_LABEL_W as u16,
        y: area.y,
        width: (canvas_cols as u16).min(area.width.saturating_sub(Y_LABEL_W as u16)),
        height: area.height,
    };
    // Web-parity styling: bold traces (thickness 2, like the RSI line) in the
    // web pane's hues — sky-blue MACD line over an orange signal.
    let series = [
        OscillatorLine {
            name: "MACD",
            values: &line_vals,
            color: Color::Indexed(81),
        },
        OscillatorLine {
            name: "Signal",
            values: &sig_vals,
            color: Color::Indexed(214),
        },
    ];
    draw_oscillator_lines(
        &series,
        candle_width,
        canvas_area,
        buf,
        display_min,
        display_max,
        2,
    );

    let _ = pane_width;
}

// ─── Direct-buffer renderers for volume + RSI panes (TUI path only) ───────────

pub(crate) fn render_volume_to_buf(
    data: &ChartResponse,
    chart_cols: usize,
    area: ratatui::layout::Rect,
    buf: &mut ratatui::buffer::Buffer,
    visible_bars: u32,
    offset_from_right: usize,
    subcell: bool,
) {
    use ratatui::style::{Color, Style};

    let pane_rows = area.height as usize;
    let pane_width = area.width as usize;
    if pane_rows == 0 || pane_width == 0 {
        return;
    }

    let canvas_cols = chart_cols.saturating_sub(Y_LABEL_W);
    let total = data.ohlcv.len();
    let (start, end, candle_width) =
        pane_slice(total, canvas_cols, visible_bars, offset_from_right, subcell);
    let candles: &[Candle] = &data.ohlcv[start..end];

    let write_cell =
        |buf: &mut ratatui::buffer::Buffer, col: u16, row: u16, ch: char, style: Style| {
            let x = area.x + col;
            let y = area.y + row;
            if x < area.x + area.width
                && y < area.y + area.height
                && let Some(cell) = buf.cell_mut((x, y))
            {
                cell.set_char(ch);
                cell.set_style(style);
            }
        };
    let blank_style = Style::default();
    for row in 0..area.height {
        for col in 0..area.width {
            write_cell(buf, col, row, ' ', blank_style);
        }
    }

    if candles.is_empty() {
        return;
    }

    let vol_map: std::collections::HashMap<i64, f64> =
        data.volume.iter().map(|v| (v.time, v.value)).collect();
    let volumes: Vec<f64> = candles
        .iter()
        .map(|c| vol_map.get(&c.time).copied().unwrap_or(0.0))
        .collect();
    let max_vol = volumes.iter().copied().fold(0.0_f64, f64::max);
    if max_vol <= 0.0 {
        return;
    }

    let n_candles = candles.len();

    struct BG {
        h_full: usize,
        h_partial_present: bool,
    }
    let bar_geo: Vec<BG> = volumes
        .iter()
        .map(|&v| {
            if v <= 0.0 {
                return BG {
                    h_full: 0,
                    h_partial_present: false,
                };
            }
            let frac = (v / max_vol).clamp(0.0, 1.0);
            let h_sub = ((frac * pane_rows as f64 * 2.0) as usize).min(pane_rows * 2);
            let h_full = h_sub / 2;
            let h_partial_present = (h_sub % 2 == 1) && h_full < pane_rows;
            BG {
                h_full,
                h_partial_present,
            }
        })
        .collect();

    // Per-candle trailing average volume (20-bar SMA over nonzero bars). Drawn
    // as a dotted line that tracks the bars — like the RSI line — instead of a
    // single flat horizontal level.
    const AVG_WINDOW: usize = 20;
    let avg_series: Vec<Option<f64>> = (0..volumes.len())
        .map(|i| {
            let lo = (i + 1).saturating_sub(AVG_WINDOW);
            let win: Vec<f64> = volumes[lo..=i]
                .iter()
                .copied()
                .filter(|&v| v > 0.0)
                .collect();
            if win.is_empty() {
                None
            } else {
                Some(win.iter().sum::<f64>() / win.len() as f64)
            }
        })
        .collect();
    let avg_row_at = |v: f64| -> Option<usize> {
        if v <= 0.0 {
            return None;
        }
        let frac = (v / max_vol).clamp(0.0, 1.0);
        Some((((1.0 - frac) * pane_rows as f64).round() as usize).min(pane_rows - 1))
    };
    let latest_avg = avg_series.iter().rev().find_map(|v| *v);

    let bull_style = Style::default().fg(Color::Green);
    let bear_style = Style::default().fg(Color::Red);
    let avg_style = Style::default().fg(Color::LightYellow);
    let label_style = Style::default().add_modifier(ratatui::style::Modifier::DIM);

    {
        let vol_str = fmt_volume(max_vol);
        let lbl = format!("  Vol {:>6} ", vol_str);
        for (i, ch) in lbl.chars().take(Y_LABEL_W).enumerate() {
            write_cell(buf, i as u16, 0, ch, label_style);
        }
    }

    // Average-volume tracking line: a continuous braille polyline (same renderer
    // as the RSI line). Drawn BEFORE the bars so the bars paint over it — the line
    // shows above the bar tops and sits *behind* the bars where it crosses them,
    // instead of notching gaps into the solid bar glyphs.
    {
        let canvas_area = ratatui::layout::Rect {
            x: area.x + Y_LABEL_W as u16,
            y: area.y,
            width: (canvas_cols as u16).min(area.width.saturating_sub(Y_LABEL_W as u16)),
            height: area.height,
        };
        let series = [OscillatorLine {
            name: "VolMA",
            values: &avg_series,
            color: Color::LightYellow,
        }];
        draw_oscillator_lines(&series, candle_width, canvas_area, buf, 0.0, max_vol, 1);
    }

    // In the sub-cell styles the bar fills the candle BODY width (pitch − gap)
    // so volume bars sit directly under the candles instead of a thin 1-column
    // mark at each slot's left edge. Block geometry keeps 1-col bars.
    let bar_cols = if subcell {
        subcell_body(candle_width)
    } else {
        1
    };

    for col_idx in 0..canvas_cols {
        let screen_col = (Y_LABEL_W + col_idx) as u16;
        if screen_col >= area.width {
            break;
        }
        let ci = col_idx / candle_width;
        let is_gap = col_idx % candle_width >= bar_cols;

        if ci >= n_candles || is_gap {
            continue;
        }

        let geo = &bar_geo[ci];
        if volumes[ci] <= 0.0 {
            continue;
        }

        let is_bull = candles[ci].close >= candles[ci].open;
        let bar_style = if is_bull { bull_style } else { bear_style };

        let full_start = pane_rows - geo.h_full;
        let partial_row_opt: Option<usize> = if geo.h_partial_present {
            Some(full_start - 1)
        } else {
            None
        };

        for row in 0..pane_rows {
            let glyph_opt: Option<char> = if row >= full_start {
                Some('█')
            } else if partial_row_opt == Some(row) {
                Some('▄')
            } else {
                None
            };

            if let Some(glyph) = glyph_opt {
                write_cell(buf, screen_col, row as u16, glyph, bar_style);
            }
        }
    }

    if let Some(avg) = latest_avg
        && let Some(row) = avg_row_at(avg)
    {
        let avg_label = format!(" avg {}", fmt_volume(avg));
        let label_col_start = (Y_LABEL_W + canvas_cols) as u16;
        for (i, ch) in avg_label.chars().enumerate() {
            let col = label_col_start + i as u16;
            if col < area.width {
                write_cell(buf, col, row as u16, ch, avg_style);
            }
        }
    }

    let _ = pane_width;
}

pub(crate) fn render_rsi_to_buf(
    data: &ChartResponse,
    chart_cols: usize,
    area: ratatui::layout::Rect,
    buf: &mut ratatui::buffer::Buffer,
    visible_bars: u32,
    offset_from_right: usize,
    subcell: bool,
) {
    use ratatui::style::{Color, Modifier, Style};

    let pane_rows = area.height as usize;
    let pane_width = area.width as usize;
    if pane_rows == 0 || pane_width == 0 {
        return;
    }

    let canvas_cols = chart_cols.saturating_sub(Y_LABEL_W);
    let total = data.ohlcv.len();
    let (start, end, candle_width) =
        pane_slice(total, canvas_cols, visible_bars, offset_from_right, subcell);
    let candles: &[Candle] = &data.ohlcv[start..end];

    let write_cell =
        |buf: &mut ratatui::buffer::Buffer, col: u16, row: u16, ch: char, style: Style| {
            let x = area.x + col;
            let y = area.y + row;
            if x < area.x + area.width
                && y < area.y + area.height
                && let Some(cell) = buf.cell_mut((x, y))
            {
                cell.set_char(ch);
                cell.set_style(style);
            }
        };

    let blank = Style::default();
    for row in 0..area.height {
        for col in 0..area.width {
            write_cell(buf, col, row, ' ', blank);
        }
    }

    if candles.is_empty() {
        return;
    }

    let rsi_vals: Vec<Option<f64>> = if !data.rsi.is_empty() {
        let rsi_map: std::collections::HashMap<i64, f64> =
            data.rsi.iter().map(|r| (r.time, r.value)).collect();
        let mapped: Vec<Option<f64>> = candles
            .iter()
            .map(|c| rsi_map.get(&c.time).copied())
            .collect();
        let hit_count = mapped.iter().filter(|v| v.is_some()).count();
        if hit_count * 2 >= candles.len() {
            mapped
        } else {
            let full_rsi = compute_rsi14(&data.ohlcv);
            full_rsi[start..end].to_vec()
        }
    } else {
        let full_rsi = compute_rsi14(&data.ohlcv);
        if start < full_rsi.len() {
            full_rsi[start..end.min(full_rsi.len())].to_vec()
        } else {
            vec![None; candles.len()]
        }
    };

    let current_rsi = rsi_vals.iter().rev().find_map(|v| *v);

    let visible: Vec<f64> = rsi_vals.iter().filter_map(|v| *v).collect();
    let (display_min, display_max) = if visible.is_empty() {
        (30.0, 70.0)
    } else {
        let vmin = visible.iter().cloned().fold(f64::INFINITY, f64::min);
        let vmax = visible.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let pad = ((vmax - vmin) * 0.1).max(2.0);
        let lo = (vmin - pad).clamp(0.0, 30.0);
        let hi = (vmax + pad).clamp(70.0, 100.0);
        (lo, hi)
    };

    let ref_cell_row = |level: f64| -> Option<usize> {
        if level < display_min || level > display_max {
            return None;
        }
        let span = display_max - display_min;
        let frac = 1.0 - ((level - display_min) / span);
        Some(((frac * pane_rows as f64) as usize).min(pane_rows - 1))
    };
    let row_70 = ref_cell_row(70.0);
    let row_50 = ref_cell_row(50.0);
    let row_30 = ref_cell_row(30.0);
    let label_row = row_70.or(row_50).or(row_30).unwrap_or(0);

    let rsi_color = |rsi: f64| -> Color {
        if rsi > 70.0 {
            Color::LightRed
        } else if rsi > 50.0 {
            Color::LightYellow
        } else if rsi >= 30.0 {
            Color::Cyan
        } else {
            Color::LightGreen
        }
    };

    let dim_style = Style::default().add_modifier(Modifier::DIM);
    let ref_style = Style::default().fg(Color::Indexed(238));
    let label_style = Style::default().add_modifier(Modifier::DIM);

    let lbl = format!("{:<10}", "RSI");
    for (i, ch) in lbl.chars().take(Y_LABEL_W).enumerate() {
        write_cell(buf, i as u16, label_row as u16, ch, label_style);
    }

    if let Some(rsi) = current_rsi {
        let rsi_lbl = format!(" {:.1}", rsi);
        let start_col = (Y_LABEL_W + canvas_cols) as u16;
        let style = Style::default().fg(rsi_color(rsi));
        for (i, ch) in rsi_lbl.chars().enumerate() {
            let col = start_col + i as u16;
            if col < area.width {
                write_cell(buf, col, (pane_rows - 1) as u16, ch, style);
            }
        }
    } else if candles.len() < RSI_PERIOD + 1 {
        let msg = format!(" RSI: {} bars (need {})", candles.len(), RSI_PERIOD + 1);
        let start_col = (Y_LABEL_W + canvas_cols) as u16;
        for (i, ch) in msg.chars().enumerate() {
            let col = start_col + i as u16;
            if col < area.width {
                write_cell(buf, col, (pane_rows - 1) as u16, ch, dim_style);
            }
        }
    }

    for col_idx in 0..canvas_cols {
        let screen_col = (Y_LABEL_W + col_idx) as u16;
        if screen_col >= area.width {
            break;
        }
        for r in [row_70, row_50, row_30].iter().filter_map(|x| *x) {
            write_cell(buf, screen_col, r as u16, '╌', ref_style);
        }
    }

    let canvas_area = ratatui::layout::Rect {
        x: area.x + Y_LABEL_W as u16,
        y: area.y,
        width: (canvas_cols as u16).min(area.width.saturating_sub(Y_LABEL_W as u16)),
        height: area.height,
    };
    let series = [OscillatorLine {
        name: "RSI",
        values: &rsi_vals,
        color: current_rsi.map(rsi_color).unwrap_or(Color::Cyan),
    }];
    draw_oscillator_lines(
        &series,
        candle_width,
        canvas_area,
        buf,
        display_min,
        display_max,
        2,
    );

    let _ = pane_width;
}

// ─── Oscillator line renderer ─────────────────────────────────────────────────

pub(crate) fn draw_oscillator_lines(
    series: &[OscillatorLine],
    candle_width: usize,
    canvas_area: ratatui::layout::Rect,
    buf: &mut ratatui::buffer::Buffer,
    display_min: f64,
    display_max: f64,
    thickness: usize,
) {
    let cell_cols = canvas_area.width as usize;
    let cell_rows = canvas_area.height as usize;
    if cell_cols == 0 || cell_rows == 0 || candle_width == 0 {
        return;
    }

    let mut canvas = BrailleCanvas::new(cell_cols, cell_rows);
    let dot_h = canvas.dot_height();
    if dot_h == 0 {
        return;
    }

    let to_dot_y = |v: f64| -> i32 {
        let span = (display_max - display_min).max(1e-9);
        let frac = ((v - display_min) / span).clamp(0.0, 1.0);
        let y = ((1.0 - frac) * (dot_h - 1) as f64).round() as i32;
        y.max(0).min(dot_h as i32 - 1)
    };

    let to_dot_x = |ci: usize| -> i32 { (ci * candle_width * 2) as i32 };

    // `thickness` = number of stacked dot-rows. 1 = a crisp single line, 2 = a
    // bolder trace (used for the RSI line, which would otherwise be faint).
    let max_y = dot_h as i32 - 1;
    let extra = thickness.max(1) - 1;
    for s in series {
        let mut prev: Option<(i32, i32)> = None;
        for (ci, v_opt) in s.values.iter().enumerate() {
            let Some(v) = v_opt else {
                prev = None;
                continue;
            };
            let x = to_dot_x(ci);
            let y = to_dot_y(*v);
            for d in 0..=extra as i32 {
                if let Some((px, py)) = prev {
                    canvas.line(px, (py + d).min(max_y), x, (y + d).min(max_y));
                } else {
                    canvas.set_dot(x.max(0) as usize, (y + d).min(max_y).max(0) as usize);
                }
            }
            prev = Some((x, y));
        }
        canvas.flush(
            buf,
            canvas_area,
            ratatui::style::Style::default().fg(s.color),
        );
    }
}

// ─── ANSI → ratatui Text converter ───────────────────────────────────────────

pub(crate) fn ansi_to_ratatui_text(s: &str) -> ratatui::text::Text<'static> {
    use ratatui::style::{Color as RC, Modifier as RM, Style as RS};
    use ratatui::text::{Line, Span, Text};

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut cur_style = RS::default();
    let mut buf = String::new();

    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if bytes[i] == b'\x1b' && i + 1 < len && bytes[i + 1] == b'[' {
            if !buf.is_empty() {
                spans.push(Span::styled(buf.clone(), cur_style));
                buf.clear();
            }
            i += 2;
            let start = i;
            while i < len && !bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            if i < len && bytes[i] == b'm' {
                let param_str = &s[start..i];
                let mut new_style = cur_style;
                let parts: Vec<&str> = param_str.split(';').collect();
                let mut pi = 0;
                while pi < parts.len() {
                    match parts[pi].trim() {
                        "" | "0" => {
                            new_style = RS::default();
                            pi += 1;
                        }
                        "1" => {
                            new_style = new_style.add_modifier(RM::BOLD);
                            pi += 1;
                        }
                        "2" => {
                            new_style = new_style.add_modifier(RM::DIM);
                            pi += 1;
                        }
                        "30" => {
                            new_style = new_style.fg(RC::Black);
                            pi += 1;
                        }
                        "31" => {
                            new_style = new_style.fg(RC::Red);
                            pi += 1;
                        }
                        "32" => {
                            new_style = new_style.fg(RC::Green);
                            pi += 1;
                        }
                        "33" => {
                            new_style = new_style.fg(RC::Yellow);
                            pi += 1;
                        }
                        "34" => {
                            new_style = new_style.fg(RC::Blue);
                            pi += 1;
                        }
                        "35" => {
                            new_style = new_style.fg(RC::Magenta);
                            pi += 1;
                        }
                        "36" => {
                            new_style = new_style.fg(RC::Cyan);
                            pi += 1;
                        }
                        "37" => {
                            new_style = new_style.fg(RC::White);
                            pi += 1;
                        }
                        "91" => {
                            new_style = new_style.fg(RC::LightRed);
                            pi += 1;
                        }
                        "92" => {
                            new_style = new_style.fg(RC::LightGreen);
                            pi += 1;
                        }
                        "93" => {
                            new_style = new_style.fg(RC::LightYellow);
                            pi += 1;
                        }
                        "96" => {
                            new_style = new_style.fg(RC::LightCyan);
                            pi += 1;
                        }
                        "38" if pi + 2 < parts.len() && parts[pi + 1].trim() == "5" => {
                            if let Ok(n) = parts[pi + 2].trim().parse::<u8>() {
                                new_style = new_style.fg(RC::Indexed(n));
                            }
                            pi += 3;
                        }
                        _ => {
                            pi += 1;
                        }
                    }
                }
                cur_style = new_style;
            }
            i += 1;
        } else if bytes[i] == b'\n' {
            if !buf.is_empty() {
                spans.push(Span::styled(buf.clone(), cur_style));
                buf.clear();
            }
            lines.push(Line::from(spans.clone()));
            spans.clear();
            i += 1;
        } else {
            let ch = s[i..].chars().next().unwrap_or('\u{FFFD}');
            buf.push(ch);
            i += ch.len_utf8();
        }
    }

    if !buf.is_empty() {
        spans.push(Span::styled(buf, cur_style));
    }
    if !spans.is_empty() {
        lines.push(Line::from(spans));
    }

    Text::from(lines)
}

// ─── Original static renderers (used by --once) ───────────────────────────────

pub(crate) fn render_line(
    data: &ChartResponse,
    symbol: &str,
    chart_cols: usize,
    show_cpr: bool,
    _show_ema: bool,
    pos: Option<&Position>,
    visible_bars: u32,
) -> String {
    let chart_rows = 24usize;
    let canvas_cols = chart_cols.saturating_sub(Y_LABEL_W);

    let max_candles = canvas_cols * 2;
    let slice_n = (visible_bars as usize)
        .min(max_candles)
        .min(data.ohlcv.len());
    let candles: &[Candle] = &data.ohlcv[data.ohlcv.len().saturating_sub(slice_n)..];

    let mut extra_levels: Vec<f64> = Vec::new();
    if let Some(p) = pos {
        if let Some(v) = p.entry_price {
            extra_levels.push(v);
        }
        if let Some(v) = p.stop_price {
            extra_levels.push(v);
        }
        if let Some(v) = p.target_price {
            extra_levels.push(v);
        }
    }

    let yax = YAxis::new(candles, &extra_levels, chart_rows);
    let mut canvas = Canvas::new(canvas_cols, chart_rows);

    draw_close_line(&mut canvas, candles, &yax, canvas_cols);

    let has_live_price_rl = pos.and_then(|p| p.current_price).is_some();
    let mut hlines: Vec<HLine> = Vec::new();
    if show_cpr
        && let Some(cpr) = &data.cpr
        && let Some(v) = cpr.pivot
    {
        hlines.push(HLine {
            price: v,
            label: format!("P {}", fmt_price(v)),
            ansi_color: "35",
            dashed: true,
            start_time: None,
        });
    }
    if let Some(p) = pos {
        if let Some(v) = p.stop_price {
            hlines.push(HLine {
                price: v,
                label: format!("SL {}", fmt_price(v)),
                ansi_color: "31",
                dashed: false,
                start_time: None,
            });
        }
        if let Some(v) = p.entry_price {
            hlines.push(HLine {
                price: v,
                label: format!("EN {}", fmt_price(v)),
                ansi_color: "34",
                dashed: false,
                start_time: None,
            });
        }
        if let Some(v) = p.target_price {
            hlines.push(HLine {
                price: v,
                label: format!("TP {}", fmt_price(v)),
                ansi_color: "32",
                dashed: false,
                start_time: None,
            });
        }
    }
    if !has_live_price_rl && let Some(last_c) = data.ohlcv.last() {
        hlines.push(HLine {
            price: last_c.close,
            label: format!("<< {}", fmt_price(last_c.close)),
            ansi_color: "33",
            dashed: false,
            start_time: None,
        });
    }

    let last = candles.last().unwrap();
    let prev = candles.get(candles.len().saturating_sub(2));
    let pct_chg = prev
        .map(|p| (last.close - p.close) / p.close * 100.0)
        .unwrap_or(0.0);
    let pct_str = format!("{:+.2}%", pct_chg);
    let pct_colored = if pct_chg >= 0.0 {
        f::ok(&pct_str)
    } else {
        f::alert(&pct_str)
    };

    let mut out = format!(
        "{}  {}  {} bars  {}  {}  (line mode)\n",
        f::bold(symbol),
        f::dim(&data.timeframe),
        candles.len(),
        pct_colored,
        f::dim(&fmt_price(last.close))
    );

    out += &build_chart_rows(
        &canvas,
        &hlines,
        &[],
        &yax,
        canvas_cols,
        chart_rows,
        Y_LABEL_W,
    );
    out += &render_x_axis(candles, canvas_cols, Y_LABEL_W, data);
    out
}

// ─── Tests ────────────────────────────────────────────────────────────────────

/// Human-readable label for the *current* market session, taken from the most
/// recent candle's classification (server-computed, DST-correct). Returns e.g.
/// `"New York (session)"`, `"Asia (gap)"`, `"closed"`, or `"—"` when unknown.
///
/// The per-candle block strip was dropped: once enough candles are on screen it
/// collapses too many sessions into an unreadable smear. A compact textual line
/// (schedule + current session) reads cleanly at every zoom level.
pub(crate) fn current_session_label(sessions: &[SessionEntry]) -> String {
    session_label_at(sessions, sessions.len().saturating_sub(1))
}

/// Session label for the candle at `idx` (sessions is index-aligned with
/// ohlcv). Used by the strip to label the rightmost *visible* candle when the
/// user has panned into history instead of always showing the live session.
pub(crate) fn session_label_at(sessions: &[SessionEntry], idx: usize) -> String {
    match sessions.get(idx) {
        None => "—".to_string(),
        Some(s) => {
            let name = match s.session.as_str() {
                "asia" => "Asia",
                "uk" => "London",
                "us" => "New York",
                _ => "closed",
            };
            if s.session == "dead" || s.phase.is_empty() {
                name.to_string()
            } else {
                format!("{name} ({})", s.phase)
            }
        }
    }
}

/// Draw the market-session line into `buf` (one row): a fixed UTC schedule
/// reference plus a session marker — "now: <session>" for the live edge, or
/// "at: <session>" for the rightmost visible candle when the user has panned
/// into history (sessions is index-aligned with ohlcv). No-op when the server
/// sent no session data (1D+ timeframes). `chart_cols`/`visible_bars`/
/// `subcell` are unused now (kept for signature parity with the other panes).
pub(crate) fn render_sessions_to_buf(
    data: &ChartResponse,
    _chart_cols: usize,
    area: ratatui::layout::Rect,
    buf: &mut ratatui::buffer::Buffer,
    _visible_bars: u32,
    offset_from_right: usize,
    _subcell: bool,
) {
    use ratatui::style::{Modifier, Style};

    if area.height == 0 || area.width == 0 {
        return;
    }
    let write_cell = |buf: &mut ratatui::buffer::Buffer, col: u16, ch: char, style: Style| {
        let x = area.x + col;
        if x < area.x + area.width
            && let Some(cell) = buf.cell_mut((x, area.y))
        {
            cell.set_char(ch);
            cell.set_style(style);
        }
    };

    // Clear the row first so no stale cells survive between frames.
    let blank = Style::default();
    for col in 0..area.width {
        write_cell(buf, col, ' ', blank);
    }

    let dim = Style::default().add_modifier(Modifier::DIM);
    let hot = Style::default().add_modifier(Modifier::BOLD);

    // Schedule is the standard-time (UTC) reference; London/NY shift ±1h under
    // DST, hence "≈". The session marker below is exact (from the data).
    let (marker, now) = if offset_from_right == 0 {
        ("now: ", current_session_label(&data.sessions))
    } else {
        // Panned into history: label the rightmost candle on screen, not the
        // live session — showing "New York" while looking at Asia candles
        // from yesterday was actively misleading.
        let idx = data.sessions.len().saturating_sub(1 + offset_from_right);
        ("at: ", session_label_at(&data.sessions, idx))
    };
    let schedule = format!(" Sessions (UTC≈)  Asia 00–07  London 07–13  NY 12–21    {marker}");

    let mut col: u16 = 0;
    for ch in schedule.chars() {
        if col >= area.width {
            return;
        }
        write_cell(buf, col, ch, dim);
        col += 1;
    }
    for ch in now.chars() {
        if col >= area.width {
            return;
        }
        write_cell(buf, col, ch, hot);
        col += 1;
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use crate::chart::canvas::WickInfo;
    use crate::commands::chart::constants::RSI_PERIOD;
    use crate::commands::chart::{
        Candle, ChartResponse, CprData, EmaRibbon, Indicators, VolumeEntry,
    };

    #[test]
    fn subcell_count_bounds_match_pane_slice_clamp() {
        // 70-col canvas, pitches 2..=11 → counts 6..=35; pane_slice must clamp
        // to exactly the same range so the [/] accumulator clamp stays honest.
        let (lo, hi) = subcell_count_bounds(70);
        assert_eq!((lo, hi), (6, 35));
        let (s0, e0, _) = pane_slice(1000, 70, 200, 0, true);
        assert_eq!(e0 - s0, hi, "huge visible_bars clamps to max count");
        let (s1, e1, _) = pane_slice(1000, 70, 1, 0, true);
        assert_eq!(e1 - s1, lo, "tiny visible_bars clamps to min count");
    }

    #[test]
    fn sextant_wicks_are_thin_box_chars() {
        // Wide candles (big pitch): wick must be a hairline │ (with ╵/╷ end
        // stubs), not a body-width solid sextant column.
        let data = make_data(8);
        let out = render_sextant_candles_sized(
            &data,
            100,
            14,
            CprSelect::Off,
            false,
            false,
            None,
            8,
            0,
            1.0,
            0.0,
        );
        assert!(
            out.contains('│') || out.contains('▏'),
            "expected thin │/▏ wick chars in zoomed-in sextant output"
        );
    }

    #[test]
    fn sextant_wicks_visible_when_zoomed_out() {
        // At pitch 2 the old solid wick was body-width — indistinguishable
        // from the body. The thin-char wick must still be present.
        let data = make_data(60);
        let out = render_sextant_candles_sized(
            &data,
            80,
            12,
            CprSelect::Off,
            false,
            false,
            None,
            60,
            0,
            1.0,
            0.0,
        );
        let wick_chars = out
            .chars()
            .filter(|c| matches!(c, '│' | '▏' | '╵' | '╷'))
            .count();
        assert!(
            wick_chars > 0,
            "zoomed-out sextant candles must still show wicks"
        );
    }

    #[test]
    fn ema_series_known_values() {
        // Closes 1..=5, period 3: seed = mean(1,2,3)=2; k=0.5.
        let candles: Vec<Candle> = (1..=5)
            .map(|i| Candle {
                time: i as i64,
                open: 0.0,
                high: 0.0,
                low: 0.0,
                close: i as f64,
            })
            .collect();
        let e = ema_series(&candles, 3);
        assert_eq!(e[0], None);
        assert_eq!(e[2], Some(2.0));
        assert_eq!(e[3], Some(3.0)); // 4*0.5 + 2*0.5
        assert_eq!(e[4], Some(4.0)); // 5*0.5 + 3*0.5
    }

    #[test]
    fn ema_ribbon_draws_dotted_overlay() {
        let data = make_data(120); // enough history for EMA60 warmup
        let plain = render_sextant_candles_sized(
            &data,
            110,
            16,
            CprSelect::Off,
            false,
            false,
            None,
            35,
            0,
            1.0,
            0.0,
        );
        let ribbon = render_sextant_candles_sized(
            &data,
            110,
            16,
            CprSelect::Off,
            false,
            true,
            None,
            35,
            0,
            1.0,
            0.0,
        );
        assert_ne!(plain, ribbon, "ribbon toggle must change the output");
        let ink = |s: &str| s.chars().filter(|c| !c.is_whitespace()).count();
        assert!(
            ink(&ribbon) > ink(&plain) + 20,
            "dotted ribbon must add indicator dots ({} vs {})",
            ink(&ribbon),
            ink(&plain)
        );
    }

    #[test]
    fn active_cpr_picks_the_selected_set() {
        let mut data = make_data(10);
        let mk = |p: f64| {
            Some(CprData {
                pivot: Some(p),
                ..Default::default()
            })
        };
        data.cpr = mk(1.0);
        data.weekly_cpr = mk(2.0);
        data.daily_cpr = mk(3.0);
        fn pivot(data: &ChartResponse, sel: CprSelect) -> Option<f64> {
            active_cpr(data, sel).and_then(|c| c.pivot)
        }
        assert_eq!(pivot(&data, CprSelect::Daily), Some(3.0));
        assert_eq!(pivot(&data, CprSelect::Weekly), Some(2.0));
        assert_eq!(pivot(&data, CprSelect::Monthly), Some(1.0));
        assert_eq!(pivot(&data, CprSelect::Off), None);
        // Explicit selection: a missing set draws nothing (no silent fallback
        // to a different period's levels).
        data.daily_cpr = None;
        assert_eq!(pivot(&data, CprSelect::Daily), None);
        // The default matches the web chart's primary overlay.
        assert_eq!(CprSelect::default(), CprSelect::Monthly);
        // And the cycle hits every state exactly once.
        let mut seen = vec![CprSelect::Monthly];
        let mut cur = CprSelect::Monthly;
        for _ in 0..3 {
            cur = cur.cycle();
            seen.push(cur);
        }
        assert_eq!(cur.cycle(), CprSelect::Monthly);
        seen.dedup();
        assert_eq!(seen.len(), 4, "cycle must visit all four states");
    }

    #[test]
    fn cpr_lines_render_when_near_price() {
        // CPR sits within one candle-span of the window → must draw dashes.
        // (The scale band regression: a 25%-of-span band silently dropped CPR
        // on every intraday chart.)
        let mut data = make_data(30);
        let lo = data.ohlcv.iter().map(|c| c.low).fold(f64::MAX, f64::min);
        let hi = data.ohlcv.iter().map(|c| c.high).fold(f64::MIN, f64::max);
        let span = hi - lo;
        data.cpr = Some(CprData {
            pivot: Some(hi + span * 0.5), // within one span above the range
            ..Default::default()
        });
        let out = render_sextant_candles_sized(
            &data,
            80,
            16,
            CprSelect::Monthly,
            false,
            false,
            None,
            30,
            0,
            1.0,
            0.0,
        );
        assert!(
            out.contains('╌'),
            "CPR pivot within one span must render its dashed line"
        );
    }

    /// First column holding a CPR dash on the pivot row, or None when no dash
    /// rendered.
    fn first_dash_col(out: &str) -> Option<usize> {
        out.lines()
            .find(|l| l.contains("P·"))
            .and_then(|l| l.chars().position(|c| c == '╌'))
    }

    #[test]
    fn cpr_lines_bounded_to_period_start() {
        // A virgin CPR must not be painted across candles that predate its
        // period: with periodStart at the 30th of 40 candles the dashes start
        // well right of a full-width control render.
        let mk = |period_start: Option<i64>| {
            let mut data = make_data(40);
            let hi = data.ohlcv.iter().map(|c| c.high).fold(f64::MIN, f64::max);
            let lo = data.ohlcv.iter().map(|c| c.low).fold(f64::MAX, f64::min);
            data.cpr = Some(CprData {
                pivot: Some(hi + (hi - lo) * 0.5), // off-candle row: pure line
                period_start,
                ..Default::default()
            });
            data.timeframe = "1W".to_string(); // monthly set → payload periodStart
            render_sextant_candles_sized(
                &data,
                80,
                16,
                CprSelect::Monthly,
                false,
                false,
                None,
                40,
                0,
                1.0,
                0.0,
            )
        };

        let early = mk(Some(1716000000)); // first candle → full width
        let late = mk(Some(1716000000 + 30 * 14400)); // 30th candle
        let early_col = first_dash_col(&early).expect("unbounded CPR line renders");
        let late_col = first_dash_col(&late).expect("bounded CPR line renders");
        assert!(
            late_col > early_col + 20,
            "period-bounded CPR must start well right of full width ({} vs {})",
            late_col,
            early_col
        );
    }

    #[test]
    fn cpr_line_past_window_is_hidden() {
        let mut data = make_data(40);
        let hi = data.ohlcv.iter().map(|c| c.high).fold(f64::MIN, f64::max);
        let lo = data.ohlcv.iter().map(|c| c.low).fold(f64::MAX, f64::min);
        data.cpr = Some(CprData {
            pivot: Some(hi + (hi - lo) * 0.5),
            period_start: Some(1716000000 + 100 * 14400), // after the last candle
            ..Default::default()
        });
        data.timeframe = "1W".to_string();
        let out = render_sextant_candles_sized(
            &data,
            80,
            16,
            CprSelect::Monthly,
            false,
            false,
            None,
            40,
            0,
            1.0,
            0.0,
        );
        assert!(
            first_dash_col(&out).is_none(),
            "a CPR whose period starts past the window must not draw"
        );
    }

    #[test]
    fn hline_start_cols_resolution() {
        let candles = make_candles(4, 100.0);
        let t0 = candles[0].time;
        let hl = |start_time: Option<i64>| HLine {
            price: 1.0,
            label: String::new(),
            ansi_color: "35",
            dashed: true,
            start_time,
        };
        let hlines = vec![
            hl(None),
            hl(Some(t0 - 5)),            // before the window → full width
            hl(Some(t0 + 2 * 14400)),    // third candle
            hl(Some(t0 + 1000 * 14400)), // past the window → hidden
        ];
        let cols = hline_start_cols(&hlines, &candles, |vis| vis * 10, 100);
        assert_eq!(cols, vec![0, 0, 20, 100]);
    }

    #[test]
    fn active_cpr_period_start_per_kind() {
        use chrono::TimeZone;
        // Saturday mid-2026 12:00 UTC as the newest candle.
        let last = Utc
            .with_ymd_and_hms(2026, 6, 6, 12, 0, 0)
            .unwrap()
            .timestamp();
        let mut data = make_data(3);
        if let Some(c) = data.ohlcv.last_mut() {
            c.time = last;
        }
        let day = |y, m, d| Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap().timestamp();

        // Intraday → daily set → start of the newest candle's UTC day.
        data.timeframe = "1H".to_string();
        data.daily_cpr = Some(CprData::default());
        assert_eq!(
            active_cpr_period_start(&data, CprSelect::Daily),
            Some(day(2026, 6, 6))
        );

        // 1D → weekly set → that week's Monday.
        data.timeframe = "1D".to_string();
        data.weekly_cpr = Some(CprData::default());
        assert_eq!(
            active_cpr_period_start(&data, CprSelect::Weekly),
            Some(day(2026, 6, 1))
        );

        // 1W → monthly set → server periodStart wins; month floor otherwise.
        data.timeframe = "1W".to_string();
        data.cpr = Some(CprData {
            period_start: Some(123456),
            ..Default::default()
        });
        assert_eq!(
            active_cpr_period_start(&data, CprSelect::Monthly),
            Some(123456)
        );
        data.cpr = Some(CprData::default());
        assert_eq!(
            active_cpr_period_start(&data, CprSelect::Monthly),
            Some(day(2026, 6, 1))
        );
    }

    #[test]
    fn wick_crossing_hline_keeps_candle_glyph() {
        // Wicks stay │/╵/╷ where they cross an hline (line occluded behind
        // the candle) — junction glyphs painted candle-colored horizontal
        // arms into the line. ▏ must never appear (odd-width bodies).
        let data = make_data(60);
        let out = render_sextant_candles_sized(
            &data,
            100,
            14,
            CprSelect::Off,
            false,
            false,
            None,
            60,
            0,
            1.0,
            0.0,
        );
        assert!(!out.contains('▏'), "▏ retired with the odd-body rule");
        let live = out
            .lines()
            .find(|l| l.contains("<<"))
            .expect("live price row present");
        assert!(
            !live.contains('┼') && !live.contains('┴') && !live.contains('┬'),
            "no junction glyphs on hline rows"
        );
    }

    #[test]
    fn hlines_render_over_the_ema_ribbon() {
        // The ribbon's indicator dots must not shred the live-price line:
        // on hline rows, line dashes win over indicator cells.
        let data = make_data(120);
        let out = render_sextant_candles_sized(
            &data,
            110,
            16,
            CprSelect::Off,
            false,
            true,
            None,
            35,
            0,
            1.0,
            0.0,
        );
        let live = out
            .lines()
            .find(|l| l.contains("<<"))
            .expect("live price row present");
        let dashes = live.chars().filter(|c| *c == '─').count();
        assert!(
            dashes > 40,
            "live line must stay continuous over the ribbon, got {dashes} dashes"
        );
    }

    #[test]
    fn hline_label_spill_stays_within_contract() {
        // build_chart_rows appends hline labels PAST the canvas. Interactive
        // callers budget HLINE_LABEL_SPILL columns for that — pin the contract
        // so a longer label format can't silently overflow panes again.
        use crate::commands::chart::constants::HLINE_LABEL_SPILL;
        let data = make_data(60); // includes CPR + live close
        let chart_cols = 80usize;
        let out = render_sextant_candles_sized(
            &data,
            chart_cols,
            20,
            CprSelect::Monthly,
            false,
            false,
            None,
            40,
            0,
            1.0,
            0.0,
        );
        let max_w = out
            .lines()
            .map(|l| crate::format::strip_ansi(l).chars().count())
            .max()
            .unwrap_or(0);
        assert!(
            max_w <= chart_cols + HLINE_LABEL_SPILL,
            "widest line {max_w} exceeds chart_cols {chart_cols} + spill budget {HLINE_LABEL_SPILL}"
        );
        // and at least one line actually carries a spilled label (live price)
        assert!(
            max_w > chart_cols,
            "expected an hline label to spill past the canvas"
        );
    }

    #[test]
    fn session_label_at_uses_panned_index() {
        let sessions = vec![
            SessionEntry {
                session: "asia".into(),
                phase: "session".into(),
                label: None,
                holiday: None,
            },
            SessionEntry {
                session: "us".into(),
                phase: "session".into(),
                label: None,
                holiday: None,
            },
        ];
        assert_eq!(current_session_label(&sessions), "New York (session)");
        assert_eq!(session_label_at(&sessions, 0), "Asia (session)");
        assert_eq!(
            session_label_at(&sessions, 99),
            "—",
            "out of range stays graceful"
        );
    }

    fn make_candles(n: usize, base: f64) -> Vec<Candle> {
        let mut candles = Vec::new();
        let mut price = base;
        for i in 0..n {
            let open = price;
            let close = price + (if i % 3 == 0 { 200.0 } else { -150.0 });
            let high = close.max(open) + 80.0;
            let low = close.min(open) - 80.0;
            candles.push(Candle {
                time: 1716000000 + (i as i64) * 14400,
                open,
                high,
                low,
                close,
            });
            price = close;
        }
        candles
    }

    fn make_volumes(candles: &[Candle]) -> Vec<VolumeEntry> {
        candles
            .iter()
            .enumerate()
            .map(|(i, c)| VolumeEntry {
                time: c.time,
                value: 500.0 + (i as f64 * 10.0),
                color: String::new(),
            })
            .collect()
    }

    fn make_data(n: usize) -> ChartResponse {
        let candles = make_candles(n, 105000.0);
        let volumes = make_volumes(&candles);
        ChartResponse {
            symbol: "BTC".to_string(),
            timeframe: "4H".to_string(),
            ohlcv: candles,
            cpr: Some(CprData {
                pivot: Some(104800.0),
                tc: Some(105200.0),
                bc: Some(104400.0),
                ..Default::default()
            }),
            weekly_cpr: None,
            daily_cpr: None,
            indicators: None,
            volume: volumes,
            rsi: Vec::new(),
            ..Default::default()
        }
    }

    #[test]
    fn price_marker_uses_double_angle() {
        let data = make_data(60);
        let hlines = collect_hlines(None, CprSelect::Off, &data);
        let live_line = hlines.iter().find(|hl| hl.label.starts_with("<<"));
        assert!(live_line.is_some(), "live price label should start with <<");
    }

    #[test]
    fn cpr_lines_are_dashed() {
        let data = make_data(60);
        let hlines = collect_hlines(None, CprSelect::Monthly, &data);
        for hl in hlines.iter().filter(|h| {
            h.label.starts_with('P') || h.label.starts_with("TC") || h.label.starts_with("BC")
        }) {
            assert!(hl.dashed, "CPR line {} should be dashed", hl.label);
            assert_eq!(hl.ansi_color, "35", "CPR line should be purple (35)");
        }
    }

    #[test]
    fn rsi14_warmup() {
        let candles = make_candles(30, 100.0);
        let rsi = compute_rsi14(&candles);
        assert_eq!(rsi.len(), 30);
        for (i, val) in rsi.iter().enumerate().take(RSI_PERIOD) {
            assert!(val.is_none(), "rsi[{}] should be None before warmup", i);
        }
        for (i, val) in rsi.iter().enumerate().skip(RSI_PERIOD) {
            assert!(val.is_some(), "rsi[{}] should have value after warmup", i);
            let v = val.unwrap();
            assert!((0.0..=100.0).contains(&v), "rsi[{}] out of range: {}", i, v);
        }
    }

    #[test]
    fn volume_pane_produces_output() {
        let data = make_data(50);
        let out = render_volume_pane(&data, 100, 6, 45, 0);
        assert!(!out.is_empty());
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 6, "volume pane should have 6 rows");
    }

    #[test]
    fn slice_zoom_limits() {
        let data = make_data(100);
        let out_200 = render_sextant_candles_sized(
            &data,
            220,
            20,
            CprSelect::Off,
            false,
            false,
            None,
            200,
            0,
            1.0,
            0.0,
        );
        let out_20 = render_sextant_candles_sized(
            &data,
            220,
            20,
            CprSelect::Off,
            false,
            false,
            None,
            20,
            0,
            1.0,
            0.0,
        );
        assert!(!out_200.is_empty());
        assert!(!out_20.is_empty());
        let density = |s: &str| -> usize {
            s.lines()
                .take(5)
                .map(|l| l.chars().filter(|&c| c != ' ').count())
                .sum()
        };
        assert!(density(&out_20) > 0);
        assert!(density(&out_200) > 0);
    }

    #[test]
    fn synthetic_wick_row_for_sub_row_upper_wick() {
        let candle = Candle {
            time: 0,
            open: 100.3,
            high: 100.7,
            low: 99.5,
            close: 100.5,
        };
        let yax = YAxis {
            min: 95.0,
            max: 105.0,
            chart_rows: 10,
        };
        let body_top_row = yax.to_row(candle.open.max(candle.close));
        let wick_top_row = yax.to_row(candle.high);
        assert_eq!(
            body_top_row, wick_top_row,
            "precondition: rows must collapse"
        );
        let info = WickInfo::compute(&candle, &yax, 10);
        assert_eq!(info.synth_upper_row, Some(body_top_row.saturating_sub(1)));
        assert_eq!(info.synth_lower_row, None);
    }

    #[test]
    fn no_synthetic_wick_when_data_has_no_wick() {
        let candle = Candle {
            time: 0,
            open: 100.0,
            high: 100.0,
            low: 99.5,
            close: 100.0,
        };
        let yax = YAxis {
            min: 95.0,
            max: 105.0,
            chart_rows: 10,
        };
        let info = WickInfo::compute(&candle, &yax, 10);
        assert_eq!(info.synth_upper_row, None);
    }

    #[test]
    fn live_price_row_not_solid_line() {
        let data = make_data(10);
        let out = render_sextant_candles_sized(
            &data,
            30,
            20,
            CprSelect::Off,
            false,
            false,
            None,
            10,
            0,
            1.0,
            0.0,
        );
        for line in out.lines() {
            if line.contains("<<") {
                let canvas_part: String = line.chars().skip(10).collect();
                let dash_count = canvas_part.chars().filter(|&c| c == '─').count();
                let total_canvas = canvas_part.chars().filter(|c| !c.is_whitespace()).count();
                assert!(
                    dash_count < total_canvas || total_canvas == 0,
                    "price-line row appears to be all dashes (solid line overwrites candles): '{}'",
                    line
                );
            }
        }
    }

    #[test]
    fn volume_pane_no_gap_in_bars() {
        let data = make_data(50);
        let out = render_volume_pane(&data, 100, 6, 45, 0);
        let lines: Vec<&str> = out.lines().collect();
        let pane_rows = lines.len();
        let label_offset = 13usize;
        let bar_chars: &[char] = &['█', '▄'];
        for candle_idx in 0..21usize {
            let char_pos = label_offset + candle_idx * 2;
            let filled_rows: Vec<usize> = (0..pane_rows)
                .filter(|&row| {
                    let ch = lines[row].chars().nth(char_pos).unwrap_or(' ');
                    bar_chars.contains(&ch)
                })
                .collect();
            if filled_rows.len() >= 2 {
                let min_r = *filled_rows.first().unwrap();
                let max_r = *filled_rows.last().unwrap();
                assert_eq!(
                    max_r - min_r + 1,
                    filled_rows.len(),
                    "volume bar at candle {} has gaps: rows {:?}",
                    candle_idx,
                    filled_rows
                );
            }
        }
    }

    #[test]
    fn rsi_pane_bars_visible() {
        let data = make_data(50);
        let out = render_rsi_pane(&data, 100, 6, 45, 0);
        assert!(
            out.contains('█'),
            "RSI pane should contain █ bars when data covers >14 candles"
        );
    }

    #[test]
    fn price_line_row_within_candle_body() {
        let chart_rows = 20usize;
        let ymin = 76000.0f64;
        let ymax = 78000.0f64;
        let yax = YAxis {
            min: ymin,
            max: ymax,
            chart_rows,
        };
        let price_per_row = (ymax - ymin) / chart_rows as f64;

        let body_rows = |body_lo: f64, body_hi: f64| -> (usize, usize) {
            let mut top_row = chart_rows - 1;
            let mut bot_row = 0usize;
            for r in 0..chart_rows {
                let row_hi = ymax - r as f64 * price_per_row;
                let row_lo = row_hi - price_per_row;
                if body_hi >= row_lo && body_lo <= row_hi {
                    if r < top_row {
                        top_row = r;
                    }
                    if r > bot_row {
                        bot_row = r;
                    }
                }
            }
            (top_row, bot_row)
        };

        let test_prices: Vec<f64> = (0..=200usize)
            .map(|i| ymin + (ymax - ymin) * i as f64 / 200.0)
            .collect();

        for &close in &test_prices {
            for &open in &test_prices {
                if (close - open).abs() < 1e-9 {
                    continue;
                }
                let body_lo = open.min(close);
                let body_hi = open.max(close);
                let (top_row, bot_row) = body_rows(body_lo, body_hi);
                let line_row = yax.to_row(close);

                assert!(
                    line_row >= top_row && line_row <= bot_row,
                    "close={close:.1} open={open:.1}: hline at row={line_row} outside body rows [{top_row}..{bot_row}]"
                );
            }
        }
    }

    #[test]
    fn price_line_color_matches_last_candle() {
        let mut data = make_data(20);
        let last = data.ohlcv.last_mut().unwrap();
        last.open = 100.0;
        last.close = 110.0;
        let (_, live) = collect_hlines_split(None, CprSelect::Off, &data);
        let hl = live.expect("live price line should exist");
        assert_eq!(
            hl.ansi_color, "32",
            "bull last candle should give green price line"
        );

        let last = data.ohlcv.last_mut().unwrap();
        last.open = 110.0;
        last.close = 100.0;
        let (_, live) = collect_hlines_split(None, CprSelect::Off, &data);
        let hl = live.expect("live price line should exist");
        assert_eq!(
            hl.ansi_color, "31",
            "bear last candle should give red price line"
        );
    }

    #[test]
    fn volume_pane_has_avg_line() {
        let data = make_data(30);
        let out = render_volume_pane(&data, 100, 6, 25, 0);
        assert!(
            out.contains('\u{2504}'),
            "volume pane should contain dotted avg line (┄)"
        );
        assert!(
            out.contains("avg"),
            "volume pane should contain 'avg' label"
        );
    }

    #[test]
    fn ema_tolerance_offset_timestamps() {
        let candles = make_candles(30, 100_000.0);
        let candle_dt = candles[1].time - candles[0].time;

        let ema_pts: Vec<SeriesPoint> = candles
            .iter()
            .map(|c| SeriesPoint {
                time: c.time + 1,
                value: (c.open + c.close) / 2.0,
            })
            .collect();

        let data = ChartResponse {
            symbol: "BTC".to_string(),
            timeframe: "4H".to_string(),
            ohlcv: candles.clone(),
            cpr: None,
            weekly_cpr: None,
            daily_cpr: None,
            indicators: Some(Indicators {
                ema_ribbon: Some(EmaRibbon {
                    fast: Some(ema_pts),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            volume: Vec::new(),
            rsi: Vec::new(),
            ..Default::default()
        };

        let raw_pts: Vec<(i64, f64)> = data
            .indicators
            .as_ref()
            .and_then(|ind| ind.ema_ribbon.as_ref())
            .and_then(|r| r.fast.as_ref())
            .map(|pts| pts.iter().map(|p| (p.time, p.value)).collect())
            .unwrap_or_default();
        let ema_map: std::collections::HashMap<i64, f64> = raw_pts.iter().copied().collect();
        let mut sorted_ema = raw_pts;
        sorted_ema.sort_unstable_by_key(|(t, _)| *t);

        let dt = ohlcv_interval(&data);
        assert_eq!(dt, candle_dt, "ohlcv_interval should match candle spacing");

        let mut hits = 0usize;
        for c in &candles {
            assert!(
                !ema_map.contains_key(&c.time),
                "exact lookup should miss for shifted EMA timestamps"
            );
            if ema_lookup_tolerant(&ema_map, &sorted_ema, c.time, dt).is_some() {
                hits += 1;
            }
        }
        assert_eq!(
            hits,
            candles.len(),
            "ema_lookup_tolerant should find all EMA dots for +1s offset timestamps"
        );
    }

    // ─── MACD pane tests ─────────────────────────────────────────────────────

    use crate::commands::chart::{Macd, SeriesPoint};

    /// Build a simple synthetic MACD whose timestamps align exactly with the
    /// provided candle slice.  Values alternate positive / negative so both
    /// histogram colors are exercised.
    fn make_macd(candles: &[Candle]) -> Macd {
        let mut line = Vec::new();
        let mut signal = Vec::new();
        let mut histogram = Vec::new();
        for (i, c) in candles.iter().enumerate() {
            let hv = if i % 2 == 0 {
                10.0 + i as f64
            } else {
                -(5.0 + i as f64)
            };
            let lv = hv * 0.8;
            let sv = hv * 0.5;
            histogram.push(SeriesPoint {
                time: c.time,
                value: hv,
            });
            line.push(SeriesPoint {
                time: c.time,
                value: lv,
            });
            signal.push(SeriesPoint {
                time: c.time,
                value: sv,
            });
        }
        Macd {
            line,
            signal,
            histogram,
        }
    }

    fn make_data_with_macd(n: usize) -> ChartResponse {
        let candles = make_candles(n, 105000.0);
        let volumes = make_volumes(&candles);
        let macd = make_macd(&candles);
        ChartResponse {
            symbol: "BTC".to_string(),
            timeframe: "4H".to_string(),
            ohlcv: candles,
            cpr: None,
            weekly_cpr: None,
            daily_cpr: None,
            indicators: None,
            volume: volumes,
            rsi: Vec::new(),
            macd: Some(macd),
            ..Default::default()
        }
    }

    #[test]
    fn macd_pane_produces_correct_row_count() {
        let data = make_data_with_macd(50);
        let out = render_macd_pane(&data, 100, 8, 45, 0);
        assert!(!out.is_empty(), "MACD pane should produce non-empty output");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines.len(),
            8,
            "MACD pane with pane_rows=8 should produce exactly 8 rows"
        );
    }

    #[test]
    fn macd_pane_histogram_glyphs_present() {
        let data = make_data_with_macd(50);
        let out = render_macd_pane(&data, 100, 8, 45, 0);
        assert!(
            out.contains('█') || out.contains('▄') || out.contains('▀'),
            "MACD pane should contain histogram block glyphs (█/▄/▀)"
        );
    }

    #[test]
    fn macd_pane_curves_are_continuous() {
        let data = make_data_with_macd(50);
        let out = render_macd_pane(&data, 100, 8, 45, 0);
        // Line + signal render as continuous slope glyphs (web parity), not
        // per-candle dots — and interpolation must carry them through the gap
        // columns between candle centers, so glyph count exceeds candle count.
        let curve_glyphs = out.chars().filter(|c| matches!(c, '─' | '╱' | '╲')).count();
        assert!(
            curve_glyphs > 45,
            "expected continuous curve glyphs across gap columns, got {curve_glyphs}"
        );
        assert!(!out.contains('·'), "per-candle dot markers were retired");
    }

    #[test]
    fn macd_pane_label_at_top() {
        let data = make_data_with_macd(50);
        let out = render_macd_pane(&data, 100, 8, 45, 0);
        let first_line = out.lines().next().unwrap_or("");
        assert!(
            first_line.contains("MACD"),
            "first row of MACD pane should contain 'MACD' label, got: '{}'",
            first_line
        );
    }

    #[test]
    fn macd_pane_empty_when_no_macd_data() {
        let data = make_data(50); // no macd field
        let out = render_macd_pane(&data, 100, 8, 45, 0);
        assert!(
            out.is_empty(),
            "MACD pane should be empty when data.macd is None"
        );
    }

    #[test]
    fn macd_pane_zero_rows_returns_empty() {
        let data = make_data_with_macd(50);
        let out = render_macd_pane(&data, 100, 0, 45, 0);
        assert!(
            out.is_empty(),
            "MACD pane with pane_rows=0 should return empty string"
        );
    }

    #[test]
    fn macd_pane_handles_warmup_region() {
        // Only give MACD data for the last 10 of 50 candles; the first 40 are
        // "warmup" and should render as empty / zero-baseline columns without
        // panicking.
        let candles = make_candles(50, 105000.0);
        let volumes = make_volumes(&candles);
        let warmup_macd = make_macd(&candles[40..]);
        // Timestamps match candles[40..50] only.
        let data = ChartResponse {
            symbol: "BTC".to_string(),
            timeframe: "4H".to_string(),
            ohlcv: candles,
            cpr: None,
            weekly_cpr: None,
            daily_cpr: None,
            indicators: None,
            volume: volumes,
            rsi: Vec::new(),
            macd: Some(warmup_macd),
            ..Default::default()
        };
        let out = render_macd_pane(&data, 100, 6, 45, 0);
        // Should still produce 6 rows (even if some are mostly spaces).
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines.len(),
            6,
            "MACD pane with partial data should still produce pane_rows rows"
        );
    }
}

#[cfg(test)]
mod session_pane_tests {
    use super::*;
    use crate::chart::data::{Candle, ChartResponse, SessionEntry};

    fn se(session: &str, phase: &str) -> SessionEntry {
        SessionEntry {
            session: session.to_string(),
            phase: phase.to_string(),
            label: None,
            holiday: None,
        }
    }

    #[test]
    fn current_session_label_from_last_candle() {
        assert_eq!(current_session_label(&[]), "—");
        assert_eq!(
            current_session_label(&[se("us", "session")]),
            "New York (session)"
        );
        assert_eq!(current_session_label(&[se("uk", "gap")]), "London (gap)");
        assert_eq!(
            current_session_label(&[se("asia", "extension")]),
            "Asia (extension)"
        );
        assert_eq!(current_session_label(&[se("dead", "dead")]), "closed");
    }

    #[test]
    fn current_session_uses_the_latest_entry_not_the_first() {
        let s = vec![se("asia", "session"), se("uk", "session"), se("us", "gap")];
        assert_eq!(current_session_label(&s), "New York (gap)");
    }

    #[test]
    fn render_sessions_to_buf_writes_schedule_and_current_session() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let ohlcv: Vec<Candle> = (0..3)
            .map(|i| Candle {
                time: i,
                open: 1.0,
                high: 1.0,
                low: 1.0,
                close: 1.0,
            })
            .collect();
        let sessions = vec![
            se("asia", "session"),
            se("uk", "session"),
            se("us", "session"),
        ];
        let data = ChartResponse {
            ohlcv,
            sessions,
            ..Default::default()
        };
        let area = Rect {
            x: 0,
            y: 0,
            width: 90,
            height: 1,
        };
        let mut buf = Buffer::empty(area);
        render_sessions_to_buf(&data, 90, area, &mut buf, 30, 0, false);
        let row: String = (0..area.width)
            .map(|x| {
                buf.cell((x, 0))
                    .and_then(|c| c.symbol().chars().next())
                    .unwrap_or(' ')
            })
            .collect();
        assert!(row.contains("Sessions"), "schedule label present: {row:?}");
        assert!(row.contains("London"), "schedule lists sessions: {row:?}");
        assert!(
            row.contains("now: New York"),
            "live current session present: {row:?}"
        );
    }
}
