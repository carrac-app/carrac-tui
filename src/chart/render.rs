/// Pure chart render pass: takes candle data + indicators + drawings and returns PNG bytes.
///
/// No I/O happens here — callers write the result to disk or emit via KGP.
use anyhow::{Result, anyhow};
use plotters::prelude::*;
use serde::{Deserialize, Serialize};

use crate::chart::primitives::{ChartPrimitive, LineStyle};
use crate::commands::chart::{Candle, CprData, fmt_price};

/// Register the embedded sans-serif font under the name the chart label styles
/// reference (`("sans-serif", N)`). With the `ab_glyph` backend plotters has no
/// system-font discovery, so the font must be registered before any text is
/// drawn. Idempotent: runs exactly once per process.
fn ensure_font() {
    use std::sync::Once;
    static FONT_INIT: Once = Once::new();
    FONT_INIT.call_once(|| {
        let _ = plotters::style::register_font(
            "sans-serif",
            plotters::style::FontStyle::Normal,
            include_bytes!("../../assets/fonts/LiberationSans-Regular.ttf"),
        );
    });
}

// ─── Drawing types from /api/drawings ────────────────────────────────────────

/// A single drawing returned by `/api/drawings?asset=&timeframe=`.
/// Only the fields we need for rendering are typed; the rest is ignored.
/// `Serialize` so the launch-agent bundle can hand the agent exact, machine-
/// readable drawing levels (`drawings.json`), not just the rendered pixels.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Drawing {
    pub drawing_type: String,
    /// Primary price level (horizontal line Y, trendline start Y, zone bottom, etc.)
    #[serde(default)]
    pub price_level: Option<f64>,
    /// Secondary price level (zone top, trendline end Y, etc.)
    #[serde(default)]
    pub price_level_upper: Option<f64>,
    /// Display color (CSS hex string e.g. "#ff0000")
    #[serde(default)]
    pub color: Option<String>,
    /// Optional text label
    #[serde(default)]
    pub label: Option<String>,
    /// Rich metadata blob (trendline timestamps, `kline_name`, etc.)
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

/// Response from `GET /api/drawings?asset=&timeframe=`
#[derive(Debug, Deserialize)]
pub struct DrawingsResponse {
    pub drawings: Vec<Drawing>,
}

// ─── Indicator carry-through types ───────────────────────────────────────────

/// A time+value data point (used for EMA series).
#[derive(Debug, Clone, Deserialize)]
pub struct SeriesPoint {
    pub time: i64,
    pub value: f64,
}

/// EMA ribbon: fast + slow series.
/// Separate from the TUI `EmaRibbon` (chart.rs) which only has a fast series;
/// this version supports a slow series for the PNG renderer.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct EmaRibbon {
    #[serde(default)]
    pub fast: Vec<SeriesPoint>,
    #[serde(default)]
    pub slow: Vec<SeriesPoint>,
}

// ─── Dark background palette ─────────────────────────────────────────────────

const BG: RGBColor = RGBColor(22, 22, 28);
const AXIS_COLOR: RGBColor = RGBColor(80, 80, 90);
const CANDLE_UP: RGBColor = RGBColor(38, 166, 154); // teal green
const CANDLE_DOWN: RGBColor = RGBColor(239, 83, 80); // coral red
const CPR_PIVOT: RGBColor = RGBColor(255, 200, 60); // yellow
const CPR_TC: RGBColor = RGBColor(100, 200, 255); // light blue
const CPR_BC: RGBColor = RGBColor(255, 140, 100); // salmon
const EMA_FAST: RGBColor = RGBColor(243, 222, 44); // yellow
const EMA_SLOW: RGBColor = RGBColor(60, 190, 120); // green
const TEXT_COLOR: RGBColor = RGBColor(180, 180, 190);

// Support (S1/S2/S3) — red shades; Resistance (R1/R2/R3) — green shades.
// Deeper levels are slightly dimmer so the primary S1/R1 reads strongest.
const CPR_S1: RGBColor = RGBColor(239, 83, 80); // strong red
const CPR_S2: RGBColor = RGBColor(200, 70, 67); // medium red
const CPR_S3: RGBColor = RGBColor(160, 55, 53); // dim red
const CPR_R1: RGBColor = RGBColor(76, 175, 80); // strong green
const CPR_R2: RGBColor = RGBColor(60, 145, 65); // medium green
const CPR_R3: RGBColor = RGBColor(45, 115, 50); // dim green

// Drawing overlay colors (kept visually distinct from candles + indicators).
const DRW_HLINE: RGBColor = RGBColor(255, 85, 255); // magenta
const DRW_TREND: RGBColor = RGBColor(100, 220, 255); // cyan
const DRW_ZONE: RGBColor = RGBColor(255, 200, 80); // amber
const DRW_ENTRY: RGBColor = RGBColor(80, 220, 120); // green (entry)
const DRW_SL: RGBColor = RGBColor(239, 83, 80); // red (stop loss)
const DRW_TP: RGBColor = RGBColor(38, 166, 154); // teal (take profit)

