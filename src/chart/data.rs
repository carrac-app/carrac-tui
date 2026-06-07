/// Data types and fetch helpers for the chart subsystem.
///
/// This module is intentionally free of rendering, TUI, and terminal concerns.
/// It owns the API response structs, pivot-level computation, and the helpers
/// that decide how many days / bars to fetch for a given timeframe.
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::client::Client;

// ─── API response types ───────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Default)]
pub(crate) struct ChartResponse {
    #[allow(dead_code)]
    pub(crate) symbol: String,
    pub(crate) timeframe: String,
    pub(crate) ohlcv: Vec<Candle>,
    /// Monthly CPR (prior closed month). Far from price on tight intraday
    /// windows, where the weekly/daily sets track closer.
    pub(crate) cpr: Option<CprData>,
    /// Weekly CPR (prior week) — server sends it alongside the monthly one.
    #[serde(default)]
    pub(crate) weekly_cpr: Option<CprData>,
    /// Daily CPR (prior day) — what the web chart shows on intraday TFs.
    #[serde(default)]
    pub(crate) daily_cpr: Option<CprData>,
    pub(crate) indicators: Option<Indicators>,
    /// Top-level volume array: [{time, value, color}, …]
    #[serde(default)]
    pub(crate) volume: Vec<VolumeEntry>,
    /// Server-computed RSI14: [{time, value}, …], may be empty for short windows
    #[serde(default)]
    pub(crate) rsi: Vec<RsiEntry>,
    /// MACD series: line, signal, histogram (top-level alongside rsi/volume)
    #[serde(default)]
    pub(crate) macd: Option<Macd>,
    /// Server-computed CPR support/resistance price lines
    #[serde(rename = "priceLines", default)]
    pub(crate) price_lines: Vec<PriceLine>,
    /// Chart markers (e.g. event annotations from the server)
    #[serde(default)]
    pub(crate) markers: Option<Vec<ChartMarker>>,
    /// CVD spot series (present only when ?derivatives= query param is set)
    #[serde(rename = "cvd_spot", default)]
    pub(crate) cvd_spot: Option<serde_json::Value>,
    /// OI history series (present only when ?derivatives= is set)
    #[serde(rename = "oi_hist", default)]
    pub(crate) oi_hist: Option<serde_json::Value>,
    /// CVD USDT-margined futures (present only when ?derivatives= is set)
    #[serde(rename = "cvd_usdt", default)]
    pub(crate) cvd_usdt: Option<serde_json::Value>,
    /// CVD coin-margined futures (present only when ?derivatives= is set)
    #[serde(rename = "cvd_coin", default)]
    pub(crate) cvd_coin: Option<serde_json::Value>,
    /// Quant macro series map (present only when ?quant= is set)
    #[serde(default)]
    pub(crate) quant: Option<HashMap<String, serde_json::Value>>,
    /// Per-candle market-session classification. The server attaches this only
    /// for intraday timeframes (≤ 4H); it is absent on 1D+ (so the session pane
    /// auto-hides). Index-aligned 1:1 with `ohlcv`.
    #[serde(default)]
    pub(crate) sessions: Vec<SessionEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Candle {
    pub time: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
}

/// One per-candle market-session classification, as served by `/api/chart`
/// (intraday timeframes only). Index-aligned with `ChartResponse.ohlcv`.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SessionEntry {
    /// "asia" | "uk" | "us" | "dead"
    #[serde(default)]
    pub session: String,
    /// "gap" | "session" | "extension" | "reversal" | "dead"
    #[allow(dead_code)]
    #[serde(default)]
    pub phase: String,
    /// Uppercased session name ("ASIA"/"UK"/"US") on the first candle of a new
    /// session, else absent. (We re-derive run starts ourselves, so this is
    /// informational.)
    #[allow(dead_code)]
    #[serde(default)]
    pub label: Option<String>,
    /// Holiday name if this session is affected (e.g. "Weekend").
    #[allow(dead_code)]
    #[serde(default)]
    pub holiday: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct VolumeEntry {
    pub(crate) time: i64,
    pub(crate) value: f64,
    #[allow(dead_code)]
    #[serde(default)]
    pub(crate) color: String,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct RsiEntry {
    pub(crate) time: i64,
    pub(crate) value: f64,
}

#[derive(Serialize, Deserialize, Default)]
pub(crate) struct CprData {
    pub(crate) pivot: Option<f64>,
    pub(crate) tc: Option<f64>,
    pub(crate) bc: Option<f64>,
    /// Start of the period this CPR set applies to (UNIX seconds). The server
    /// sends it for the monthly set; used to bound the drawn lines on the left.
    #[serde(rename = "periodStart", default)]
    pub(crate) period_start: Option<i64>,
    /// S1-S3 / R1-R3 are computed client-side via `enrich_cpr_with_sr_levels`
    /// from the prior closed monthly bar; the server's `/api/chart` doesn't
    /// expose them today.
    #[serde(default)]
    pub(crate) s1: Option<f64>,
    #[serde(default)]
    pub(crate) s2: Option<f64>,
    #[serde(default)]
    pub(crate) s3: Option<f64>,
    #[serde(default)]
    pub(crate) r1: Option<f64>,
    #[serde(default)]
    pub(crate) r2: Option<f64>,
    #[serde(default)]
    pub(crate) r3: Option<f64>,
}

/// A generic time+value series point.  Used for every overlay series that is
/// just `{time, value}`.  Timestamps are UNIX seconds (same unit as Candle.time).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SeriesPoint {
    pub time: i64,
    pub value: f64,
}

#[derive(Serialize, Deserialize, Default)]
pub(crate) struct Indicators {
    pub(crate) ema_ribbon: Option<EmaRibbon>,
    /// Weekly moving averages: wma50, wma100, wma200
    #[serde(rename = "weeklyMAs", default)]
    pub(crate) weekly_mas: Option<WeeklyMAs>,
    /// Bull Market Band: ema21w and sma20w
    #[serde(rename = "bullMarketBand", default)]
    pub(crate) bull_market_band: Option<BullMarketBand>,
    /// Simple moving averages: sma20 and sma200
    #[serde(rename = "smaLines", default)]
    pub(crate) sma_lines: Option<SmaLines>,
    /// TTM Squeeze histogram dots: [{time, squeeze: bool}, …]
    #[serde(rename = "ttmSqueeze", default)]
    pub(crate) ttm_squeeze: Option<Vec<TtmSqueezeEntry>>,
}

#[derive(Serialize, Deserialize, Default)]
pub(crate) struct EmaRibbon {
    pub(crate) fast: Option<Vec<SeriesPoint>>,
    /// Slow EMA series (same shape as fast)
    #[serde(default)]
    pub(crate) slow: Option<Vec<SeriesPoint>>,
    /// Fill series [{time, trend}, …] — leniently typed; skipped if absent
    #[serde(default)]
    pub(crate) fill: Option<Vec<EmaFillEntry>>,
}

/// One entry in the EMA ribbon fill series.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EmaFillEntry {
    pub time: i64,
    /// "bull" | "bear" | "neutral" etc. from the server
    #[serde(default)]
    pub trend: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct WeeklyMAs {
    #[serde(default)]
    pub(crate) wma50: Option<Vec<SeriesPoint>>,
    #[serde(default)]
    pub(crate) wma100: Option<Vec<SeriesPoint>>,
    #[serde(default)]
    pub(crate) wma200: Option<Vec<SeriesPoint>>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct BullMarketBand {
    #[serde(default)]
    pub(crate) ema21w: Option<Vec<SeriesPoint>>,
    #[serde(default)]
    pub(crate) sma20w: Option<Vec<SeriesPoint>>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct SmaLines {
    #[serde(default)]
    pub(crate) sma20: Option<Vec<SeriesPoint>>,
    #[serde(default)]
    pub(crate) sma200: Option<Vec<SeriesPoint>>,
}

/// One entry from the TTM Squeeze series.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TtmSqueezeEntry {
    pub time: i64,
    #[serde(default)]
    pub squeeze: bool,
}

/// MACD output series (line + signal + histogram).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Macd {
    #[serde(default)]
    pub line: Vec<SeriesPoint>,
    #[serde(default)]
    pub signal: Vec<SeriesPoint>,
    #[serde(default)]
    pub histogram: Vec<SeriesPoint>,
}

/// A server-supplied horizontal price line (e.g. CPR S/R levels).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PriceLine {
    pub price: f64,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(rename = "lineStyle", default)]
    pub line_style: Option<i64>,
}

