//! Keyless Kraken OHLC adapter — a transparent fallback for the public crypto
//! path when the primary keyless source refuses a request by region.
//!
//! Kraken's public OHLC endpoint needs no API key and is served worldwide, so
//! it covers regions where the primary keyless source returns a legal-block
//! status. The adapter speaks the same [`ChartResponse`] shape the rest of the
//! chart subsystem expects, so callers swap to it without any render changes.
//!
//! Crypto OHLCV only: the Carrac-computed overlays (CPR, RSI,
//! sessions, indicators) are absent, exactly as on the other keyless path, and
//! the renderer already treats each as Option/default.

use anyhow::{Result, anyhow};
use serde::Deserialize;
use std::collections::HashMap;

use crate::chart::data::{Candle, ChartResponse, VolumeEntry};

const KRAKEN_OHLC_URL: &str = "https://api.kraken.com/0/public/OHLC";

/// Kraken's OHLC envelope: `{"error":[...],"result":{<pair>:[[...]],"last":N}}`.
/// The result map holds exactly one OHLC pair key plus a `"last"` cursor, so the
/// one OHLC row set is read as "the value under the key that isn't `last`".
#[derive(Deserialize)]
struct KrakenOhlcEnvelope {
    #[serde(default)]
    error: Vec<String>,
    #[serde(default)]
    result: HashMap<String, serde_json::Value>,
}

/// Fetch keyless OHLC from Kraken for `symbol` at timeframe `tf`, mapping into
/// the shared [`ChartResponse`]. `symbol` arrives in the same format the primary
/// keyless path receives (e.g. `BTCUSDT`); it is normalised to a Kraken pair
/// here. `bars` is unused by Kraken's endpoint (it returns a fixed trailing
/// window) but kept in the signature so the call site stays uniform.
pub async fn fetch_chart(
    http: &reqwest::Client,
    symbol: &str,
    tf: &str,
    _bars: u32,
) -> Result<ChartResponse> {
    let pair = kraken_pair(symbol)
        .ok_or_else(|| anyhow!("{symbol} is not available in the keyless fallback"))?;
    let interval =
        kraken_interval(tf).ok_or_else(|| anyhow!("kraken: unsupported timeframe {tf}"))?;
    let url = format!("{KRAKEN_OHLC_URL}?pair={pair}&interval={interval}");
    let resp = http
        .get(&url)
        .send()
        .await
        .map_err(|e| anyhow!("kraken: request failed: {e}"))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| anyhow!("kraken: reading body: {e}"))?;
    if !status.is_success() {
        let snippet: String = body.chars().take(160).collect();
        return Err(anyhow!("kraken: {pair} {interval} -> {status}: {snippet}"));
    }
    parse_kraken_ohlc(&body, symbol, tf)
}

/// Parse a Kraken OHLC JSON envelope into a [`ChartResponse`].
///
/// Each OHLC row is `[time, open, high, low, close, vwap, volume, count]` where
/// `time` is already UNIX SECONDS (the unit every downstream consumer expects —
/// no division) and open/high/low/close/vwap/volume are JSON strings.
///
/// A non-empty `error` array is a hard failure (returned as Err). The OHLC rows
/// live under the single result key that is not `"last"`.
///
/// Separated from the HTTP call so it is unit-testable with a hardcoded fixture
/// and zero network.
pub fn parse_kraken_ohlc(body: &str, symbol: &str, tf: &str) -> Result<ChartResponse> {
    let env: KrakenOhlcEnvelope =
        serde_json::from_str(body).map_err(|e| anyhow!("kraken: decoding OHLC JSON: {e}"))?;

    if !env.error.is_empty() {
        return Err(anyhow!("kraken: {}", env.error.join("; ")));
    }

    // The OHLC rows sit under the one result key that isn't the `last` cursor.
    let rows_val = env
        .result
        .iter()
        .find(|(k, _)| k.as_str() != "last")
        .map(|(_, v)| v)
        .ok_or_else(|| anyhow!("kraken: no OHLC series in response"))?;

    let rows: Vec<Vec<serde_json::Value>> = serde_json::from_value(rows_val.clone())
        .map_err(|e| anyhow!("kraken: decoding OHLC rows: {e}"))?;

    let mut ohlcv = Vec::with_capacity(rows.len());
    let mut volume = Vec::with_capacity(rows.len());
    for row in &rows {
        // [0]=time(s) [1]=open [2]=high [3]=low [4]=close [5]=vwap [6]=volume
        if row.len() < 7 {
            continue;
        }
        let time = val_i64(&row[0])?; // already seconds — no /1000
        let open = val_f64(&row[1])?;
        let high = val_f64(&row[2])?;
        let low = val_f64(&row[3])?;
        let close = val_f64(&row[4])?;
        let vol = val_f64(&row[6]).unwrap_or(0.0);
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
        symbol: symbol.trim().to_uppercase(),
        timeframe: tf.to_string(),
        ohlcv,
        volume,
        // Everything else is Carrac-server-computed; absent on the keyless
        // backend. The renderer treats each as Option/default.
        ..Default::default()
    })
}

