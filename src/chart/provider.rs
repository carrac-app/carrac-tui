//! OHLCV provider seam — the data layer behind every chart fetch.
//!
//! The TUI historically fetched candles by calling
//! `client.get::<ChartResponse>("/api/chart/…")` from four sites (the
//! standalone `carrac chart` once / png / interactive paths and the `carrac
//! tui` cockpit fetcher). This module collapses that into one enum-dispatched
//! seam so a second backend can slot in without touching the render code.
//!
//! Two backends ship today:
//!   - [`Provider::Carrac`] — wraps the authenticated [`Client`] exactly as
//!     before. Zero behaviour change when an API key is configured.
//!   - [`Provider::Binance`] — public, keyless Binance klines for the demo /
//!     bring-your-own-backend story. Crypto OHLCV only; the Carrac-computed
//!     overlays (CPR, MACD supplement, RSI, sessions, indicators) are simply
//!     absent and the renderer already treats every one as Option/default.
//!
//! Enum dispatch (not a `dyn` trait) keeps us off the `async_trait` dependency
//! while still giving callers a single `provider.fetch_chart(..)` call.

use anyhow::{Result, anyhow};

use crate::chart::data::{
    Candle, ChartResponse, VolumeEntry, enrich_cpr_with_sr_levels, fetch_macd,
};
use crate::client::Client;
use crate::config::{Backend, Config};

/// A source of OHLCV chart data. Construct via [`Provider::carrac`] /
/// [`Provider::binance`] (or [`crate::config::Config::build_provider`]).
#[derive(Clone)]
pub enum Provider {
    /// Authenticated Carrac backend (`/api/chart/…`). The default whenever an
    /// API key is configured.
    Carrac(Client),
    /// Keyless public Binance backend — crypto OHLCV only.
    Binance(BinanceProvider),
}

impl Provider {
    pub fn carrac(client: Client) -> Self {
        Provider::Carrac(client)
    }

    pub fn binance() -> Self {
        Provider::Binance(BinanceProvider::new())
    }

    /// Build the provider the config selected. Carrac reuses the already-built
    /// authenticated [`Client`]; Binance ignores it (keyless).
    pub fn from_config(cfg: &Config, client: Client) -> Self {
        match cfg.backend {
            Backend::Carrac => Provider::carrac(client),
            Backend::Binance => Provider::binance(),
        }
    }

    /// True for any backend that is NOT the full Carrac platform — i.e. the
    /// keyless demo. Carrac-only surfaces gate on this.
    pub fn is_demo(&self) -> bool {
        matches!(self, Provider::Binance(_))
    }

    /// Fetch the base OHLCV chart for `symbol` at timeframe `tf`, asking for
    /// `bars` candles. The returned [`ChartResponse`] always carries candles;
    /// every other field is backend-dependent (Carrac fills CPR/RSI/etc.,
    /// Binance leaves them empty).
    pub async fn fetch_chart(&self, symbol: &str, tf: &str, bars: u32) -> Result<ChartResponse> {
        match self {
            Provider::Carrac(client) => {
                let path = format!("/api/chart/{}?timeframe={}&days={}", symbol, tf, bars);
                client.get::<ChartResponse>(&path).await
            }
            Provider::Binance(b) => b.fetch_chart(symbol, tf, bars).await,
        }
    }

    /// Apply the Carrac-only post-fetch supplements (CPR S/R levels computed
    /// from the prior monthly bar, plus the MACD series from the chart endpoint
    /// with `?macd=1` when `show_macd`). No-op for the Binance backend, which has
    /// no such endpoints — the renderer shows those panes empty/absent.
    pub async fn enrich(
        &self,
        symbol: &str,
        tf: &str,
        bars: u32,
        data: &mut ChartResponse,
        show_macd: bool,
    ) {
        if let Provider::Carrac(client) = self {
            enrich_cpr_with_sr_levels(client, symbol, data).await;
            if show_macd {
                data.macd = fetch_macd(client, symbol, tf, bars).await;
            }
        }
    }
}

// ─── Binance backend ──────────────────────────────────────────────────────────

const BINANCE_KLINES_URL: &str = "https://api.binance.com/api/v3/klines";

#[derive(Clone)]
pub struct BinanceProvider {
    http: reqwest::Client,
}

impl Default for BinanceProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl BinanceProvider {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("carrac-tui/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self { http }
    }

    async fn fetch_chart(&self, symbol: &str, tf: &str, bars: u32) -> Result<ChartResponse> {
        let market = binance_symbol(symbol);
        let interval =
            binance_interval(tf).ok_or_else(|| anyhow!("binance: unsupported timeframe {tf}"))?;
        // Binance caps `limit` at 1000 per request; clamp (the renderer slices
        // the visible window anyway).
        let limit = bars.clamp(1, 1000);
        let url = format!("{BINANCE_KLINES_URL}?symbol={market}&interval={interval}&limit={limit}");
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| anyhow!("binance: request failed: {e}"))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| anyhow!("binance: reading body: {e}"))?;
        if !status.is_success() {
            let snippet: String = body.chars().take(200).collect();
            return Err(anyhow!(
                "binance: {market} {interval} -> {status}: {snippet}"
            ));
        }
        parse_binance_klines(&body, symbol, tf)
    }
}

