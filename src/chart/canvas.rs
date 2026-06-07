/// Low-level terminal drawing primitives used by the chart renderer.
///
/// This module is intentionally free of I/O, networking, and ratatui-buffer
/// concerns beyond `BrailleCanvas::flush`. All state is plain in-memory data.
use crate::commands::chart::Candle;

// ─── Braille dot table ────────────────────────────────────────────────────────

pub(crate) const DOT: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];

// ─── Canvas (braille dot grid for legacy braille/line renderers) ──────────────

pub(crate) struct Canvas {
    pub(crate) cols: usize,
    pub(crate) rows: usize,
    pub(crate) dots: Vec<u8>,
    pub(crate) colors: Vec<&'static str>,
}

impl Canvas {
    pub(crate) fn new(cols: usize, rows: usize) -> Self {
        Canvas {
            cols,
            rows,
            dots: vec![0u8; cols * rows],
            colors: vec![""; cols * rows],
        }
    }

    pub(crate) fn set_dot(&mut self, bx: usize, by: usize, color: &'static str) {
        let cx = bx / 2;
        let cy = by / 4;
        if cx >= self.cols || cy >= self.rows {
            return;
        }
        let idx = cy * self.cols + cx;
        let dc = bx % 2;
        let dr = by % 4;
        self.dots[idx] |= DOT[dc][dr];
        if !color.is_empty() {
            self.colors[idx] = color;
        }
    }

    pub(crate) fn vline(&mut self, bx: usize, by_start: usize, by_end: usize, color: &'static str) {
        let (lo, hi) = if by_start <= by_end {
            (by_start, by_end)
        } else {
            (by_end, by_start)
        };
        for by in lo..=hi {
            self.set_dot(bx, by, color);
        }
    }
}

// ─── Y-axis mapping ───────────────────────────────────────────────────────────

pub(crate) struct YAxis {
    pub(crate) min: f64,
    pub(crate) max: f64,
    pub(crate) chart_rows: usize,
}

impl YAxis {
    pub(crate) fn new(candles: &[Candle], extra_levels: &[f64], rows: usize) -> Self {
        Self::new_zoomed(candles, extra_levels, rows, 1.0, 0.0)
    }

    pub(crate) fn new_zoomed(
        candles: &[Candle],
        extra_levels: &[f64],
        rows: usize,
        y_zoom: f32,
        y_offset: f32,
    ) -> Self {
        let mut lo = f64::MAX;
        let mut hi = f64::MIN;
        for c in candles {
            lo = lo.min(c.low);
            hi = hi.max(c.high);
        }
        // Overlay levels (CPR, position entry/stop/target) may stretch the
        // scale only when they sit near the candle range — within one full
        // candle-span on either side. Tight enough that a far-away level
        // can't squash the candles into a thin band (worst case they still
        // fill ~half the height); wide enough that the daily CPR on an
        // intraday window (typically 1-3% off while the window spans 1-2%)
        // stays on the chart. Far levels are clipped by the hline renderer.
        let span = (hi - lo).max(f64::EPSILON);
        let band_lo = lo - span;
        let band_hi = hi + span;
        for v in extra_levels {
            if *v >= band_lo && *v <= band_hi {
                lo = lo.min(*v);
                hi = hi.max(*v);
            }
        }
        let pad = (hi - lo) * 0.05;
        let lo = lo - pad;
        let hi = hi + pad;

        // Apply y_zoom around the midpoint, then shift by y_offset — the
        // vertical pan, in units of one full visible range per 1.0 (positive
        // pans up toward higher prices).
        let mid = (lo + hi) / 2.0;
        let half_range = (hi - lo) / 2.0;
        let zoomed_half = half_range * y_zoom as f64;
        let mid = mid + 2.0 * zoomed_half * y_offset as f64;
        YAxis {
            min: mid - zoomed_half,
            max: mid + zoomed_half,
            chart_rows: rows,
        }
    }