/// Map a TUI symbol to a Kraken OHLC pair, or `None` when the asset has no
/// Kraken USD market (the caller surfaces a clean "not available" message).
///
/// The incoming symbol matches whatever the primary keyless path receives (a
/// bare ticker like `BTC` or a quoted pair like `BTCUSDT`). The quote suffix is
/// stripped, the base is mapped to Kraken's spelling (notably `XBT` for BTC and
/// `XDG` for DOGE), and a `USD` quote is appended.
pub fn kraken_pair(symbol: &str) -> Option<String> {
    let base = kraken_base(symbol)?;
    Some(format!("{base}USD"))
}

/// Normalise an incoming symbol to a Kraken base asset, or `None` if unmapped.
fn kraken_base(symbol: &str) -> Option<&'static str> {
    let s = symbol.trim().to_uppercase();
    // Strip a trailing quote suffix to recover the base ticker.
    const QUOTES: [&str; 6] = ["USDT", "USDC", "BUSD", "FDUSD", "TUSD", "USD"];
    let mut base = s.as_str();
    for q in QUOTES {
        if let Some(stripped) = base.strip_suffix(q)
            && !stripped.is_empty()
        {
            base = stripped;
            break;
        }
    }

    // Map the keyless starter assets (plus the common majors) to Kraken bases.
    // Kraken spells BTC as XBT and DOGE as XDG; the rest match their ticker.
    match base {
        "BTC" | "XBT" => Some("XBT"),
        "ETH" => Some("ETH"),
        "SOL" => Some("SOL"),
        "BNB" => Some("BNB"),
        "LINK" => Some("LINK"),
        "XRP" => Some("XRP"),
        "ADA" => Some("ADA"),
        "DOGE" | "XDG" => Some("XDG"),
        "LTC" => Some("LTC"),
        "DOT" => Some("DOT"),
        "AVAX" => Some("AVAX"),
        // Non-crypto starters (equity index, metals, energy) have no Kraken
        // crypto OHLC market — treat as unmapped so the caller reports cleanly.
        _ => None,
    }
}

/// Map a TUI timeframe to a Kraken OHLC interval in MINUTES.
/// Returns `None` for timeframes Kraken's OHLC endpoint does not offer.
pub fn kraken_interval(tf: &str) -> Option<u32> {
    match tf.trim() {
        "1m" => Some(1),
        "5m" => Some(5),
        "15m" => Some(15),
        "30m" => Some(30),
        "1H" | "1h" => Some(60),
        "4H" | "4h" => Some(240),
        "1D" | "1d" => Some(1440),
        "1W" | "1w" => Some(10080),
        _ => None,
    }
}

/// Read a number that Kraken encodes as a JSON string ("63000.50") or a raw
/// number.
fn val_f64(v: &serde_json::Value) -> Result<f64> {
    if let Some(n) = v.as_f64() {
        return Ok(n);
    }
    if let Some(s) = v.as_str() {
        return s
            .parse::<f64>()
            .map_err(|e| anyhow!("kraken: bad float {s:?}: {e}"));
    }
    Err(anyhow!("kraken: expected number, got {v}"))
}

