/// Medium-agnostic chart primitives consumed by both the PNG and terminal renderers.
///
/// Two pure adapter functions convert server data into `ChartPrimitive` slices:
///
/// - [`drawings_to_primitives`] — classifies user drawings by `drawing_type`
/// - [`overlays_to_primitives`] — emits price-pane indicator overlay series
///
/// Neither function panics on missing / malformed data; unknown drawing types are
/// silently skipped.  All timestamps are **UNIX seconds** (same unit as `Candle.time`
/// and `metadata.points[].timestamp` in render.rs).
use crate::chart::data::{ChartResponse, SeriesPoint};
use crate::chart::render::Drawing;

// ─── Line style ───────────────────────────────────────────────────────────────

/// How a line segment should be rendered visually.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineStyle {
    #[default]
    Solid,
    Dashed,
    Dotted,
}

// ─── ChartPrimitive ───────────────────────────────────────────────────────────

/// A medium-agnostic chart drawing primitive.
///
/// - `i64` timestamps are UNIX **seconds** throughout (match `Candle.time`).
/// - Colors are optional CSS hex strings (`"#rrggbb"`) — renderers supply a
///   default when `None`.
#[derive(Debug, Clone)]
pub enum ChartPrimitive {
    /// An infinite horizontal line at a fixed price.
    Hline {
        price: f64,
        color: Option<String>,
        style: LineStyle,
        label: Option<String>,
    },
    /// A connected poly-line through a sequence of `(timestamp_s, price)` points.
    Polyline {
        points: Vec<(i64, f64)>,
        color: Option<String>,
        style: LineStyle,
        label: Option<String>,
    },
    /// An axis-aligned rectangle defined by two corner points.
    Rect {
        p1: (i64, f64),
        p2: (i64, f64),
        color: Option<String>,
        fill: bool,
        label: Option<String>,
    },
    /// A point annotation (text label placed at a chart coordinate).
    Marker {
        point: (i64, f64),
        text: String,
        color: Option<String>,
    },
}

// ─── Fibonacci ratios ─────────────────────────────────────────────────────────

const FIB_RATIOS: &[f64] = &[0.0, 0.236, 0.382, 0.5, 0.618, 0.786, 1.0];

// ─── drawings_to_primitives ───────────────────────────────────────────────────