    /// Map a price to a sub-row index in `[0, chart_rows * sub)` where `sub` is
    /// the number of vertical dots per terminal cell (4 for braille, 3 for
    /// sextants). Row 0 is the top (highest price).
    pub(crate) fn to_subrow(&self, price: f64, sub: usize) -> usize {
        let total = (self.chart_rows * sub).max(1);
        let frac = ((price - self.min) / (self.max - self.min)).clamp(0.0, 1.0);
        (((1.0 - frac) * total as f64) as usize).min(total - 1)
    }

    pub(crate) fn to_brow(&self, price: f64) -> usize {
        self.to_subrow(price, 4)
    }

    pub(crate) fn to_row(&self, price: f64) -> usize {
        (self.to_brow(price) / 4).min(self.chart_rows.saturating_sub(1))
    }
}

// ─── Sub-cell dot grid abstraction ────────────────────────────────────────────

/// A grid of terminal cells, each subdivided into a 2-wide × N-tall array of
/// sub-cell dots. Lets the candle/line drawing code stay generic over the
/// glyph family: braille (`Canvas`, 2×4) for the finest vertical resolution, or
/// sextants (`SextantCanvas`, 2×3) for a solid fill. Dot coordinates are in
/// sub-cell units; `(0,0)` is the top-left dot of the top-left cell.
pub(crate) trait DotGrid {
    /// Vertical dots per cell (4 braille, 3 sextant).
    fn sub_rows(&self) -> usize;
    /// Set the dot at sub-cell `(dot_col, dot_row)`. Out-of-bounds is a no-op.
    fn set_dot(&mut self, dot_col: usize, dot_row: usize, color: &'static str);
    /// The glyph + color for cell `(cell_col, cell_row)`, or `None` if empty.
    fn cell(&self, cell_col: usize, cell_row: usize) -> Option<(char, &'static str)>;
    /// Vertical run of dots in one sub-cell column.
    fn vline(&mut self, dot_col: usize, r0: usize, r1: usize, color: &'static str) {
        let (lo, hi) = if r0 <= r1 { (r0, r1) } else { (r1, r0) };
        for r in lo..=hi {
            self.set_dot(dot_col, r, color);
        }
    }

    /// True when the cell's art belongs to an indicator line (EMA ribbon /
    /// EMA21 dots) rather than candles — hlines render OVER those cells.
    fn is_indicator_cell(&self, _cell_col: usize, _cell_row: usize) -> bool {
        false
    }

    /// Place an indicator dot (EMA ribbon etc.) at sub-cell precision, only
    /// where no candle art lives. Default: plain `set_dot` (braille).
    /// `SextantCanvas` tracks indicator-only cells so multiple indicator dots
    /// can share a cell while candle cells stay untouched.
    fn set_indicator_dot(&mut self, dot_col: usize, dot_row: usize, color: &'static str) {
        self.set_dot(dot_col, dot_row, color);
    }

    /// Candle body over dot-columns `cols`, spanning sub-rows `top..=bot`
    /// (top = smaller index = higher price). Default: exact sub-row fill
    /// (braille keeps its fine-grained bodies). `SextantCanvas` overrides
    /// with cell-quantized rectangles — crisp aligned edges instead of
    /// ragged partial blocks — plus a thin-sliver doji form.
    fn body_span(
        &mut self,
        cols: std::ops::Range<usize>,
        top: usize,
        bot: usize,
        color: &'static str,
    ) {
        for c in cols {
            self.vline(c, top, bot, color);
        }
    }

    /// Candle wick over dot-columns `cols`, spanning sub-rows `r0..=r1`.
    /// `ch` is the thin line glyph for fully-covered cells (always `│` —
    /// bodies are odd-width so a true middle cell exists). Default: solid
    /// dot columns (braille's dots are already hairline; it ignores `ch`).
    fn wick_span(
        &mut self,
        cols: std::ops::Range<usize>,
        r0: usize,
        r1: usize,
        color: &'static str,
        _ch: char,
    ) {
        for c in cols {
            self.vline(c, r0, r1, color);
        }
    }
}

impl DotGrid for Canvas {
    fn sub_rows(&self) -> usize {
        4
    }
    fn set_dot(&mut self, dot_col: usize, dot_row: usize, color: &'static str) {
        Canvas::set_dot(self, dot_col, dot_row, color);
    }
    fn cell(&self, cell_col: usize, cell_row: usize) -> Option<(char, &'static str)> {
        if cell_col >= self.cols || cell_row >= self.rows {
            return None;
        }
        let idx = cell_row * self.cols + cell_col;
        let bits = self.dots[idx];
        if bits == 0 {
            return None;
        }
        Some((
            char::from_u32(0x2800 + bits as u32).unwrap_or(' '),
            self.colors[idx],
        ))
    }
}

// ─── Sextant canvas (2×3 solid sub-cells) ─────────────────────────────────────

/// A grid of terminal cells, each a 2-wide × 3-tall array of *solid* sub-blocks
/// rendered with Unicode "block sextant" characters (U+1FB00…). Gives 3× the
/// vertical resolution of a plain cell while keeping a fully solid fill — the
/// middle ground between block candles (solid, 1×) and braille (dotty, 4×).
pub(crate) struct SextantCanvas {
    cols: usize,
    rows: usize,
    cells: Vec<u8>,
    colors: Vec<&'static str>,
    /// Thin-wick overlay: box-drawing chars (│ ╵ ╷) rendered in cells that
    /// hold no sextant dots. Solid sextant wicks were body-width — invisible
    /// as wicks when zoomed out (same width as the body) and chunky when
    /// zoomed in. Cells with dots always win over the overlay.
    overlay: Vec<Option<(char, &'static str)>>,
    /// Cells whose dots belong to indicator lines (EMA ribbon), not candles.
    /// Indicator dots may stack here, but never into candle-owned cells.
    indicator: Vec<bool>,
}

impl SextantCanvas {
    pub(crate) fn new(cols: usize, rows: usize) -> Self {
        SextantCanvas {
            cols,
            rows,
            cells: vec![0u8; cols * rows],
            colors: vec![""; cols * rows],
            overlay: vec![None; cols * rows],
            indicator: vec![false; cols * rows],
        }
    }
}

/// Map a 6-bit sextant mask to its Unicode glyph. Bit layout (LSB first):
/// `0`=top-left, `1`=top-right, `2`=mid-left, `3`=mid-right, `4`=bot-left,
/// `5`=bot-right. The four masks that coincide with pre-existing block glyphs
/// (empty, left half, right half, full) live outside the U+1FB00 run, so the
/// index into that run skips them.
pub(crate) fn sextant_char(mask: u8) -> char {
    let m = mask & 0x3F;
    match m {
        0 => ' ',
        0b010101 => '▌', // left column (TL, ML, BL)
        0b101010 => '▐', // right column (TR, MR, BR)
        0b111111 => '█',
        _ => {
            let idx = (m as u32 - 1) - (m > 0b010101) as u32 - (m > 0b101010) as u32;
            char::from_u32(0x1FB00 + idx).unwrap_or('?')
        }
    }
}

impl DotGrid for SextantCanvas {
    fn sub_rows(&self) -> usize {
        3
    }
    fn set_dot(&mut self, dot_col: usize, dot_row: usize, color: &'static str) {
        let cc = dot_col / 2;
        let cr = dot_row / 3;
        if cc >= self.cols || cr >= self.rows {
            return;
        }
        let bit = (dot_row % 3) * 2 + (dot_col % 2);
        let idx = cr * self.cols + cc;
        self.cells[idx] |= 1 << bit;
        if !color.is_empty() {
            self.colors[idx] = color;
        }
    }
    fn cell(&self, cell_col: usize, cell_row: usize) -> Option<(char, &'static str)> {
        if cell_col >= self.cols || cell_row >= self.rows {
            return None;
        }
        let idx = cell_row * self.cols + cell_col;
        let mask = self.cells[idx];
        if mask != 0 {
            return Some((sextant_char(mask), self.colors[idx]));
        }
        self.overlay[idx]
    }

    /// Thin wick, three cases per cell of the span:
    /// - cell owns body dots → merge the wick DOTS in (connected joint — a
    ///   skipped cell here left a visible gap between body and wick);
    /// - fully covered empty cell → the thin line glyph (`│`/`▏`);
    /// - partially covered end cell → wick dots only for the covered
    ///   sub-rows (precise, centered tip at any body parity).
    fn is_indicator_cell(&self, cell_col: usize, cell_row: usize) -> bool {
        if cell_col >= self.cols || cell_row >= self.rows {
            return false;
        }
        self.indicator[cell_row * self.cols + cell_col]
    }

    fn set_indicator_dot(&mut self, dot_col: usize, dot_row: usize, color: &'static str) {
        let cc = dot_col / 2;
        let cr = dot_row / 3;
        if cc >= self.cols || cr >= self.rows {
            return;
        }
        let idx = cr * self.cols + cc;
        // Candle art (body dots, wick/doji chars) always wins the cell;
        // indicator dots may only enter empty or indicator-owned cells.
        if self.overlay[idx].is_some() || (self.cells[idx] != 0 && !self.indicator[idx]) {
            return;
        }
        self.indicator[idx] = true;
        self.set_dot(dot_col, dot_row, color);
    }

    /// Cell-quantized body: both edges round to the nearest cell boundary,
    /// so every candle is a clean solid rectangle and edges align across the
    /// chart (max half-cell rounding error per edge — "correct-ish ratios",
    /// sharp look). A body thinner than ~2/3 cell renders as a one-sub-row
    /// sliver instead — the classic thin doji dash.
    fn body_span(
        &mut self,
        cols: std::ops::Range<usize>,
        top: usize,
        bot: usize,
        color: &'static str,
    ) {
        let height_sub = bot.saturating_sub(top) + 1;
        if height_sub <= 1 {
            // True hairline doji: a thin ─ dash across the body cells (chars,
            // not dots — dot slivers merged with the wick into blocky cells).
            // The wick pass turns the crossing cell into ┼.
            let mid_cell = (((top + bot) / 2) / 3).min(self.rows.saturating_sub(1));
            let first_cell = cols.start / 2;
            let last_cell = (cols.end.saturating_sub(1)) / 2;
            for cc in first_cell..=last_cell.min(self.cols.saturating_sub(1)) {
                let idx = mid_cell * self.cols + cc;
                if self.cells[idx] == 0 {
                    self.overlay[idx] = Some(('─', color));
                }
            }
            return;
        }
        // Round each edge to its nearest cell boundary; guarantee ≥1 cell.
        let cell_top = (top + 1) / 3; // ≈ round(top/3)
        let cell_bot_excl = ((bot + 1) + 1) / 3; // ≈ round((bot+1)/3)
        let (cell_top, cell_bot_excl) = if cell_bot_excl > cell_top {
            (cell_top, cell_bot_excl)
        } else {
            let mid_cell = ((top + bot) / 2) / 3;
            (mid_cell, mid_cell + 1)
        };
        for cr in cell_top..cell_bot_excl {
            for r in (cr * 3)..(cr * 3 + 3) {
                for c in cols.clone() {
                    self.set_dot(c, r, color);
                }
            }
        }
    }

    fn wick_span(
        &mut self,
        cols: std::ops::Range<usize>,
        r0: usize,
        r1: usize,
        color: &'static str,
        ch: char,
    ) {
        // The char cell: the body's true middle cell (bodies are odd-width).
        let char_cell = cols.start.div_ceil(2);
        if char_cell >= self.cols {
            return;
        }
        let (lo, hi) = if r0 <= r1 { (r0, r1) } else { (r1, r0) };
        for cr in (lo / 3)..=(hi / 3).min(self.rows.saturating_sub(1)) {
            let idx = cr * self.cols + char_cell;
            let mask = self.cells[idx];
            if mask == 0x3F {
                continue; // solid body interior — wick hidden behind it
            }
            // Crossing the doji's ─ dash: a proper box-drawing cross.
            let crosses_dash = matches!(self.overlay[idx], Some(('─', _)));
            let covers_full = lo <= cr * 3 && hi >= cr * 3 + 2;
            if mask == 0 && covers_full {
                let glyph = if crosses_dash { '┼' } else { ch };
                self.overlay[idx] = Some((glyph, color));
                continue;
            }
            let from = lo.max(cr * 3);
            let to = hi.min(cr * 3 + 2);
            if mask == 0 {
                // Partial tip cell: thin half-cell stub.
                let stub = if crosses_dash {
                    '┼'
                } else {
                    match ch {
                        '│' if to < cr * 3 + 2 => '╵',
                        '│' => '╷',
                        other => other,
                    }
                };
                self.overlay[idx] = Some((stub, color));
                continue;
            }
            // Partially-filled cell: with cell-quantized bodies this is a
            // doji sliver — merge exact dots so the wick crosses it (the
            // classic doji "+" shape).
            for r in from..=to {
                for c in cols.clone() {
                    self.set_dot(c, r, color);
                }
            }
        }
    }
}

// ─── Braille canvas (oscillator line rendering) ──────────────────────────────

/// A grid of braille cells, each holding a 2-dot-wide × 4-dot-tall bitmask.
/// Filled cells flush as Unicode `U+2800 + mask` (braille pattern characters)
/// at the cell coordinates. Used by the RSI / oscillator pane to render lines
/// at 4× the vertical and 2× the horizontal resolution of a normal terminal
/// cell.
pub(crate) struct BrailleCanvas {
    cell_cols: usize,
    cell_rows: usize,
    cells: Vec<u8>,
}

impl BrailleCanvas {
    pub(crate) fn new(cell_cols: usize, cell_rows: usize) -> Self {
        BrailleCanvas {
            cell_cols,
            cell_rows,
            cells: vec![0u8; cell_cols * cell_rows],
        }
    }

    /// Height in dots (cell_rows * 4).
    pub(crate) fn dot_height(&self) -> usize {
        self.cell_rows * 4
    }

    /// Set the dot at (dot_col, dot_row). Out-of-bounds is a no-op.
    pub(crate) fn set_dot(&mut self, dot_col: usize, dot_row: usize) {
        let cell_col = dot_col / 2;
        let cell_row = dot_row / 4;
        if cell_col >= self.cell_cols || cell_row >= self.cell_rows {
            return;
        }
        let sub_col = dot_col % 2;
        let sub_row = dot_row % 4;
        let bit = match (sub_col, sub_row) {
            (0, 0) => 0,
            (0, 1) => 1,
            (0, 2) => 2,
            (0, 3) => 6,
            (1, 0) => 3,
            (1, 1) => 4,
            (1, 2) => 5,
            (1, 3) => 7,
            _ => return,
        };
        let idx = cell_row * self.cell_cols + cell_col;
        self.cells[idx] |= 1 << bit;
    }

    /// Bresenham line in dot-space.
    pub(crate) fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        let mut x0 = x0;
        let mut y0 = y0;
        let dx = (x1 - x0).abs();
        let sx: i32 = if x0 < x1 { 1 } else { -1 };
        let dy = -(y1 - y0).abs();
        let sy: i32 = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        loop {
            if x0 >= 0 && y0 >= 0 {
                self.set_dot(x0 as usize, y0 as usize);
            }
            if x0 == x1 && y0 == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                if x0 == x1 {
                    break;
                }
                err += dy;
                x0 += sx;
            }
            if e2 <= dx {
                if y0 == y1 {
                    break;
                }
                err += dx;
                y0 += sy;
            }
        }
    }

    /// Write all non-empty cells to the ratatui buffer at `area`. Cells with
    /// mask == 0 are left untouched (so reference lines or other underlay
    /// content show through).
    pub(crate) fn flush(
        &self,
        buf: &mut ratatui::buffer::Buffer,
        area: ratatui::layout::Rect,
        style: ratatui::style::Style,
    ) {
        for cell_row in 0..self.cell_rows {
            for cell_col in 0..self.cell_cols {
                let mask = self.cells[cell_row * self.cell_cols + cell_col];
                if mask == 0 {
                    continue;
                }
                let codepoint = 0x2800u32 + mask as u32;
                let Some(ch) = char::from_u32(codepoint) else {
                    continue;
                };
                let x = area.x + cell_col as u16;
                let y = area.y + cell_row as u16;
                if x < area.x + area.width
                    && y < area.y + area.height
                    && let Some(cell) = buf.cell_mut((x, y))
                {
                    cell.set_char(ch);
                    cell.set_style(style);
                }
            }
        }
    }
}

// ─── OscillatorLine ──────────────────────────────────────────────────────────

/// One oscillator series to render on the RSI / oscillator pane.
///
/// `values` is per-candle (matching the candle slice length); `None` means
/// "no data for this bar" and breaks the line at that point. Adding stochastic
/// RSI or any other 0–100 oscillator is a matter of pushing a second
/// `OscillatorLine` into the slice passed to the renderer.
pub(crate) struct OscillatorLine<'a> {
    #[allow(dead_code)] // series label for a future legend, set but not yet rendered
    pub name: &'a str,
    pub values: &'a [Option<f64>],
    pub color: ratatui::style::Color,
}