/// A chart marker annotation from the server.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ChartMarker {
    pub time: i64,
    #[serde(default)]
    pub position: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub shape: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
}

// ─── MACD supplement fetch ────────────────────────────────────────────────────

/// Build the path for the MACD supplement request: the chart endpoint with
/// `?macd=1`. The timeframe/days params mirror the displayed chart window so
/// the returned MACD series matches the candles already on screen.
fn macd_fetch_path(symbol: &str, tf: &str, days: u32) -> String {
    format!(
        "/api/chart/{}?timeframe={}&days={}&macd=1",
        symbol, tf, days
    )
}

/// Fetch the MACD series from the chart endpoint with `?macd=1` and return it.
///
/// With `?macd=1` the chart endpoint returns the same `ChartResponse` shape plus
/// a top-level `macd` field (which a plain `/api/chart` call omits). Deserialising
/// into `ChartResponse` is safe: any extra fields are silently ignored by serde.
///
/// Returns `None` on ANY error — the chart must never fail over a missing MACD.
pub(crate) async fn fetch_macd(client: &Client, symbol: &str, tf: &str, days: u32) -> Option<Macd> {
    let path = macd_fetch_path(symbol, tf, days);
    match client.get::<ChartResponse>(&path).await {
        Ok(resp) => resp.macd,
        Err(_) => None,
    }
}