// Rich-renderer primitive defaults (muted so candles stay readable).
const PRIM_HLINE_DEFAULT: RGBColor = RGBColor(200, 100, 255); // violet
const PRIM_POLYLINE_DEFAULT: RGBColor = RGBColor(80, 200, 230); // muted cyan
const PRIM_RECT_DEFAULT: RGBColor = RGBColor(255, 190, 60); // muted amber
const PRIM_MARKER_DEFAULT: RGBColor = RGBColor(255, 120, 60); // muted orange

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Parse a CSS hex color string (#rrggbb or #rgb) to an `RGBColor`.
/// Returns a fallback magenta if parsing fails.
fn parse_css_color(css: &str) -> RGBColor {
    let s = css.trim().trim_start_matches('#');
    match s.len() {
        6 => {
            if let (Ok(r), Ok(g), Ok(b)) = (
                u8::from_str_radix(&s[0..2], 16),
                u8::from_str_radix(&s[2..4], 16),
                u8::from_str_radix(&s[4..6], 16),
            ) {
                return RGBColor(r, g, b);
            }
        }
        3 => {
            if let (Ok(r), Ok(g), Ok(b)) = (
                u8::from_str_radix(&s[0..1].repeat(2), 16),
                u8::from_str_radix(&s[1..2].repeat(2), 16),
                u8::from_str_radix(&s[2..3].repeat(2), 16),
            ) {
                return RGBColor(r, g, b);
            }
        }
        _ => {}
    }
    DRW_HLINE // fallback
}

// ─── Dashed / dotted line helpers ────────────────────────────────────────────

/// Draw a dashed or dotted horizontal line by emitting small `PathElement` segments.
///
/// `chart`        — target chart context
/// `x_start`/`x_end` — time-axis range
/// `price`        — y value
/// `color`        — stroke color
/// `style`        — Solid / Dashed / Dotted
/// `step_frac`    — fraction of the total x-span per segment (e.g. 1/80 for dashes)
fn draw_styled_hline<DB: DrawingBackend>(
    chart: &mut ChartContext<
        DB,
        Cartesian2d<plotters::coord::types::RangedCoordi64, plotters::coord::types::RangedCoordf64>,
    >,
    x_start: i64,
    x_end: i64,
    price: f64,
    color: RGBColor,
    style: LineStyle,
) -> Result<()>
where
    DB::ErrorType: std::error::Error + Send + Sync + 'static,
{
    let span = (x_end - x_start).max(1);
    match style {
        LineStyle::Solid => {
            chart.draw_series(std::iter::once(PathElement::new(
                vec![(x_start, price), (x_end, price)],
                ShapeStyle {
                    color: color.to_rgba(),
                    filled: false,
                    stroke_width: 1,
                },
            )))?;
        }
        LineStyle::Dashed => {
            // Draw segments: 3% filled, 2% gap (visually clear dashes).
            let seg = span / 40;
            if seg == 0 {
                // Too narrow — just draw solid.
                chart.draw_series(std::iter::once(PathElement::new(
                    vec![(x_start, price), (x_end, price)],
                    ShapeStyle {
                        color: color.to_rgba(),
                        filled: false,
                        stroke_width: 1,
                    },
                )))?;
                return Ok(());
            }
            let gap = seg * 2 / 3;
            let mut x = x_start;
            while x < x_end {
                let x2 = (x + seg).min(x_end);
                chart.draw_series(std::iter::once(PathElement::new(
                    vec![(x, price), (x2, price)],
                    ShapeStyle {
                        color: color.to_rgba(),
                        filled: false,
                        stroke_width: 1,
                    },
                )))?;
                x = x2 + gap;
            }
        }
        LineStyle::Dotted => {
            // Single-pixel dots spaced every ~2% of total span.
            let gap = span / 50;
            if gap == 0 {
                chart.draw_series(std::iter::once(PathElement::new(
                    vec![(x_start, price), (x_end, price)],
                    ShapeStyle {
                        color: color.to_rgba(),
                        filled: false,
                        stroke_width: 1,
                    },
                )))?;
                return Ok(());
            }
            let mut x = x_start;
            while x <= x_end {
                chart.draw_series(std::iter::once(PathElement::new(
                    vec![(x, price), (x + 1, price)],
                    ShapeStyle {
                        color: color.to_rgba(),
                        filled: false,
                        stroke_width: 2,
                    },
                )))?;
                x += gap;
            }
        }
    }
    Ok(())
}

/// Draw a styled polyline (solid / dashed / dotted) between a list of (time, price) points.
fn draw_styled_polyline<DB: DrawingBackend>(
    chart: &mut ChartContext<
        DB,
        Cartesian2d<plotters::coord::types::RangedCoordi64, plotters::coord::types::RangedCoordf64>,
    >,
    points: &[(i64, f64)],
    color: RGBColor,
    style: LineStyle,
) -> Result<()>
where
    DB::ErrorType: std::error::Error + Send + Sync + 'static,
{
    if points.len() < 2 {
        return Ok(());
    }
    match style {
        LineStyle::Solid => {
            chart.draw_series(LineSeries::new(
                points.iter().copied(),
                color.stroke_width(1),
            ))?;
        }
        LineStyle::Dashed | LineStyle::Dotted => {
            // Segment each consecutive pair with the same logic as the hline helper.
            for window in points.windows(2) {
                let (x1, y1) = window[0];
                let (x2, y2) = window[1];
                let span = (x2 - x1).abs().max(1);
                match style {
                    LineStyle::Dashed => {
                        let seg = span / 10;
                        if seg == 0 {
                            chart.draw_series(std::iter::once(PathElement::new(
                                vec![(x1, y1), (x2, y2)],
                                ShapeStyle {
                                    color: color.to_rgba(),
                                    filled: false,
                                    stroke_width: 1,
                                },
                            )))?;
                            continue;
                        }
                        let gap = seg * 2 / 3;
                        let steps = (span / (seg + gap)).max(1);
                        for k in 0..steps {
                            let t0 = k as f64 / steps as f64;
                            let t1 = ((k as f64 * (seg + gap) as f64 + seg as f64) / span as f64)
                                .min(1.0);
                            let px0 = x1 + (t0 * span as f64) as i64;
                            let px1 = x1 + (t1 * span as f64) as i64;
                            let py0 = y1 + t0 * (y2 - y1);
                            let py1 = y1 + t1 * (y2 - y1);
                            chart.draw_series(std::iter::once(PathElement::new(
                                vec![(px0, py0), (px1, py1)],
                                ShapeStyle {
                                    color: color.to_rgba(),
                                    filled: false,
                                    stroke_width: 1,
                                },
                            )))?;
                        }
                    }
                    LineStyle::Dotted => {
                        let gap = span / 8;
                        if gap == 0 {
                            chart.draw_series(std::iter::once(PathElement::new(
                                vec![(x1, y1), (x2, y2)],
                                ShapeStyle {
                                    color: color.to_rgba(),
                                    filled: false,
                                    stroke_width: 1,
                                },
                            )))?;
                            continue;
                        }
                        let steps = (span / gap).max(1);
                        for k in 0..=steps {
                            let t = (k as f64 / steps as f64).min(1.0);
                            let px = x1 + (t * span as f64) as i64;
                            let py = y1 + t * (y2 - y1);
                            chart.draw_series(std::iter::once(PathElement::new(
                                vec![(px, py), (px + 1, py)],
                                ShapeStyle {
                                    color: color.to_rgba(),
                                    filled: false,
                                    stroke_width: 2,
                                },
                            )))?;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

// ─── Rich PNG renderer ────────────────────────────────────────────────────────

/// Render candles + optional CPR + optional EMA ribbon + `ChartPrimitive` overlays
/// into a PNG byte buffer.
///
/// Draws the same base chart as `render_png` and adds:
/// - **Primitives** (`ChartPrimitive` slice): Hline, Polyline, Rect, Marker with
///   `LineStyle` support and optional labels.
///
/// The existing `render_png` signature and callers are unchanged.
#[allow(clippy::too_many_arguments)] // render entry point; a params struct adds churn without clarity
pub fn render_png_rich(
    candles: &[Candle],
    cpr: Option<&CprData>,
    ema_ribbon: Option<&EmaRibbon>,
    overlay_cpr: bool,
    overlay_ema: bool,
    primitives: &[ChartPrimitive],
    width: u32,
    height: u32,
) -> Result<Vec<u8>> {
    if candles.is_empty() {
        return Err(anyhow!("no candle data to render"));
    }
    ensure_font();

    // Price range — pad by 3% so candles aren't flush against the edges.
    let price_min = candles.iter().map(|c| c.low).fold(f64::INFINITY, f64::min);
    let price_max = candles
        .iter()
        .map(|c| c.high)
        .fold(f64::NEG_INFINITY, f64::max);
    let pad = (price_max - price_min) * 0.03;
    let price_lo = price_min - pad;
    let price_hi = price_max + pad;

    // Time range (UNIX timestamps as i64).
    let time_min = candles.first().map_or(0, |c| c.time);
    let time_max = candles.last().map_or(0, |c| c.time);

    // Allocate an in-memory pixel buffer and write PNG into it.
    let mut buf: Vec<u8> = vec![0u8; (width * height * 3) as usize];
    {
        let root = BitMapBackend::with_buffer(&mut buf, (width, height)).into_drawing_area();
        root.fill(&BG)?;

        // ── Candle chart ─────────────────────────────────────────────────────
        let mut chart = ChartBuilder::on(&root)
            .margin(16)
            .x_label_area_size(30)
            .y_label_area_size(70)
            .build_cartesian_2d(time_min..time_max, price_lo..price_hi)?;

        chart
            .configure_mesh()
            .disable_x_mesh()
            .disable_y_mesh()
            .axis_style(AXIS_COLOR)
            .label_style(("sans-serif", 11).into_font().color(&TEXT_COLOR))
            .x_labels(6)
            .y_labels(8)
            .x_label_formatter(&|ts| {
                let dt = chrono::DateTime::from_timestamp(*ts, 0);
                match dt {
                    Some(d) => d.format("%m-%d").to_string(),
                    None => String::new(),
                }
            })
            .y_label_formatter(&|price| fmt_price(*price))
            .draw()?;

        // Candle body width in pixels.
        let draw_width_px = f64::from(width.saturating_sub(16 * 2 + 70));
        let slot_px = draw_width_px / candles.len().max(1) as f64;
        let body_width_px = ((slot_px * 0.70) as u32).max(1);

        // Draw candle series.
        chart.draw_series(candles.iter().map(|c| {
            let color = if c.close >= c.open {
                CANDLE_UP
            } else {
                CANDLE_DOWN
            };
            CandleStick::new(
                c.time,
                c.open,
                c.high,
                c.low,
                c.close,
                color.filled(),
                color.stroke_width(1),
                body_width_px,
            )
        }))?;

        // ─── CPR lines ───────────────────────────────────────────────────────
        if overlay_cpr && let Some(cpr) = cpr {
            let mut levels: Vec<(f64, RGBColor, &'static str)> = Vec::new();
            if let Some(v) = cpr.pivot {
                levels.push((v, CPR_PIVOT, "P"));
            }
            if let Some(v) = cpr.tc {
                levels.push((v, CPR_TC, "TC"));
            }
            if let Some(v) = cpr.bc {
                levels.push((v, CPR_BC, "BC"));
            }
            if let Some(v) = cpr.s1 {
                levels.push((v, CPR_S1, "S1"));
            }
            if let Some(v) = cpr.s2 {
                levels.push((v, CPR_S2, "S2"));
            }
            if let Some(v) = cpr.s3 {
                levels.push((v, CPR_S3, "S3"));
            }
            if let Some(v) = cpr.r1 {
                levels.push((v, CPR_R1, "R1"));
            }
            if let Some(v) = cpr.r2 {
                levels.push((v, CPR_R2, "R2"));
            }
            if let Some(v) = cpr.r3 {
                levels.push((v, CPR_R3, "R3"));
            }

            for &(price, color, tag) in &levels {
                chart.draw_series(std::iter::once(PathElement::new(
                    vec![(time_min, price), (time_max, price)],
                    ShapeStyle {
                        color: color.to_rgba(),
                        filled: false,
                        stroke_width: 1,
                    },
                )))?;
                let label_text = format!("{} {}", tag, fmt_price(price));
                chart.draw_series(std::iter::once(Text::new(
                    label_text,
                    (time_max, price),
                    ("sans-serif", 10).into_font().color(&color),
                )))?;
            }
        }

        // ─── EMA ribbon ──────────────────────────────────────────────────────
        if overlay_ema && let Some(ribbon) = ema_ribbon {
            if !ribbon.fast.is_empty() {
                let fast_pts: Vec<(i64, f64)> = ribbon
                    .fast
                    .iter()
                    .filter(|p| p.time >= time_min && p.time <= time_max)
                    .map(|p| (p.time, p.value))
                    .collect();
                if !fast_pts.is_empty() {
                    chart
                        .draw_series(LineSeries::new(fast_pts, EMA_FAST.stroke_width(1)))?
                        .label("EMA fast")
                        .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 15, y)], EMA_FAST));
                }
            }
            if !ribbon.slow.is_empty() {
                let slow_pts: Vec<(i64, f64)> = ribbon
                    .slow
                    .iter()
                    .filter(|p| p.time >= time_min && p.time <= time_max)
                    .map(|p| (p.time, p.value))
                    .collect();
                if !slow_pts.is_empty() {
                    chart
                        .draw_series(LineSeries::new(slow_pts, EMA_SLOW.stroke_width(1)))?
                        .label("EMA slow")
                        .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 15, y)], EMA_SLOW));
                }
            }
        }

        // ─── ChartPrimitive overlays ──────────────────────────────────────────
        for prim in primitives {
            match prim {
                ChartPrimitive::Hline {
                    price,
                    color,
                    style,
                    label,
                } => {
                    let c = color.as_deref().map_or(PRIM_HLINE_DEFAULT, parse_css_color);
                    draw_styled_hline(&mut chart, time_min, time_max, *price, c, *style)?;
                    if let Some(lbl) = label
                        && !lbl.is_empty()
                    {
                        chart.draw_series(std::iter::once(Text::new(
                            lbl.clone(),
                            (time_max, *price),
                            ("sans-serif", 9).into_font().color(&c),
                        )))?;
                    }
                }

                ChartPrimitive::Polyline {
                    points,
                    color,
                    style,
                    label,
                } => {
                    if points.len() < 2 {
                        continue;
                    }
                    let c = color
                        .as_deref()
                        .map_or(PRIM_POLYLINE_DEFAULT, parse_css_color);
                    // Filter to only points within (or slightly beyond) the time range.
                    let clipped: Vec<(i64, f64)> = points
                        .iter()
                        .filter(|&&(t, _)| t >= time_min && t <= time_max)
                        .copied()
                        .collect();
                    let pts = if clipped.len() >= 2 {
                        &clipped
                    } else {
                        points.as_slice()
                    };
                    if pts.len() >= 2 {
                        draw_styled_polyline(&mut chart, pts, c, *style)?;
                    }
                    if let Some(lbl) = label
                        && !lbl.is_empty()
                        && let Some(&(last_t, last_p)) = points.last()
                    {
                        chart.draw_series(std::iter::once(Text::new(
                            lbl.clone(),
                            (last_t, last_p),
                            ("sans-serif", 9).into_font().color(&c),
                        )))?;
                    }
                }

                ChartPrimitive::Rect {
                    p1,
                    p2,
                    color,
                    fill,
                    label,
                } => {
                    let c = color.as_deref().map_or(PRIM_RECT_DEFAULT, parse_css_color);

                    // Full-span sentinel: p1.0 == 0 && p2.0 == i64::MAX → clamp to chart extent.
                    let (t1, t2) = if p1.0 == 0 && p2.0 == i64::MAX {
                        (time_min, time_max)
                    } else {
                        (p1.0.max(time_min), p2.0.min(time_max))
                    };
                    let (y_lo, y_hi) = {
                        let a = p1.1.min(p2.1);
                        let b = p1.1.max(p2.1);
                        (a.max(price_lo * 0.5), b.min(price_hi * 2.0))
                    };

                    if *fill {
                        // Translucent fill: draw horizontal stripes every ~1% of price range.
                        let step = (price_hi - price_lo) * 0.01;
                        if step > 0.0 {
                            let mut y = y_lo;
                            while y <= y_hi {
                                chart.draw_series(std::iter::once(PathElement::new(
                                    vec![(t1, y), (t2, y)],
                                    ShapeStyle {
                                        color: c.mix(0.25).to_rgba(),
                                        filled: false,
                                        stroke_width: 1,
                                    },
                                )))?;
                                y += step;
                            }
                        }
                    }
                    // Always draw the border edges (4 sides).
                    // Top edge
                    chart.draw_series(std::iter::once(PathElement::new(
                        vec![(t1, y_hi), (t2, y_hi)],
                        ShapeStyle {
                            color: c.mix(0.85).to_rgba(),
                            filled: false,
                            stroke_width: 1,
                        },
                    )))?;
                    // Bottom edge
                    chart.draw_series(std::iter::once(PathElement::new(
                        vec![(t1, y_lo), (t2, y_lo)],
                        ShapeStyle {
                            color: c.mix(0.85).to_rgba(),
                            filled: false,
                            stroke_width: 1,
                        },
                    )))?;
                    // Left edge
                    chart.draw_series(std::iter::once(PathElement::new(
                        vec![(t1, y_lo), (t1, y_hi)],
                        ShapeStyle {
                            color: c.mix(0.85).to_rgba(),
                            filled: false,
                            stroke_width: 1,
                        },
                    )))?;
                    // Right edge
                    chart.draw_series(std::iter::once(PathElement::new(
                        vec![(t2, y_lo), (t2, y_hi)],
                        ShapeStyle {
                            color: c.mix(0.85).to_rgba(),
                            filled: false,
                            stroke_width: 1,
                        },
                    )))?;

                    if let Some(lbl) = label
                        && !lbl.is_empty()
                    {
                        let mid_y = f64::midpoint(y_lo, y_hi);
                        chart.draw_series(std::iter::once(Text::new(
                            lbl.clone(),
                            (t2, mid_y),
                            ("sans-serif", 9).into_font().color(&c),
                        )))?;
                    }
                }

                ChartPrimitive::Marker {
                    point: (pt, pp),
                    text,
                    color,
                } => {
                    let c = color
                        .as_deref()
                        .map_or(PRIM_MARKER_DEFAULT, parse_css_color);
                    // Small filled circle — approximated by a very short thick segment.
                    chart.draw_series(std::iter::once(PathElement::new(
                        vec![(*pt, *pp), (*pt + 1, *pp)],
                        ShapeStyle {
                            color: c.to_rgba(),
                            filled: true,
                            stroke_width: 6,
                        },
                    )))?;
                    if !text.is_empty() {
                        chart.draw_series(std::iter::once(Text::new(
                            text.clone(),
                            (*pt, *pp),
                            ("sans-serif", 9).into_font().color(&c),
                        )))?;
                    }
                }
            }
        }

        // ─── Legend ───────────────────────────────────────────────────────────
        if overlay_cpr || overlay_ema {
            chart
                .configure_series_labels()
                .background_style(BG.mix(0.8))
                .border_style(AXIS_COLOR)
                .label_font(("sans-serif", 10).into_font().color(&TEXT_COLOR))
                .position(SeriesLabelPosition::UpperLeft)
                .draw()?;
        }

        root.present()?;
    }

    // Encode the raw RGB pixel buffer to PNG bytes.
    let mut png_bytes: Vec<u8> = Vec::new();
    {
        let encoder = image::codecs::png::PngEncoder::new(&mut png_bytes);
        use image::ImageEncoder;
        encoder.write_image(&buf, width, height, image::ColorType::Rgb8.into())?;
    }
    Ok(png_bytes)
}

// ─── Main render function ─────────────────────────────────────────────────────

/// Render candles + optional CPR + optional EMA ribbon + user drawings into a PNG byte buffer.
///
/// Returns the raw PNG bytes. `width` and `height` are in pixels.
#[allow(dead_code)] // retained as a tested simple-path renderer; production uses render_png_rich
#[allow(clippy::too_many_arguments)] // render entry point; a params struct adds churn without clarity
pub fn render_png(
    candles: &[Candle],
    cpr: Option<&CprData>,
    ema_ribbon: Option<&EmaRibbon>,
    overlay_cpr: bool,
    overlay_ema: bool,
    drawings: &[Drawing],
    width: u32,
    height: u32,
) -> Result<Vec<u8>> {
    if candles.is_empty() {
        return Err(anyhow!("no candle data to render"));
    }
    ensure_font();

    // Price range — pad by 3% so candles aren't flush against the edges.
    let price_min = candles.iter().map(|c| c.low).fold(f64::INFINITY, f64::min);
    let price_max = candles
        .iter()
        .map(|c| c.high)
        .fold(f64::NEG_INFINITY, f64::max);
    let pad = (price_max - price_min) * 0.03;
    let price_lo = price_min - pad;
    let price_hi = price_max + pad;

    // Time range (UNIX timestamps as i64).
    let time_min = candles.first().map_or(0, |c| c.time);
    let time_max = candles.last().map_or(0, |c| c.time);

    // Allocate an in-memory pixel buffer and write PNG into it.
    let mut buf: Vec<u8> = vec![0u8; (width * height * 3) as usize];
    {
        let root = BitMapBackend::with_buffer(&mut buf, (width, height)).into_drawing_area();
        root.fill(&BG)?;

        let mut chart = ChartBuilder::on(&root)
            .margin(16)
            .x_label_area_size(30)
            .y_label_area_size(70)
            .build_cartesian_2d(time_min..time_max, price_lo..price_hi)?;

        chart
            .configure_mesh()
            .disable_x_mesh()
            .disable_y_mesh()
            .axis_style(AXIS_COLOR)
            .label_style(("sans-serif", 11).into_font().color(&TEXT_COLOR))
            .x_labels(6)
            .y_labels(8)
            .x_label_formatter(&|ts| {
                let dt = chrono::DateTime::from_timestamp(*ts, 0);
                match dt {
                    Some(d) => d.format("%m-%d").to_string(),
                    None => String::new(),
                }
            })
            .y_label_formatter(&|price| fmt_price(*price))
            .draw()?;

        // Candle body width in pixels.
        let draw_width_px = f64::from(width.saturating_sub(16 * 2 + 70));
        let slot_px = draw_width_px / candles.len().max(1) as f64;
        let body_width_px = ((slot_px * 0.70) as u32).max(1);

        // Draw candle series.
        chart.draw_series(candles.iter().map(|c| {
            let color = if c.close >= c.open {
                CANDLE_UP
            } else {
                CANDLE_DOWN
            };
            CandleStick::new(
                c.time,
                c.open,
                c.high,
                c.low,
                c.close,
                color.filled(),
                color.stroke_width(1),
                body_width_px,
            )
        }))?;

        // ─── CPR lines ───────────────────────────────────────────────────────
        if overlay_cpr && let Some(cpr) = cpr {
            // Core trio: P (yellow), TC (light blue), BC (salmon).
            // Then S1/S2/S3 (red, dimming) and R1/R2/R3 (green, dimming) if available.
            // CprData fields are all Option<f64>; only render levels that are present.
            let mut levels: Vec<(f64, RGBColor, &'static str)> = Vec::new();
            if let Some(v) = cpr.pivot {
                levels.push((v, CPR_PIVOT, "P"));
            }
            if let Some(v) = cpr.tc {
                levels.push((v, CPR_TC, "TC"));
            }
            if let Some(v) = cpr.bc {
                levels.push((v, CPR_BC, "BC"));
            }
            if let Some(v) = cpr.s1 {
                levels.push((v, CPR_S1, "S1"));
            }
            if let Some(v) = cpr.s2 {
                levels.push((v, CPR_S2, "S2"));
            }
            if let Some(v) = cpr.s3 {
                levels.push((v, CPR_S3, "S3"));
            }
            if let Some(v) = cpr.r1 {
                levels.push((v, CPR_R1, "R1"));
            }
            if let Some(v) = cpr.r2 {
                levels.push((v, CPR_R2, "R2"));
            }
            if let Some(v) = cpr.r3 {
                levels.push((v, CPR_R3, "R3"));
            }

            for &(price, color, tag) in &levels {
                chart.draw_series(std::iter::once(PathElement::new(
                    vec![(time_min, price), (time_max, price)],
                    ShapeStyle {
                        color: color.to_rgba(),
                        filled: false,
                        stroke_width: 1,
                    },
                )))?;
                let label_text = format!("{} {}", tag, fmt_price(price));
                chart.draw_series(std::iter::once(Text::new(
                    label_text,
                    (time_max, price),
                    ("sans-serif", 10).into_font().color(&color),
                )))?;
            }
        }

        // ─── EMA ribbon ──────────────────────────────────────────────────────
        if overlay_ema && let Some(ribbon) = ema_ribbon {
            if !ribbon.fast.is_empty() {
                let fast_pts: Vec<(i64, f64)> = ribbon
                    .fast
                    .iter()
                    .filter(|p| p.time >= time_min && p.time <= time_max)
                    .map(|p| (p.time, p.value))
                    .collect();
                if !fast_pts.is_empty() {
                    chart
                        .draw_series(LineSeries::new(fast_pts, EMA_FAST.stroke_width(1)))?
                        .label("EMA fast")
                        .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 15, y)], EMA_FAST));
                }
            }
            if !ribbon.slow.is_empty() {
                let slow_pts: Vec<(i64, f64)> = ribbon
                    .slow
                    .iter()
                    .filter(|p| p.time >= time_min && p.time <= time_max)
                    .map(|p| (p.time, p.value))
                    .collect();
                if !slow_pts.is_empty() {
                    chart
                        .draw_series(LineSeries::new(slow_pts, EMA_SLOW.stroke_width(1)))?
                        .label("EMA slow")
                        .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 15, y)], EMA_SLOW));
                }
            }
        }

        // ─── User drawings overlay ────────────────────────────────────────────
        // 5% margin above/below the visible candle range. Drawings whose primary
        // price falls outside this window are skipped entirely — without the
        // filter, BTC accumulates years of out-of-frame SL/TP/zone lines that
        // get compressed to the edges and clutter the rendered PNG.
        let margin = (price_hi - price_lo) * 0.05;
        let visible_lo = price_lo - margin;
        let visible_hi = price_hi + margin;
        let in_visible_range = |p: f64| p >= visible_lo && p <= visible_hi;

        for drawing in drawings {
            // Cull drawings completely outside the visible price window: keep a
            // drawing if either its primary or upper price level is in range, so
            // cross-window trendlines and zones still draw.
            let any_level_in_range = drawing.price_level.is_some_and(in_visible_range)
                || drawing.price_level_upper.is_some_and(in_visible_range);
            if !any_level_in_range {
                continue;
            }

            // Clamp prices to chart range so in-range drawings with one out-of-range
            // endpoint (e.g. trendline crossing the window) don't crash.
            let clamp = |p: f64| p.max(price_lo * 0.5).min(price_hi * 2.0);

            match drawing.drawing_type.as_str() {
                "horizontal_line" => {
                    let price = match drawing.price_level {
                        Some(p) => clamp(p),
                        None => continue,
                    };
                    let color = drawing.color.as_deref().map_or(DRW_HLINE, parse_css_color);
                    chart.draw_series(std::iter::once(PathElement::new(
                        vec![(time_min, price), (time_max, price)],
                        ShapeStyle {
                            color: color.to_rgba(),
                            filled: false,
                            stroke_width: 1,
                        },
                    )))?;
                    if let Some(label) = &drawing.label
                        && !label.is_empty()
                    {
                        chart.draw_series(std::iter::once(Text::new(
                            label.clone(),
                            (time_max, price),
                            ("sans-serif", 9).into_font().color(&color),
                        )))?;
                    }
                }

                "trendline" => {
                    // Try metadata timestamps first, fall back to full-span.
                    let (t1, t2) = if let Some(meta) = &drawing.metadata {
                        let ts1 = meta
                            .get("point1_timestamp")
                            .and_then(serde_json::Value::as_i64)
                            .unwrap_or(time_min);
                        let ts2 = meta
                            .get("point2_timestamp")
                            .and_then(serde_json::Value::as_i64)
                            .unwrap_or(time_max);
                        (ts1, ts2)
                    } else {
                        (time_min, time_max)
                    };
                    let v1 = match drawing.price_level {
                        Some(p) => clamp(p),
                        None => continue,
                    };
                    let v2 = drawing.price_level_upper.map_or(v1, clamp);
                    let color = drawing.color.as_deref().map_or(DRW_TREND, parse_css_color);
                    chart.draw_series(std::iter::once(PathElement::new(
                        vec![(t1, v1), (t2, v2)],
                        ShapeStyle {
                            color: color.to_rgba(),
                            filled: false,
                            stroke_width: 1,
                        },
                    )))?;
                    if let Some(label) = &drawing.label
                        && !label.is_empty()
                    {
                        chart.draw_series(std::iter::once(Text::new(
                            label.clone(),
                            (t2, v2),
                            ("sans-serif", 9).into_font().color(&color),
                        )))?;
                    }
                }

                "zone" | "rectangle" => {
                    let bottom = match drawing.price_level {
                        Some(p) => clamp(p),
                        None => continue,
                    };
                    let top = match drawing.price_level_upper {
                        Some(p) => clamp(p),
                        None => continue,
                    };
                    let color = drawing.color.as_deref().map_or(DRW_ZONE, parse_css_color);
                    // Two boundary lines + semi-transparent fill approximated by two more lines.
                    for &price in &[bottom, top] {
                        chart.draw_series(std::iter::once(PathElement::new(
                            vec![(time_min, price), (time_max, price)],
                            ShapeStyle {
                                color: color.mix(0.8).to_rgba(),
                                filled: false,
                                stroke_width: 1,
                            },
                        )))?;
                    }
                    // Label at midpoint.
                    if let Some(label) = &drawing.label
                        && !label.is_empty()
                    {
                        let mid = f64::midpoint(bottom, top);
                        chart.draw_series(std::iter::once(Text::new(
                            label.clone(),
                            (time_max, mid),
                            ("sans-serif", 9).into_font().color(&color),
                        )))?;
                    }
                }

                "liquidation_zone" => {
                    let bottom = match drawing.price_level {
                        Some(p) => clamp(p),
                        None => continue,
                    };
                    let top = match drawing.price_level_upper {
                        Some(p) => clamp(p),
                        None => continue,
                    };
                    // Gold dashed-style (plotters doesn't do native dash, use mix for dimmer look)
                    let color = RGBColor(255, 215, 0);
                    for &price in &[bottom, top] {
                        chart.draw_series(std::iter::once(PathElement::new(
                            vec![(time_min, price), (time_max, price)],
                            ShapeStyle {
                                color: color.mix(0.5).to_rgba(),
                                filled: false,
                                stroke_width: 1,
                            },
                        )))?;
                    }
                }

                "position" => {
                    // Position drawing: points[0]=entry, points[1]=SL, points[2]=TP
                    // Also handles kline_name="position" in metadata.
                    // Try to extract from metadata.points if present.
                    let points = drawing
                        .metadata
                        .as_ref()
                        .and_then(|m| m.get("points"))
                        .and_then(|p| p.as_array());

                    if let Some(pts) = points {
                        // Expect [{value: entry}, {value: sl}, {value: tp}]
                        let get_val = |i: usize| {
                            pts.get(i)
                                .and_then(|p| p.get("value").or_else(|| p.get("price")))
                                .and_then(serde_json::Value::as_f64)
                                .map(clamp)
                        };
                        let entry = get_val(0);
                        let sl = get_val(1);
                        let tp = get_val(2);

                        for (opt_price, color, tag) in [
                            (entry, DRW_ENTRY, "Entry"),
                            (sl, DRW_SL, "SL"),
                            (tp, DRW_TP, "TP"),
                        ] {
                            if let Some(price) = opt_price {
                                chart.draw_series(std::iter::once(PathElement::new(
                                    vec![(time_min, price), (time_max, price)],
                                    ShapeStyle {
                                        color: color.to_rgba(),
                                        filled: false,
                                        stroke_width: 1,
                                    },
                                )))?;
                                chart.draw_series(std::iter::once(Text::new(
                                    format!("{} {}", tag, fmt_price(price)),
                                    (time_max, price),
                                    ("sans-serif", 9).into_font().color(&color),
                                )))?;
                            }
                        }
                    } else if let Some(price) = drawing.price_level.map(clamp) {
                        // Fallback: just draw the entry line.
                        chart.draw_series(std::iter::once(PathElement::new(
                            vec![(time_min, price), (time_max, price)],
                            ShapeStyle {
                                color: DRW_ENTRY.to_rgba(),
                                filled: false,
                                stroke_width: 1,
                            },
                        )))?;
                    }
                }

                // Deferred / unsupported types — skip gracefully.
                // fibonacci, scenarioPath, boundedChannel, parallelChannel, etc.
                // are KlineChart-specific spline/multi-point types not trivially
                // renderable with plotters line primitives.
                other => {
                    eprintln!(
                        "carrac chart --png: drawing type '{other}' not rendered (unsupported); skipping."
                    );
                }
            }
        }

        // ─── Legend ───────────────────────────────────────────────────────────
        if overlay_cpr || overlay_ema {
            chart
                .configure_series_labels()
                .background_style(BG.mix(0.8))
                .border_style(AXIS_COLOR)
                .label_font(("sans-serif", 10).into_font().color(&TEXT_COLOR))
                .position(SeriesLabelPosition::UpperLeft)
                .draw()?;
        }

        root.present()?;
    }

    // Encode the raw RGB pixel buffer to PNG bytes.
    let mut png_bytes: Vec<u8> = Vec::new();
    {
        let encoder = image::codecs::png::PngEncoder::new(&mut png_bytes);
        use image::ImageEncoder;
        encoder.write_image(&buf, width, height, image::ColorType::Rgb8.into())?;
    }
    Ok(png_bytes)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::chart::{Candle, CprData};

    fn synthetic_candles(n: usize) -> Vec<Candle> {
        let mut price = 100.0_f64;
        (0..n)
            .map(|i| {
                let open = price;
                price *= 1.0 + (if i % 2 == 0 { 0.01 } else { -0.005 });
                let close = price;
                Candle {
                    time: 1_700_000_000 + i as i64 * 14400,
                    open,
                    high: open.max(close) * 1.005,
                    low: open.min(close) * 0.995,
                    close,
                }
            })
            .collect()
    }

    #[test]
    fn render_png_returns_valid_png() {
        let candles = synthetic_candles(50);
        let cpr = CprData {
            pivot: Some(110.0),
            tc: Some(115.0),
            bc: Some(105.0),
            period_start: None,
            s1: None,
            s2: None,
            s3: None,
            r1: None,
            r2: None,
            r3: None,
        };
        let result = render_png(&candles, Some(&cpr), None, true, false, &[], 800, 400);
        assert!(result.is_ok(), "render_png failed: {:?}", result.err());
        let bytes = result.unwrap();
        // PNG magic bytes: 0x89 50 4e 47
        assert_eq!(
            &bytes[0..4],
            &[0x89, 0x50, 0x4e, 0x47],
            "not a valid PNG header"
        );
        assert!(bytes.len() > 1000, "PNG too small ({} bytes)", bytes.len());
    }

    #[test]
    fn render_png_empty_candles_errors() {
        let result = render_png(&[], None, None, false, false, &[], 800, 400);
        assert!(result.is_err());
    }

    #[test]
    fn render_png_no_overlays() {
        let candles = synthetic_candles(10);
        let result = render_png(&candles, None, None, false, false, &[], 400, 200);
        assert!(result.is_ok());
    }

    #[test]
    fn render_png_with_drawings() {
        let candles = synthetic_candles(50);
        let drawings = vec![
            Drawing {
                drawing_type: "horizontal_line".to_string(),
                price_level: Some(105.0),
                price_level_upper: None,
                color: Some("#ff0000".to_string()),
                label: Some("Resistance".to_string()),
                metadata: None,
            },
            Drawing {
                drawing_type: "zone".to_string(),
                price_level: Some(98.0),
                price_level_upper: Some(102.0),
                color: Some("#ffa500".to_string()),
                label: Some("Support Zone".to_string()),
                metadata: None,
            },
        ];
        let result = render_png(&candles, None, None, false, false, &drawings, 800, 400);
        assert!(
            result.is_ok(),
            "render_png with drawings failed: {:?}",
            result.err()
        );
        let bytes = result.unwrap();
        assert_eq!(&bytes[0..4], &[0x89, 0x50, 0x4e, 0x47]);
    }

    #[test]
    fn candle_body_width_scales_with_bar_count() {
        let chart_w: u32 = 1200;
        let draw_w = f64::from(chart_w.saturating_sub(16 * 2 + 70));

        let bw_200 = ((draw_w / 200.0 * 0.70) as u32).max(1);
        let bw_50 = ((draw_w / 50.0 * 0.70) as u32).max(1);
        let bw_365 = ((draw_w / 365.0 * 0.70) as u32).max(1);

        assert!(
            bw_50 > bw_200,
            "50-bar body ({bw_50}px) should be wider than 200-bar ({bw_200}px)"
        );
        assert!(
            bw_200 < 10,
            "200-bar body ({bw_200}px) must not span the chart width"
        );
        assert!(
            bw_50 < 30,
            "50-bar body ({bw_50}px) must not span the chart width"
        );
        assert!(
            bw_365 >= 1,
            "365-bar body ({bw_365}px) must be at least 1px"
        );
    }

    // ─── render_png_rich tests ────────────────────────────────────────────────

    /// Build a set of fib Hlines (5 levels) for tests.
    fn fib_hlines(price_lo: f64, price_hi: f64) -> Vec<ChartPrimitive> {
        let levels = [0.236, 0.382, 0.5, 0.618, 0.786];
        levels
            .iter()
            .map(|&r| {
                let price = price_lo + r * (price_hi - price_lo);
                ChartPrimitive::Hline {
                    price,
                    color: Some("#aa8833".to_string()),
                    style: LineStyle::Dashed,
                    label: Some(format!("Fib {:.1}%", r * 100.0)),
                }
            })
            .collect()
    }

    #[test]
    fn render_png_rich_mixed_primitives() {
        let candles = synthetic_candles(50);
        let price_lo = candles.iter().map(|c| c.low).fold(f64::INFINITY, f64::min);
        let price_hi = candles
            .iter()
            .map(|c| c.high)
            .fold(f64::NEG_INFINITY, f64::max);
        let time_mid = candles[25].time;

        let mut primitives: Vec<ChartPrimitive> = vec![
            // One dashed Hline with label.
            ChartPrimitive::Hline {
                price: price_lo + (price_hi - price_lo) * 0.6,
                color: Some("#ff4444".to_string()),
                style: LineStyle::Dashed,
                label: Some("Resistance".to_string()),
            },
            // One Polyline (3 points).
            ChartPrimitive::Polyline {
                points: vec![
                    (candles[10].time, price_lo + (price_hi - price_lo) * 0.4),
                    (time_mid, price_lo + (price_hi - price_lo) * 0.55),
                    (candles[40].time, price_lo + (price_hi - price_lo) * 0.45),
                ],
                color: Some("#44aaff".to_string()),
                style: LineStyle::Solid,
                label: Some("Trend".to_string()),
            },
            // One filled Rect.
            ChartPrimitive::Rect {
                p1: (candles[5].time, price_lo + (price_hi - price_lo) * 0.3),
                p2: (candles[15].time, price_lo + (price_hi - price_lo) * 0.45),
                color: Some("#ffaa00".to_string()),
                fill: true,
                label: Some("Zone".to_string()),
            },
            // Full-span sentinel Rect (p1.0 == 0, p2.0 == i64::MAX).
            ChartPrimitive::Rect {
                p1: (0, price_lo + (price_hi - price_lo) * 0.2),
                p2: (i64::MAX, price_lo + (price_hi - price_lo) * 0.25),
                color: Some("#88ff88".to_string()),
                fill: false,
                label: Some("Full span".to_string()),
            },
            // One Marker.
            ChartPrimitive::Marker {
                point: (time_mid, price_lo + (price_hi - price_lo) * 0.7),
                text: "Entry".to_string(),
                color: Some("#00ff88".to_string()),
            },
        ];

        // Five fib Hlines.
        primitives.extend(fib_hlines(price_lo, price_hi));

        let result = render_png_rich(&candles, None, None, false, false, &primitives, 900, 600);
        assert!(result.is_ok(), "render_png_rich failed: {:?}", result.err());
        let bytes = result.unwrap();
        assert_eq!(
            &bytes[0..4],
            &[0x89, 0x50, 0x4e, 0x47],
            "not a valid PNG header"
        );
        assert!(bytes.len() > 1000, "PNG too small ({} bytes)", bytes.len());
    }

    #[test]
    fn render_png_rich_empty_primitives() {
        let candles = synthetic_candles(30);
        let result = render_png_rich(&candles, None, None, false, false, &[], 800, 400);
        assert!(
            result.is_ok(),
            "render_png_rich (no overlays) failed: {:?}",
            result.err()
        );
        let bytes = result.unwrap();
        assert_eq!(&bytes[0..4], &[0x89, 0x50, 0x4e, 0x47]);
    }
}