// ─── Sub-row body partial-fill glyphs ────────────────────────────────────────

// ─── Per-candle wick visibility ──────────────────────────────────────────────

/// When the upper or lower wick spans less than one row's worth of price,
/// `yax.to_row(wick_hi)` rounds into the same row as `yax.to_row(body_hi)`
/// and the wick disappears at that zoom level. We detect this and synthesize
/// an adjacent wick row so the candle visually reflects what the data says
/// (a candle with wick data should always show a wick).
#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct WickInfo {
    /// `Some(row)` when an upper wick exists but its top row collapses into
    /// the body row at the current zoom; the wick will be drawn at this row.
    pub(crate) synth_upper_row: Option<usize>,
    /// Symmetric for the lower wick.
    pub(crate) synth_lower_row: Option<usize>,
}

#[cfg(test)]
impl WickInfo {
    pub(crate) fn compute(c: &Candle, yax: &YAxis, chart_rows: usize) -> Self {
        let body_hi = c.open.max(c.close);
        let body_lo = c.open.min(c.close);
        let body_top_row = yax.to_row(body_hi);
        let body_bot_row = yax.to_row(body_lo);
        let wick_top_row = yax.to_row(c.high);
        let wick_bot_row = yax.to_row(c.low);

        let upper_eaten = c.high > body_hi && wick_top_row >= body_top_row;
        let lower_eaten = c.low < body_lo && wick_bot_row <= body_bot_row;

        let synth_upper_row = if upper_eaten && body_top_row > 0 {
            Some(body_top_row - 1)
        } else {
            None
        };
        let synth_lower_row = if lower_eaten && body_bot_row + 1 < chart_rows {
            Some(body_bot_row + 1)
        } else {
            None
        };
        WickInfo {
            synth_upper_row,
            synth_lower_row,
        }
    }
}