fn val_i64(v: &serde_json::Value) -> Result<i64> {
    if let Some(n) = v.as_i64() {
        return Ok(n);
    }
    if let Some(s) = v.as_str() {
        return s
            .parse::<i64>()
            .map_err(|e| anyhow!("kraken: bad int {s:?}: {e}"));
    }
    Err(anyhow!("kraken: expected integer, got {v}"))
}

// ─── Tests (no network) ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // A captured Kraken OHLC envelope (public BTC data, XBTUSD). Rows are
    // [time(seconds), open, high, low, close, vwap, volume, count] with prices
    // as strings; the result key is the OHLC pair plus a `"last"` cursor.
    const SAMPLE: &str = r#"{
        "error": [],
        "result": {
            "XXBTZUSD": [
                [1770523200,"69072.5","69665.1","68938.2","69660.1","69261.6","131.03148834",4798],
                [1770537600,"69660.0","71180.1","69472.3","70941.0","70428.9","414.42106856",9059]
            ],
            "last": 1770609600
        }
    }"#;

    #[test]
    fn parses_candles_with_seconds_timestamps_in_ascending_order() {
        let resp = parse_kraken_ohlc(SAMPLE, "BTC", "4H").unwrap();
        assert_eq!(resp.ohlcv.len(), 2);
        // Kraken already gives SECONDS — the value is used as-is, no /1000.
        assert_eq!(resp.ohlcv[0].time, 1_770_523_200);
        assert_eq!(resp.ohlcv[1].time, 1_770_537_600);
        // Ascending order preserved.
        assert!(resp.ohlcv[1].time > resp.ohlcv[0].time);
        // Plausible unix-SECONDS magnitude (~1.7e9), not milliseconds (~1.7e12).
        assert!(resp.ohlcv[0].time < 2_000_000_000);
    }

    #[test]
    fn parses_ohlc_fields_from_string_encoded_numbers() {
        let resp = parse_kraken_ohlc(SAMPLE, "BTC", "4H").unwrap();
        let c = &resp.ohlcv[0];
        assert!((c.open - 69072.5).abs() < 1e-6);
        assert!((c.high - 69665.1).abs() < 1e-6);
        assert!((c.low - 68938.2).abs() < 1e-6);
        assert!((c.close - 69660.1).abs() < 1e-6);
    }

    #[test]
    fn populates_volume_from_index_6() {
        let resp = parse_kraken_ohlc(SAMPLE, "BTC", "4H").unwrap();
        assert_eq!(resp.volume.len(), 2);
        assert!((resp.volume[0].value - 131.031_488_34).abs() < 1e-6);
        // Volume timestamps are index-aligned and in seconds too.
        assert_eq!(resp.volume[0].time, resp.ohlcv[0].time);
    }

    #[test]
    fn reads_ohlc_under_the_non_last_result_key() {
        // The result map order is not guaranteed; the parser must pick the OHLC
        // key regardless of where `last` sits.
        let body = r#"{
            "error": [],
            "result": {
                "last": 42,
                "SOLUSD": [[1770523200,"150.10","151.00","149.50","150.80","150.5","12.3",100]]
            }
        }"#;
        let resp = parse_kraken_ohlc(body, "SOL", "1H").unwrap();
        assert_eq!(resp.ohlcv.len(), 1);
        assert_eq!(resp.ohlcv[0].time, 1_770_523_200);
        assert!((resp.ohlcv[0].close - 150.80).abs() < 1e-6);
    }

    #[test]
    fn carrac_only_fields_absent() {
        let resp = parse_kraken_ohlc(SAMPLE, "BTC", "4H").unwrap();
        assert!(resp.cpr.is_none());
        assert!(resp.daily_cpr.is_none());
        assert!(resp.weekly_cpr.is_none());
        assert!(resp.indicators.is_none());
        assert!(resp.rsi.is_empty());
        assert!(resp.sessions.is_empty());
    }

    #[test]
    fn response_carries_upper_cased_symbol_and_tf() {
        let resp = parse_kraken_ohlc(SAMPLE, "btc", "4H").unwrap();
        assert_eq!(resp.symbol, "BTC");
        assert_eq!(resp.timeframe, "4H");
    }

    #[test]
    fn non_empty_error_array_is_a_failure() {
        // `ChartResponse` deliberately does not derive Debug, so assert on the
        // Err arm directly rather than via `unwrap_err`.
        let body = r#"{"error":["EQuery:Unknown asset pair"],"result":{}}"#;
        match parse_kraken_ohlc(body, "BTC", "4H") {
            Err(e) => assert!(e.to_string().contains("Unknown asset pair")),
            Ok(_) => panic!("non-empty error array must be a failure"),
        }
    }

    #[test]
    fn short_rows_are_skipped_not_fatal() {
        // Fewer than 7 elements (no volume index) — skipped, not an error.
        let body = r#"{"error":[],"result":{"XXBTZUSD":[[1770523200,"1","2","0.5","1.5"]]}}"#;
        let resp = parse_kraken_ohlc(body, "BTC", "4H").unwrap();
        assert!(resp.ohlcv.is_empty());
    }

    #[test]
    fn pair_mapping_maps_starter_and_major_symbols() {
        // BTC → XBT, DOGE → XDG; the rest keep their ticker. Always USD-quoted.
        assert_eq!(kraken_pair("BTC").as_deref(), Some("XBTUSD"));
        assert_eq!(kraken_pair("ETH").as_deref(), Some("ETHUSD"));
        assert_eq!(kraken_pair("SOL").as_deref(), Some("SOLUSD"));
        assert_eq!(kraken_pair("BNB").as_deref(), Some("BNBUSD"));
        assert_eq!(kraken_pair("LINK").as_deref(), Some("LINKUSD"));
        assert_eq!(kraken_pair("XRP").as_deref(), Some("XRPUSD"));
        assert_eq!(kraken_pair("ADA").as_deref(), Some("ADAUSD"));
        assert_eq!(kraken_pair("DOGE").as_deref(), Some("XDGUSD"));
        assert_eq!(kraken_pair("LTC").as_deref(), Some("LTCUSD"));
        assert_eq!(kraken_pair("DOT").as_deref(), Some("DOTUSD"));
        assert_eq!(kraken_pair("AVAX").as_deref(), Some("AVAXUSD"));
    }

    #[test]
    fn pair_mapping_strips_quote_suffix_and_is_case_insensitive() {
        // Incoming symbols match the primary keyless path's format (quoted).
        assert_eq!(kraken_pair("BTCUSDT").as_deref(), Some("XBTUSD"));
        assert_eq!(kraken_pair("ethusdc").as_deref(), Some("ETHUSD"));
        assert_eq!(kraken_pair(" sol ").as_deref(), Some("SOLUSD"));
        assert_eq!(kraken_pair("DOGEUSD").as_deref(), Some("XDGUSD"));
    }

    #[test]
    fn pair_mapping_returns_none_for_non_crypto_starters() {
        // Equity index / metals / energy have no Kraken crypto OHLC market.
        assert_eq!(kraken_pair("SPX"), None);
        assert_eq!(kraken_pair("GOLD"), None);
        assert_eq!(kraken_pair("WTI"), None);
    }

    #[test]
    fn interval_mapping_uses_kraken_minutes() {
        assert_eq!(kraken_interval("1m"), Some(1));
        assert_eq!(kraken_interval("5m"), Some(5));
        assert_eq!(kraken_interval("15m"), Some(15));
        assert_eq!(kraken_interval("30m"), Some(30));
        assert_eq!(kraken_interval("1H"), Some(60));
        assert_eq!(kraken_interval("4H"), Some(240));
        assert_eq!(kraken_interval("1D"), Some(1440));
        assert_eq!(kraken_interval("1W"), Some(10080));
        // Unknown TF → None (caller surfaces an error rather than guessing).
        assert_eq!(kraken_interval("7H"), None);
    }
}