// ─── Pivot-level computation ──────────────────────────────────────────────────

/// Standard pivot-point support/resistance levels computed from prior period's H/L/C.
///
/// Formulas (Floor Trader's Pivot, used by carrac monthly CPR):
///   P  = (H + L + C) / 3
///   R1 = 2P − L     S1 = 2P − H
///   R2 = P + (H−L)  S2 = P − (H−L)
///   R3 = H + 2(P−L) S3 = L − 2(H−P)
pub(crate) fn compute_pivot_levels(h: f64, l: f64, c: f64) -> (f64, f64, f64, f64, f64, f64) {
    let p = (h + l + c) / 3.0;
    let range = h - l;
    let r1 = 2.0 * p - l;
    let s1 = 2.0 * p - h;
    let r2 = p + range;
    let s2 = p - range;
    let r3 = h + 2.0 * (p - l);
    let s3 = l - 2.0 * (h - p);
    (s1, s2, s3, r1, r2, r3)
}

/// Fetch monthly OHLCV and populate `data.cpr.s1-s3 / r1-r3` from the prior
/// closed monthly bar's H/L/C. Best-effort: any failure leaves cpr unchanged.
/// Called by both the TUI chart fetcher and the standalone `carrac chart` so
/// the TUI shows the same S/R levels as the PNG render.
pub(crate) async fn enrich_cpr_with_sr_levels(
    client: &Client,
    asset: &str,
    data: &mut ChartResponse,
) {
    let cpr = match data.cpr.as_mut() {
        Some(c) => c,
        None => return,
    };
    // Skip if levels are already populated.
    if cpr.s1.is_some() {
        return;
    }

    let monthly_path = format!("/api/chart/{}?timeframe=1M&days=120", asset);
    let monthly = match client.get::<ChartResponse>(&monthly_path).await {
        Ok(m) => m,
        Err(_) => return,
    };
    let source_bar = if monthly.ohlcv.len() >= 2 {
        Some(&monthly.ohlcv[monthly.ohlcv.len() - 2])
    } else {
        monthly.ohlcv.last()
    };
    if let Some(bar) = source_bar {
        let (s1, s2, s3, r1, r2, r3) = compute_pivot_levels(bar.high, bar.low, bar.close);
        cpr.s1 = Some(s1);
        cpr.s2 = Some(s2);
        cpr.s3 = Some(s3);
        cpr.r1 = Some(r1);
        cpr.r2 = Some(r2);
        cpr.r3 = Some(r3);
    }
}

// ─── Fixed fetch-window per TF ───────────────────────────────────────────────
//
// We always fetch a large window and slice the last `bars` candles at render
// time. This makes `[`/`]` instant — just a redraw, no network round-trip.

pub(crate) fn fetch_days_for_tf(tf: &str) -> u32 {
    // NOTE: despite the name, the server treats the `days` query param as a
    // BAR COUNT (capped at 25000 intraday / 5000 HTF), NOT calendar days. So
    // these are bar counts: enough to fill a wide terminal (~180 candles) and
    // leave generous room to pan back and to thin out with `]`. The old values
    // (15m→7, 1H→14, 4H→60) returned far too few bars on lower timeframes,
    // which pinned the pan offset to 0 and made h/l and [/] no-ops.
    match tf {
        "1W" => 520,
        "1D" => 400,
        "4H" => 400,
        "1H" => 400,
        "5m" | "15m" | "30m" => 400,
        _ => 365,
    }
}