/// Convert a slice of user drawings (from `/api/drawings`) into chart primitives.
///
/// Classification is by `drawing_type` (and `metadata.kline_name` for channel
/// types).  Any drawing with missing required fields, or an unknown type, is
/// silently skipped — the function never panics.
pub fn drawings_to_primitives(drawings: &[Drawing]) -> Vec<ChartPrimitive> {
    let mut out: Vec<ChartPrimitive> = Vec::with_capacity(drawings.len() * 2);

    for d in drawings {
        let color = d.color.clone();

        match d.drawing_type.as_str() {
            // ── Horizontal line ──────────────────────────────────────────────
            "horizontal_line" => {
                let Some(price) = d.price_level else { continue };
                out.push(ChartPrimitive::Hline {
                    price,
                    color,
                    style: LineStyle::default(),
                    label: d.label.clone(),
                });
            }

            // ── Trendline ────────────────────────────────────────────────────
            "trendline" => {
                // Prefer metadata.points[]; fall back to point1/point2_timestamp +
                // price_level / price_level_upper (the shape render.rs already uses).
                let pts = extract_metadata_points(d);
                if pts.len() >= 2 {
                    out.push(ChartPrimitive::Polyline {
                        points: pts,
                        color,
                        style: LineStyle::default(),
                        label: d.label.clone(),
                    });
                } else {
                    // Fallback to point1_timestamp / point2_timestamp
                    let (t1, t2) = extract_point_timestamps(d);
                    let Some(v1) = d.price_level else { continue };
                    let v2 = d.price_level_upper.unwrap_or(v1);
                    out.push(ChartPrimitive::Polyline {
                        points: vec![(t1, v1), (t2, v2)],
                        color,
                        style: LineStyle::default(),
                        label: d.label.clone(),
                    });
                }
            }

            // ── Zone / rectangle / liquidation zone ──────────────────────────
            "zone" | "rectangle" | "liquidation_zone" => {
                // Try metadata.points[] (4 corners); fall back to price_level +
                // price_level_upper spanning the full visible time range.
                // We emit a time-spanning Rect so renderers can choose how to draw
                // it (PNG fills it, terminal draws two hlines).
                let pts = extract_metadata_points(d);
                if pts.len() >= 2 {
                    let t1 = pts.iter().map(|p| p.0).min().unwrap_or(0);
                    let t2 = pts.iter().map(|p| p.0).max().unwrap_or(0);
                    let v_min = pts.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
                    let v_max = pts.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
                    out.push(ChartPrimitive::Rect {
                        p1: (t1, v_min),
                        p2: (t2, v_max),
                        color,
                        fill: true,
                        label: d.label.clone(),
                    });
                } else {
                    let Some(bottom) = d.price_level else {
                        continue;
                    };
                    let top = d.price_level_upper.unwrap_or(bottom);
                    // t=0 / t=i64::MAX is a sentinel for "full chart width"
                    out.push(ChartPrimitive::Rect {
                        p1: (0, bottom.min(top)),
                        p2: (i64::MAX, bottom.max(top)),
                        color,
                        fill: true,
                        label: d.label.clone(),
                    });
                }
            }

            // ── Fibonacci retracement ────────────────────────────────────────
            "fibonacci" => {
                let pts = extract_metadata_points(d);
                if pts.len() < 2 {
                    continue;
                }
                let (_, v_anchor1) = pts[0];
                let (_, v_anchor2) = pts[1];
                for &ratio in FIB_RATIOS {
                    let price = v_anchor1 + (v_anchor2 - v_anchor1) * ratio;
                    out.push(ChartPrimitive::Hline {
                        price,
                        color: color.clone(),
                        style: LineStyle::default(),
                        label: Some(format!("{:.3}", ratio)),
                    });
                }
            }

            // ── Position / long_position / short_position ────────────────────
            "position" | "long_position" | "short_position" => {
                let pts = extract_metadata_points(d);
                if pts.len() >= 3 {
                    let (entry_color, sl_color, tp_color) = position_colors(&d.drawing_type);
                    let labels = ["Entry", "SL", "TP"];
                    let colors = [entry_color, sl_color, tp_color];
                    for i in 0..3 {
                        let (_, price) = pts[i];
                        out.push(ChartPrimitive::Hline {
                            price,
                            color: Some(colors[i].to_string()),
                            style: LineStyle::default(),
                            label: Some(labels[i].to_string()),
                        });
                    }
                } else if let Some(price) = d.price_level {
                    // Minimal fallback — just the entry line.
                    out.push(ChartPrimitive::Hline {
                        price,
                        color: Some("#50dc78".to_string()), // green entry
                        style: LineStyle::default(),
                        label: Some("Entry".to_string()),
                    });
                }
            }

            // ── Scenario / scenario path ─────────────────────────────────────
            "scenario" | "scenarioPath" => {
                let pts = extract_metadata_points(d);
                if pts.len() >= 2 {
                    out.push(ChartPrimitive::Polyline {
                        points: pts,
                        color,
                        style: LineStyle::Dashed,
                        label: d.label.clone(),
                    });
                }
            }

            // ── Channel types ────────────────────────────────────────────────
            // parallelChannel / boundedChannel / pitchfork: connect anchor points
            // as one or more polylines.  We keep it simple — one Polyline per
            // consecutive pair of points (renderers can optionally close the shape).
            dt if is_channel_type(dt, d) => {
                let pts = extract_metadata_points(d);
                if pts.len() >= 2 {
                    out.push(ChartPrimitive::Polyline {
                        points: pts,
                        color,
                        style: LineStyle::default(),
                        label: d.label.clone(),
                    });
                }
            }

            // ── Annotation / spot ────────────────────────────────────────────
            "annotation" | "spot" => {
                let pts = extract_metadata_points(d);
                let (t, v) = pts.first().copied().unwrap_or_else(|| {
                    // Fallback: t=0 (renderer will position at the last candle),
                    // v from price_level
                    (0, d.price_level.unwrap_or(0.0))
                });
                // Text from label, or from metadata.extend_data.text.
                let text = d
                    .label
                    .clone()
                    .or_else(|| {
                        d.metadata
                            .as_ref()
                            .and_then(|m| m.get("extend_data"))
                            .and_then(|e| e.get("text"))
                            .and_then(|t| t.as_str())
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| d.drawing_type.clone());
                out.push(ChartPrimitive::Marker {
                    point: (t, v),
                    text,
                    color,
                });
            }

            // ── Unknown — skip silently ───────────────────────────────────────
            _ => {}
        }
    }

    out
}

// ─── overlays_to_primitives ───────────────────────────────────────────────────

/// Convert price-pane overlay series from a `ChartResponse` into chart primitives.
///
/// Covers: EMA ribbon (fast+slow), weeklyMAs (3 series), bullMarketBand (2),
/// smaLines (2), server `priceLines`, and `markers`.
///
/// Sub-pane series (MACD, CVD, OI, quant) are intentionally excluded here —
/// they belong to a different rendering path.
pub fn overlays_to_primitives(resp: &ChartResponse) -> Vec<ChartPrimitive> {
    let mut out: Vec<ChartPrimitive> = Vec::new();

    if let Some(ind) = &resp.indicators {
        // ── EMA ribbon ───────────────────────────────────────────────────────
        if let Some(ribbon) = &ind.ema_ribbon {
            if let Some(fast) = &ribbon.fast {
                let pts = series_to_points(fast);
                if !pts.is_empty() {
                    out.push(ChartPrimitive::Polyline {
                        points: pts,
                        color: Some("#f3de2c".to_string()), // yellow — matches EMA_FAST in render.rs
                        style: LineStyle::Solid,
                        label: Some("EMA fast".to_string()),
                    });
                }
            }
            if let Some(slow) = &ribbon.slow {
                let pts = series_to_points(slow);
                if !pts.is_empty() {
                    out.push(ChartPrimitive::Polyline {
                        points: pts,
                        color: Some("#3cbe78".to_string()), // green — matches EMA_SLOW in render.rs
                        style: LineStyle::Solid,
                        label: Some("EMA slow".to_string()),
                    });
                }
            }
        }

        // ── Weekly MAs ───────────────────────────────────────────────────────
        if let Some(wma) = &ind.weekly_mas {
            for (series, color, label) in [
                (wma.wma50.as_deref(), "#ff9900", "WMA50"),
                (wma.wma100.as_deref(), "#ff6600", "WMA100"),
                (wma.wma200.as_deref(), "#cc3300", "WMA200"),
            ] {
                if let Some(s) = series {
                    let pts = series_to_points(s);
                    if !pts.is_empty() {
                        out.push(ChartPrimitive::Polyline {
                            points: pts,
                            color: Some(color.to_string()),
                            style: LineStyle::Solid,
                            label: Some(label.to_string()),
                        });
                    }
                }
            }
        }

        // ── Bull Market Band ─────────────────────────────────────────────────
        if let Some(bmb) = &ind.bull_market_band {
            for (series, color, label) in [
                (bmb.ema21w.as_deref(), "#a855f7", "EMA21W"),
                (bmb.sma20w.as_deref(), "#7c3aed", "SMA20W"),
            ] {
                if let Some(s) = series {
                    let pts = series_to_points(s);
                    if !pts.is_empty() {
                        out.push(ChartPrimitive::Polyline {
                            points: pts,
                            color: Some(color.to_string()),
                            style: LineStyle::Solid,
                            label: Some(label.to_string()),
                        });
                    }
                }
            }
        }

        // ── SMA lines ────────────────────────────────────────────────────────
        if let Some(sma) = &ind.sma_lines {
            for (series, color, label) in [
                (sma.sma20.as_deref(), "#60b4ff", "SMA20"),
                (sma.sma200.as_deref(), "#2060aa", "SMA200"),
            ] {
                if let Some(s) = series {
                    let pts = series_to_points(s);
                    if !pts.is_empty() {
                        out.push(ChartPrimitive::Polyline {
                            points: pts,
                            color: Some(color.to_string()),
                            style: LineStyle::Solid,
                            label: Some(label.to_string()),
                        });
                    }
                }
            }
        }
    }

    // ── Server price lines ────────────────────────────────────────────────────
    for pl in &resp.price_lines {
        // KLineChart line-style enum int: 1=dashed, 2=dotted, else solid.
        let style = match pl.line_style {
            Some(1) => LineStyle::Dashed,
            Some(2) => LineStyle::Dotted,
            _ => LineStyle::Solid,
        };
        out.push(ChartPrimitive::Hline {
            price: pl.price,
            color: pl.color.clone(),
            style,
            label: pl.title.clone(),
        });
    }

    // ── Server markers ────────────────────────────────────────────────────────
    if let Some(markers) = &resp.markers {
        for m in markers {
            let text = m.text.clone().unwrap_or_else(|| "●".to_string());
            out.push(ChartPrimitive::Marker {
                point: (m.time, 0.0), // renderers position vertically at the bar price
                text,
                color: m.color.clone(),
            });
        }
    }

    out
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

/// Extract `metadata.points` as `Vec<(timestamp_s, value)>`.
/// Returns empty vec on any parse failure.
fn extract_metadata_points(d: &Drawing) -> Vec<(i64, f64)> {
    let arr = d
        .metadata
        .as_ref()
        .and_then(|m| m.get("points"))
        .and_then(|p| p.as_array());
    let Some(arr) = arr else { return vec![] };

    arr.iter()
        .filter_map(|p| {
            let t = p.get("timestamp").and_then(|v| v.as_i64())?;
            let v = p.get("value").and_then(|v| v.as_f64())?;
            Some((t, v))
        })
        .collect()
}

/// Read `metadata.point1_timestamp` / `metadata.point2_timestamp` (i64 seconds).
/// Returns (0, 0) if absent — callers use these only as fallback anchors.
fn extract_point_timestamps(d: &Drawing) -> (i64, i64) {
    let meta = match &d.metadata {
        Some(m) => m,
        None => return (0, 0),
    };
    let t1 = meta
        .get("point1_timestamp")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let t2 = meta
        .get("point2_timestamp")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    (t1, t2)
}

/// Convert a `&[SeriesPoint]` to `Vec<(i64, f64)>` (timestamp_s, value).
fn series_to_points(series: &[SeriesPoint]) -> Vec<(i64, f64)> {
    series.iter().map(|p| (p.time, p.value)).collect()
}

/// Return (entry_color, sl_color, tp_color) CSS hex strings for a position drawing.
fn position_colors(drawing_type: &str) -> (&'static str, &'static str, &'static str) {
    // short_position: entry is red (the short entry), SL above in red, TP below in teal.
    // All others (position / long_position): green entry, red SL, teal TP.
    match drawing_type {
        "short_position" => ("#ef5350", "#ef5350", "#26a69a"),
        _ => ("#50dc78", "#ef5350", "#26a69a"),
    }
}

/// Return true if the drawing type (or its kline_name) is a channel type.
fn is_channel_type(drawing_type: &str, d: &Drawing) -> bool {
    if matches!(
        drawing_type,
        "parallelChannel" | "boundedChannel" | "pitchfork"
    ) {
        return true;
    }
    // Also check metadata.kline_name for drawings whose server type is generic.
    d.metadata
        .as_ref()
        .and_then(|m| m.get("kline_name"))
        .and_then(|v| v.as_str())
        .map(|kn| matches!(kn, "parallelChannel" | "boundedChannel" | "pitchfork"))
        .unwrap_or(false)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── Test fixture builders ─────────────────────────────────────────────────

    fn make_drawing(
        drawing_type: &str,
        price_level: Option<f64>,
        price_level_upper: Option<f64>,
        metadata: Option<serde_json::Value>,
    ) -> Drawing {
        Drawing {
            drawing_type: drawing_type.to_string(),
            price_level,
            price_level_upper,
            color: None,
            label: None,
            metadata,
        }
    }

    fn meta_with_points(points: &[(i64, f64)]) -> serde_json::Value {
        let pts: Vec<serde_json::Value> = points
            .iter()
            .map(|(t, v)| json!({ "timestamp": t, "value": v }))
            .collect();
        json!({ "points": pts, "kline_name": "trendline" })
    }

    fn make_chart_response_with_weekly_mas() -> ChartResponse {
        use crate::chart::data::*;
        let sp = |t: i64, v: f64| SeriesPoint { time: t, value: v };
        ChartResponse {
            symbol: "BTC".into(),
            timeframe: "1D".into(),
            ohlcv: vec![],
            cpr: None,
            weekly_cpr: None,
            daily_cpr: None,
            indicators: Some(Indicators {
                ema_ribbon: None,
                weekly_mas: Some(WeeklyMAs {
                    wma50: Some(vec![sp(100, 50000.0), sp(200, 51000.0)]),
                    wma100: Some(vec![sp(100, 45000.0), sp(200, 46000.0)]),
                    wma200: Some(vec![sp(100, 40000.0), sp(200, 41000.0)]),
                }),
                bull_market_band: None,
                sma_lines: None,
                ttm_squeeze: None,
            }),
            volume: vec![],
            rsi: vec![],
            macd: None,
            price_lines: vec![],
            markers: None,
            cvd_spot: None,
            oi_hist: None,
            cvd_usdt: None,
            cvd_coin: None,
            quant: None,
            sessions: vec![],
        }
    }

    fn make_chart_response_with_price_lines() -> ChartResponse {
        use crate::chart::data::*;
        ChartResponse {
            symbol: "ETH".into(),
            timeframe: "4H".into(),
            ohlcv: vec![],
            cpr: None,
            weekly_cpr: None,
            daily_cpr: None,
            indicators: None,
            volume: vec![],
            rsi: vec![],
            macd: None,
            price_lines: vec![
                PriceLine {
                    price: 3000.0,
                    color: Some("#ff0000".into()),
                    title: Some("R1".into()),
                    line_style: Some(1),
                },
                PriceLine {
                    price: 2800.0,
                    color: Some("#00ff00".into()),
                    title: Some("S1".into()),
                    line_style: None,
                },
            ],
            markers: None,
            cvd_spot: None,
            oi_hist: None,
            cvd_usdt: None,
            cvd_coin: None,
            quant: None,
            sessions: vec![],
        }
    }

    // ── Drawing adapter tests ─────────────────────────────────────────────────

    #[test]
    fn fibonacci_produces_seven_hlines_at_correct_prices() {
        // Anchors: value=10000 at ts=0, value=20000 at ts=86400
        let meta = meta_with_points(&[(0, 10000.0), (86400, 20000.0)]);
        let d = make_drawing("fibonacci", None, None, Some(meta));
        let prims = drawings_to_primitives(&[d]);

        assert_eq!(prims.len(), 7, "fibonacci should produce exactly 7 Hlines");

        let expected_prices: Vec<f64> = FIB_RATIOS
            .iter()
            .map(|&r| 10000.0 + (20000.0 - 10000.0) * r)
            .collect();

        for (prim, exp_price) in prims.iter().zip(expected_prices.iter()) {
            match prim {
                ChartPrimitive::Hline { price, label, .. } => {
                    assert!(
                        (price - exp_price).abs() < 1e-6,
                        "price mismatch: got {price}, expected {exp_price}"
                    );
                    // Label should be the ratio string
                    assert!(label.is_some(), "fibonacci Hline should have a ratio label");
                }
                other => panic!("expected Hline, got {:?}", other),
            }
        }
    }

    #[test]
    fn trendline_produces_one_polyline_with_two_points() {
        let meta = meta_with_points(&[(1_000_000, 100.0), (2_000_000, 200.0)]);
        let d = make_drawing("trendline", Some(100.0), Some(200.0), Some(meta));
        let prims = drawings_to_primitives(&[d]);

        assert_eq!(prims.len(), 1);
        match &prims[0] {
            ChartPrimitive::Polyline { points, .. } => {
                assert_eq!(points.len(), 2);
                assert_eq!(points[0], (1_000_000, 100.0));
                assert_eq!(points[1], (2_000_000, 200.0));
            }
            other => panic!("expected Polyline, got {:?}", other),
        }
    }

    #[test]
    fn zone_produces_one_rect() {
        let meta = meta_with_points(&[(500, 90.0), (1000, 90.0), (1000, 110.0), (500, 110.0)]);
        let d = make_drawing("zone", Some(90.0), Some(110.0), Some(meta));
        let prims = drawings_to_primitives(&[d]);

        assert_eq!(prims.len(), 1);
        assert!(
            matches!(&prims[0], ChartPrimitive::Rect { .. }),
            "expected Rect, got {:?}",
            prims[0]
        );
    }

    #[test]
    fn position_produces_three_hlines() {
        let meta = meta_with_points(&[(1000, 50000.0), (1000, 48000.0), (1000, 55000.0)]);
        let d = make_drawing("position", None, None, Some(meta));
        let prims = drawings_to_primitives(&[d]);

        assert_eq!(prims.len(), 3, "position should produce exactly 3 Hlines");
        for prim in &prims {
            assert!(
                matches!(prim, ChartPrimitive::Hline { .. }),
                "expected Hline, got {:?}",
                prim
            );
        }
        // First hline = entry (green), second = SL (red), third = TP (teal)
        match &prims[0] {
            ChartPrimitive::Hline {
                price,
                color,
                label,
                ..
            } => {
                assert!((price - 50000.0).abs() < 1e-6);
                assert_eq!(color.as_deref(), Some("#50dc78"));
                assert_eq!(label.as_deref(), Some("Entry"));
            }
            _ => panic!("expected Hline"),
        }
        match &prims[1] {
            ChartPrimitive::Hline { price, label, .. } => {
                assert!((price - 48000.0).abs() < 1e-6);
                assert_eq!(label.as_deref(), Some("SL"));
            }
            _ => panic!("expected Hline"),
        }
        match &prims[2] {
            ChartPrimitive::Hline { price, label, .. } => {
                assert!((price - 55000.0).abs() < 1e-6);
                assert_eq!(label.as_deref(), Some("TP"));
            }
            _ => panic!("expected Hline"),
        }
    }

    #[test]
    fn unknown_drawing_type_produces_empty_vec_and_does_not_panic() {
        let d = make_drawing("totally_unknown_type_xyz", Some(100.0), None, None);
        let prims = drawings_to_primitives(&[d]);
        assert!(
            prims.is_empty(),
            "unknown type should produce no primitives"
        );
    }

    // ── Overlay adapter tests ─────────────────────────────────────────────────

    #[test]
    fn overlays_weekly_mas_produces_three_polylines() {
        let resp = make_chart_response_with_weekly_mas();
        let prims = overlays_to_primitives(&resp);

        let polylines: Vec<_> = prims
            .iter()
            .filter(|p| matches!(p, ChartPrimitive::Polyline { .. }))
            .collect();

        assert_eq!(
            polylines.len(),
            3,
            "weeklyMAs should produce 3 Polylines, got {}",
            polylines.len()
        );

        let labels: Vec<&str> = polylines
            .iter()
            .filter_map(|p| match p {
                ChartPrimitive::Polyline { label, .. } => label.as_deref(),
                _ => None,
            })
            .collect();
        assert!(labels.contains(&"WMA50"), "missing WMA50 label");
        assert!(labels.contains(&"WMA100"), "missing WMA100 label");
        assert!(labels.contains(&"WMA200"), "missing WMA200 label");
    }

    #[test]
    fn overlays_price_lines_produces_hlines() {
        let resp = make_chart_response_with_price_lines();
        let prims = overlays_to_primitives(&resp);

        let hlines: Vec<_> = prims
            .iter()
            .filter(|p| matches!(p, ChartPrimitive::Hline { .. }))
            .collect();

        assert_eq!(hlines.len(), 2, "should get 2 Hlines for 2 priceLines");

        // R1 is dashed
        match &hlines[0] {
            ChartPrimitive::Hline {
                price,
                style,
                label,
                ..
            } => {
                assert!((price - 3000.0).abs() < 1e-6);
                assert_eq!(*style, LineStyle::Dashed);
                assert_eq!(label.as_deref(), Some("R1"));
            }
            _ => panic!("expected Hline"),
        }
        // S1 defaults to Solid
        match &hlines[1] {
            ChartPrimitive::Hline { price, style, .. } => {
                assert!((price - 2800.0).abs() < 1e-6);
                assert_eq!(*style, LineStyle::Solid);
            }
            _ => panic!("expected Hline"),
        }
    }
}