// ─── Horizontal overlay lines ─────────────────────────────────────────────────

pub(crate) struct HLine {
    pub(crate) price: f64,
    pub(crate) label: String,
    pub(crate) ansi_color: &'static str,
    /// If true, render with dashed `╌` chars instead of solid `─`
    pub(crate) dashed: bool,
    /// Earliest candle timestamp (UNIX seconds) the line applies from.
    /// `None` = full width. CPR levels set this to the current period's start
    /// so a virgin CPR is not drawn across older candles that never traded
    /// through it (the web chart bounds CPR the same way).
    pub(crate) start_time: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sextant_anchors_and_block_aliases() {
        // Empty / full / half-column aliases live outside the U+1FB00 run.
        assert_eq!(sextant_char(0b000000), ' ');
        assert_eq!(sextant_char(0b010101), '▌'); // left column
        assert_eq!(sextant_char(0b101010), '▐'); // right column
        assert_eq!(sextant_char(0b111111), '█');
        // Run endpoints + a midpoint, by Unicode name.
        assert_eq!(sextant_char(0b000001), '\u{1FB00}'); // SEXTANT-1 (top-left)
        assert_eq!(sextant_char(0b000011), '\u{1FB02}'); // SEXTANT-12 (top row)
        assert_eq!(sextant_char(0b111110), '\u{1FB3B}'); // SEXTANT-23456
    }