/// Map a TUI symbol to a Binance market pair. Bare tickers (BTC, ETH) get a
/// USDT quote appended; anything already quoted (BTCUSDT, ETHUSDC) passes
/// through untouched. Always upper-cased — Binance pairs are upper-case.
pub fn binance_symbol(symbol: &str) -> String {
    let s = symbol.trim().to_uppercase();
    // Already a quoted pair? Common stable/quote suffixes pass through.
    const QUOTES: [&str; 5] = ["USDT", "USDC", "BUSD", "FDUSD", "TUSD"];
    if QUOTES.iter().any(|q| s.ends_with(q) && s.len() > q.len()) {
        return s;
    }
    format!("{s}USDT")
}

/// Map a TUI timeframe to a Binance kline interval string.
/// Returns None for timeframes Binance does not offer.
///
/// NOTE: Binance is case-sensitive — minutes/hours/days/weeks are lower-case
/// (`5m`, `1h`, `1d`, `1w`) but MONTHS are a capital `M` (`1M`). Lower-case `m`
/// always means minutes, so a `1M` month must never be sent as `1m`.
pub fn binance_interval(tf: &str) -> Option<&'static str> {
    match tf.trim() {
        "1m" | "1M_min" => Some("1m"),
        "5m" => Some("5m"),
        "15m" => Some("15m"),
        "30m" => Some("30m"),
        "1H" | "1h" => Some("1h"),
        "2H" | "2h" => Some("2h"),
        "4H" | "4h" => Some("4h"),
        "6H" | "6h" => Some("6h"),
        "12H" | "12h" => Some("12h"),
        "1D" | "1d" => Some("1d"),
        "3D" | "3d" => Some("3d"),
        "1W" | "1w" => Some("1w"),
        // Binance months are a CAPITAL M.
        "1M" => Some("1M"),
        _ => None,
    }
}

/// Parse a Binance `/api/v3/klines` JSON array into a [`ChartResponse`].
///
/// Each kline is a heterogeneous array:
///   [ openTime(ms), open, high, low, close, volume, closeTime, … ]
/// where prices/volume are JSON STRINGS and timestamps are MILLISECONDS.
///
/// CRITICAL: the entire TUI assumes UNIX SECONDS (fmt_ts_utc, session
/// classification, x-axis labels). We divide the millisecond open-time by 1000
/// here so every downstream consumer sees seconds, identical to the Carrac
/// backend's units.
///
/// Separated from the HTTP call so it is unit-testable with a hardcoded
/// fixture and zero network.
pub fn parse_binance_klines(body: &str, symbol: &str, tf: &str) -> Result<ChartResponse> {
    let rows: Vec<Vec<serde_json::Value>> =
        serde_json::from_str(body).map_err(|e| anyhow!("binance: decoding klines JSON: {e}"))?;

    let mut ohlcv = Vec::with_capacity(rows.len());
    let mut volume = Vec::with_capacity(rows.len());
    for row in &rows {
        // [0]=openTime(ms) [1]=open [2]=high [3]=low [4]=close [5]=volume
        if row.len() < 6 {
            continue;
        }
        let time_ms = val_i64(&row[0])?;
        let time = time_ms / 1000; // ms → s — the unit every consumer expects
        let open = val_f64(&row[1])?;
        let high = val_f64(&row[2])?;
        let low = val_f64(&row[3])?;
        let close = val_f64(&row[4])?;
        let vol = val_f64(&row[5]).unwrap_or(0.0);
        ohlcv.push(Candle {
            time,
            open,
            high,
            low,
            close,
        });
        volume.push(VolumeEntry {
            time,
            value: vol,
            color: String::new(),
        });
    }

    Ok(ChartResponse {
        symbol: binance_symbol(symbol),
        timeframe: tf.to_string(),
        ohlcv,
        volume,
        // Everything else is Carrac-server-computed; absent on the keyless
        // backend. The renderer treats each as Option/default.
        ..Default::default()
    })
}

/// Read a number that Binance may encode as a JSON string ("63000.50") or a
/// raw number.
fn val_f64(v: &serde_json::Value) -> Result<f64> {
    if let Some(n) = v.as_f64() {
        return Ok(n);
    }
    if let Some(s) = v.as_str() {
        return s
            .parse::<f64>()
            .map_err(|e| anyhow!("binance: bad float {s:?}: {e}"));
    }
    Err(anyhow!("binance: expected number, got {v}"))
}

fn val_i64(v: &serde_json::Value) -> Result<i64> {
    if let Some(n) = v.as_i64() {
        return Ok(n);
    }
    if let Some(s) = v.as_str() {
        return s
            .parse::<i64>()
            .map_err(|e| anyhow!("binance: bad int {s:?}: {e}"));
    }
    Err(anyhow!("binance: expected integer, got {v}"))
}