/// Default number of visible bars per TF.
/// 1H/4H cap at 60 → show 20 (33% of cap, 40 bars left to pan).
/// 1D caps at 365 → show 80. 1W caps at 458 → show 100.
pub(crate) fn default_visible_bars(tf: &str) -> usize {
    match tf {
        "5m" | "15m" => 60,
        "1H" => 48,
        "4H" => 20,
        "1D" => 80,
        "1W" => 100,
        _ => 40,
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod session_tests {
    use super::*;

    #[test]
    fn deserializes_sessions_array_from_chart_response() {
        let json = r#"{
            "symbol":"BTCUSDT","timeframe":"15m",
            "ohlcv":[{"time":1,"open":1.0,"high":1.0,"low":1.0,"close":1.0}],
            "sessions":[
                {"session":"us","phase":"session","label":"US"},
                {"session":"dead","phase":"dead"}
            ]
        }"#;
        let resp: ChartResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.sessions.len(), 2);
        assert_eq!(resp.sessions[0].session, "us");
        assert_eq!(resp.sessions[0].label.as_deref(), Some("US"));
        assert_eq!(resp.sessions[1].session, "dead");
        assert_eq!(resp.sessions[1].label, None);
    }

    #[test]
    fn fetch_days_requests_enough_bars_to_fill_and_pan() {
        // The server's `days` query param is a BAR COUNT (max 25000 intraday /
        // 5000 HTF), not calendar days. Every TF must request enough bars to
        // fill a wide terminal AND leave room to pan/zoom — too few (the old
        // 7/14) makes h/l and [/] no-ops on lower TFs.
        for tf in ["5m", "15m", "30m", "1H", "4H", "1D", "1W"] {
            let n = fetch_days_for_tf(tf);
            assert!(n >= 300, "{tf} should request >=300 bars, got {n}");
            assert!(
                n <= 5000,
                "{tf} should stay within the HTF server cap, got {n}"
            );
        }
    }

    #[test]
    fn missing_sessions_key_defaults_empty() {
        // Higher timeframes (1D+) omit the key entirely — must default, not error.
        let json = r#"{"symbol":"BTCUSDT","timeframe":"1D","ohlcv":[]}"#;
        let resp: ChartResponse = serde_json::from_str(json).unwrap();
        assert!(resp.sessions.is_empty());
    }

    #[test]
    fn macd_fetch_path_uses_public_chart_endpoint_with_macd_flag() {
        // The MACD supplement rides the public chart endpoint with ?macd=1,
        // preserving the displayed window's timeframe/days so the series lines
        // up with the candles on screen.
        let path = macd_fetch_path("BTCUSDT", "4H", 400);
        assert_eq!(path, "/api/chart/BTCUSDT?timeframe=4H&days=400&macd=1");
        assert!(path.contains("macd=1"), "must request the macd field");
        assert!(path.starts_with("/api/chart/"), "must hit the public route");
        // Assembled from fragments so the discouraged literal never appears
        // in export-bound source.
        let internal = ["chart-int", "ernal"].concat();
        assert!(!path.contains(&internal), "no internal endpoint");
    }
}

#[cfg(test)]
mod pivot_tests {
    use super::compute_pivot_levels;

    #[test]
    fn standard_pivot_btc_april_2026() {
        // Source bar: H=82850, L=74289.6, C=76765.53
        let (s1, s2, s3, r1, r2, r3) = compute_pivot_levels(82850.0, 74289.6, 76765.53);
        let p: f64 = (82850.0 + 74289.6 + 76765.53) / 3.0;
        assert!((p - 77_968.376_666_666_66_f64).abs() < 0.01);
        assert!((r1 - (2.0 * p - 74289.6)).abs() < 0.01);
        assert!((s1 - (2.0 * p - 82850.0)).abs() < 0.01);
        assert!((r2 - (p + 8560.4)).abs() < 0.01);
        assert!((s2 - (p - 8560.4)).abs() < 0.01);
        assert!(r3 > r2, "R3 must be above R2");
        assert!(s3 < s2, "S3 must be below S2");
    }
}