    fn mk_candle(low: f64, high: f64) -> Candle {
        Candle {
            time: 0,
            open: low,
            high,
            low,
            close: high,
        }
    }

    #[test]
    fn sextant_body_quantizes_to_full_cells() {
        // Body spanning sub-rows 1..=4 (1.33 cells) → two FULL cells, no
        // ragged partial edges (the "candles falling apart" complaint).
        let mut g = SextantCanvas::new(2, 4);
        g.body_span(0..4, 1, 4, "32");
        let (ch0, _) = g.cell(0, 0).unwrap();
        let (ch1, _) = g.cell(0, 1).unwrap();
        assert_eq!(ch0, '█', "edge cell must round to a solid block");
        assert_eq!(ch1, '█', "second cell must be solid");
        assert!(g.cell(0, 2).is_none(), "no spill past the rounded edge");
    }

    #[test]
    fn sextant_hairline_doji_is_thin_dash() {
        // A ≤1-sub-row body renders as a thin ─ char dash, not dot blocks.
        let mut g = SextantCanvas::new(2, 2);
        g.body_span(0..4, 3, 3, "31"); // sub-row 3 → cell row 1
        assert_eq!(g.cell(0, 1), Some(('─', "31")));
        assert_eq!(g.cell(1, 1), Some(('─', "31")));
        assert!(g.cell(0, 0).is_none());
    }