// ─── Tests (no network) ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // A hardcoded two-kline sample in the exact Binance wire shape: numbers as
    // strings, openTime in MILLISECONDS.
    const SAMPLE: &str = r#"[
        [1717200000000,"63000.10","63500.00","62800.00","63250.50","123.456",1717203599999,"7800000.0",1500,"60.0","3900000.0","0"],
        [1717203600000,"63250.50","64000.00","63100.00","63900.00","98.765",1717207199999,"6300000.0",1200,"50.0","3200000.0","0"]
    ]"#;

    #[test]
    fn parses_candles_with_seconds_timestamps() {
        let resp = parse_binance_klines(SAMPLE, "BTC", "1H").unwrap();
        assert_eq!(resp.ohlcv.len(), 2);
        // 1717200000000 ms → 1717200000 s (NOT the raw ms value).
        assert_eq!(resp.ohlcv[0].time, 1_717_200_000);
        assert_eq!(resp.ohlcv[1].time, 1_717_203_600);
        // Sanity: this is a plausible 2024 unix-SECONDS timestamp, ~1.7e9, not
        // the ~1.7e12 millisecond value the TUI would mis-render.
        assert!(resp.ohlcv[0].time < 2_000_000_000);
    }

    #[test]
    fn parses_ohlc_fields_from_string_encoded_numbers() {
        let resp = parse_binance_klines(SAMPLE, "BTC", "1H").unwrap();
        let c = &resp.ohlcv[0];
        assert!((c.open - 63000.10).abs() < 1e-6);
        assert!((c.high - 63500.00).abs() < 1e-6);
        assert!((c.low - 62800.00).abs() < 1e-6);
        assert!((c.close - 63250.50).abs() < 1e-6);
    }

    #[test]
    fn populates_per_candle_volume_from_index_5() {
        let resp = parse_binance_klines(SAMPLE, "BTC", "1H").unwrap();
        assert_eq!(resp.volume.len(), 2);
        assert!((resp.volume[0].value - 123.456).abs() < 1e-3);
        assert!((resp.volume[1].value - 98.765).abs() < 1e-3);
        // Volume timestamps are index-aligned and in seconds too.
        assert_eq!(resp.volume[0].time, resp.ohlcv[0].time);
    }

    #[test]
    fn carrac_only_fields_absent_on_binance() {
        let resp = parse_binance_klines(SAMPLE, "BTC", "1H").unwrap();
        assert!(resp.cpr.is_none());
        assert!(resp.daily_cpr.is_none());
        assert!(resp.weekly_cpr.is_none());
        assert!(resp.indicators.is_none());
        assert!(resp.macd.is_none());
        assert!(resp.rsi.is_empty());
        assert!(resp.sessions.is_empty());
    }

    #[test]
    fn symbol_mapping_appends_usdt_for_bare_tickers() {
        assert_eq!(binance_symbol("BTC"), "BTCUSDT");
        assert_eq!(binance_symbol("eth"), "ETHUSDT");
        assert_eq!(binance_symbol(" sol "), "SOLUSDT");
    }

    #[test]
    fn symbol_mapping_passes_through_quoted_pairs() {
        assert_eq!(binance_symbol("BTCUSDT"), "BTCUSDT");
        assert_eq!(binance_symbol("ethusdc"), "ETHUSDC");
        assert_eq!(binance_symbol("BTCFDUSD"), "BTCFDUSD");
    }

    #[test]
    fn interval_mapping_matches_binance_spec() {
        assert_eq!(binance_interval("1H"), Some("1h"));
        assert_eq!(binance_interval("4H"), Some("4h"));
        assert_eq!(binance_interval("1D"), Some("1d"));
        assert_eq!(binance_interval("1W"), Some("1w"));
        assert_eq!(binance_interval("5m"), Some("5m"));
        assert_eq!(binance_interval("15m"), Some("15m"));
        // Months are a CAPITAL M on Binance — lower-case m would mean minutes.
        assert_eq!(binance_interval("1M"), Some("1M"));
        assert_ne!(binance_interval("1M"), Some("1m"));
        // Unknown TF → None (caller surfaces an error rather than guessing).
        assert_eq!(binance_interval("7H"), None);
    }

    #[test]
    fn response_carries_mapped_symbol_and_tf() {
        let resp = parse_binance_klines(SAMPLE, "btc", "4H").unwrap();
        assert_eq!(resp.symbol, "BTCUSDT");
        assert_eq!(resp.timeframe, "4H");
    }

    #[test]
    fn short_rows_are_skipped_not_fatal() {
        let body = r#"[[1717200000000,"1","2","0.5","1.5"]]"#; // only 5 elements
        let resp = parse_binance_klines(body, "BTC", "1H").unwrap();
        assert!(resp.ohlcv.is_empty());
    }
}