    #[test]
    fn wick_through_doji_dash_is_a_cross() {
        let mut g = SextantCanvas::new(2, 3);
        g.body_span(0..4, 4, 4, "31"); // dash on cell row 1
        // Wick spanning the full height through the dash cell.
        g.wick_span(1..3, 0, 8, "31", '│');
        let (mid, _) = g.cell(1, 1).unwrap();
        assert_eq!(mid, '┼', "wick × doji dash must render as a cross");
        let (top, _) = g.cell(1, 0).unwrap();
        assert_eq!(top, '│');
    }

    #[test]
    fn yaxis_far_level_does_not_blow_scale() {
        // A level far outside the candle range (stale pivot, distant stop)
        // must not stretch the scale — it squashed candles into a thin band.
        let candles = vec![mk_candle(100.0, 110.0)];
        let with_far = YAxis::new_zoomed(&candles, &[500.0], 20, 1.0, 0.0);
        let plain = YAxis::new_zoomed(&candles, &[], 20, 1.0, 0.0);
        assert!((with_far.max - plain.max).abs() < 1e-9);
        assert!((with_far.min - plain.min).abs() < 1e-9);
    }

    #[test]
    fn yaxis_near_level_still_stretches_scale() {
        // Levels within one candle-span keep stretching the scale so nearby
        // CPR/entry lines stay visible (daily CPR on an intraday window).
        let candles = vec![mk_candle(100.0, 110.0)];
        let yax = YAxis::new_zoomed(&candles, &[118.0], 20, 1.0, 0.0);
        assert!(yax.max >= 118.0, "level within one span must stay on-scale");
        // …but one just beyond the band must not.
        let yax2 = YAxis::new_zoomed(&candles, &[125.0], 20, 1.0, 0.0);
        assert!(
            yax2.max < 125.0,
            "level beyond one span must not stretch the scale"
        );
    }

    #[test]
    fn yaxis_y_offset_pans_window_up() {
        let candles = vec![mk_candle(100.0, 110.0)];
        let centered = YAxis::new_zoomed(&candles, &[], 20, 1.0, 0.0);
        let panned = YAxis::new_zoomed(&candles, &[], 20, 1.0, 0.5);
        let range = centered.max - centered.min;
        assert!((panned.min - (centered.min + range * 0.5)).abs() < 1e-9);
        assert!((panned.max - (centered.max + range * 0.5)).abs() < 1e-9);
    }

    #[test]
    fn sextant_set_dot_bit_layout() {
        let mut g = SextantCanvas::new(2, 2);
        // top-left + bottom-right of cell (0,0)
        g.set_dot(0, 0, "32"); // TL -> bit0
        g.set_dot(1, 2, "32"); // BR -> bit5
        let (ch, color) = g.cell(0, 0).unwrap();
        assert_eq!(color, "32");
        assert_eq!(ch, sextant_char(0b100001));
        // an untouched cell is empty
        assert!(g.cell(1, 1).is_none());
    }
}
