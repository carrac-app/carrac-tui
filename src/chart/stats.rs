//! Range statistics + title/footer formatting for one-shot chart output.
//! Self-contained: no chrono, no terminal deps — pure data → string.

use crate::chart::data::Candle;
use crate::commands::chart::fmt_price;
use crate::format as f;

pub(crate) struct RangeStats {
    pub high: f64,
    pub high_ts: i64,
    pub low: f64,
    pub low_ts: i64,
    // These fields are available for callers (Task 3+); not yet read from outside this module.
    #[allow(dead_code)]
    pub first_close: f64,
    #[allow(dead_code)]
    pub last_close: f64,
    pub change_pct: f64,
}

pub(crate) fn compute_range_stats(candles: &[Candle]) -> Option<RangeStats> {
    let first = candles.first()?;
    let last = candles.last()?;
    let mut high = f64::MIN;
    let mut low = f64::MAX;
    let (mut high_ts, mut low_ts) = (first.time, first.time);
    for c in candles {
        if c.high > high {
            high = c.high;
            high_ts = c.time;
        }
        if c.low < low {
            low = c.low;
            low_ts = c.time;
        }
    }
    let change_pct = if first.close == 0.0 {
        0.0
    } else {
        (last.close / first.close - 1.0) * 100.0
    };
    Some(RangeStats {
        high,
        high_ts,
        low,
        low_ts,
        first_close: first.close,
        last_close: last.close,
        change_pct,
    })
}

/// `Thu 04 16:26` — UTC, rate.sx style. Civil-date math, no chrono.
/// Day-of-month only — ambiguous across month rollover (rate.sx convention).
pub(crate) fn fmt_ts_utc(ts: i64) -> String {
    const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"]; // 1970-01-01 was a Thursday
    let days = ts.div_euclid(86_400);
    let secs = ts.rem_euclid(86_400);
    let (hh, mm) = (secs / 3600, (secs % 3600) / 60);
    let weekday = WEEKDAYS[(days.rem_euclid(7)) as usize];
    // civil-from-days (Howard Hinnant's algorithm) for day-of-month
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    format!("{weekday} {d:02} {hh:02}:{mm:02}")
}

fn fmt_pct_colored(pct: f64, plain: bool) -> String {
    let raw = format!("{pct:.2}%");
    if plain {
        return raw;
    }
    if pct < 0.0 {
        f::alert(&raw)
    } else {
        f::ok(&raw)
    }
}

/// `BTC · 4h · viewing 10d · -2.93%`
///
/// When `stats` is `None`, the pct suffix is omitted and the title is
/// styled identically (bold symbol + dim separators / plain).
pub(crate) fn format_title(
    symbol: &str,
    timeframe: &str,
    window_label: &str,
    stats: Option<f64>, // change_pct; None = no data
    plain: bool,
) -> String {
    let pct_part = stats.map(|pct| format!(" · {}", fmt_pct_colored(pct, plain)));
    if plain {
        format!(
            "{symbol} · {timeframe} · viewing {window_label}{}",
            pct_part.as_deref().unwrap_or("")
        )
    } else {
        format!(
            "\x1b[1m{symbol}\x1b[0m · \x1b[2m{timeframe}\x1b[0m · \x1b[2mviewing {window_label}\x1b[0m{}",
            pct_part.as_deref().unwrap_or("")
        )
    }
}

/// `high $64112 (Thu 04 16:26) // low $61666 (Fri 05 08:10) // change -3.34%`
pub(crate) fn format_stats_footer(s: &RangeStats, plain: bool) -> String {
    let pct = fmt_pct_colored(s.change_pct, plain);
    format!(
        "high ${} ({}) // low ${} ({}) // change {}",
        fmt_price(s.high),
        fmt_ts_utc(s.high_ts),
        fmt_price(s.low),
        fmt_ts_utc(s.low_ts),
        pct
    )
}

/// Remove all CSI/SGR escape sequences — delegates to the canonical impl in format.
pub(crate) fn strip_ansi(s: &str) -> String {
    f::strip_ansi(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chart::data::Candle;

    fn c(time: i64, o: f64, h: f64, l: f64, cl: f64) -> Candle {
        Candle {
            time,
            open: o,
            high: h,
            low: l,
            close: cl,
        }
    }

    #[test]
    fn range_stats_basic() {
        let candles = vec![
            c(1_770_000_000, 100.0, 110.0, 95.0, 105.0),
            c(1_770_003_600, 105.0, 120.0, 104.0, 118.0),
            c(1_770_007_200, 118.0, 119.0, 90.0, 92.0),
        ];
        let s = compute_range_stats(&candles).unwrap();
        assert_eq!(s.high, 120.0);
        assert_eq!(s.high_ts, 1_770_003_600);
        assert_eq!(s.low, 90.0);
        assert_eq!(s.low_ts, 1_770_007_200);
        assert_eq!(s.first_close, 105.0);
        assert_eq!(s.last_close, 92.0);
        assert!((s.change_pct - (92.0 / 105.0 - 1.0) * 100.0).abs() < 1e-9);
    }

    #[test]
    fn range_stats_empty_is_none() {
        assert!(compute_range_stats(&[]).is_none());
    }

    #[test]
    fn fmt_ts_utc_known_value() {
        // Verified: date -u -d @1780590360 → 2026-06-04 16:26 UTC, Thursday
        assert_eq!(fmt_ts_utc(1_780_590_360), "Thu 04 16:26");
    }

    #[test]
    fn title_line_plain() {
        let t = format_title("BTC", "4h", "10d", Some(-2.93), true);
        assert_eq!(t, "BTC · 4h · viewing 10d · -2.93%");
    }

    #[test]
    fn title_line_plain_no_stats() {
        let t = format_title("BTC", "4h", "10d", None, true);
        assert_eq!(t, "BTC · 4h · viewing 10d");
    }

    #[test]
    fn title_line_colored_pct_present() {
        // Colored path: plain=false. Whether ANSI is emitted depends on the
        // global color OnceLock (non-TTY in CI → may be false). Assert the
        // percentage string is present in either case.
        let t = format_title("BTC", "4h", "10d", Some(-2.93), false);
        assert!(
            t.contains("-2.93%"),
            "pct must appear in colored title; got: {t}"
        );
        // Bold symbol must appear.
        assert!(t.contains("BTC"), "symbol present; got: {t}");
    }

    #[test]
    fn footer_plain() {
        // Verified: date -u -d @1780590360 → Thu 04 16:26 UTC
        //           date -u -d @1780647000 → Fri 05 08:10 UTC
        // change_pct = (61666/63800 - 1)*100 = -3.34%
        let candles = vec![
            c(1_780_590_360, 63528.0, 64112.0, 63400.0, 63800.0),
            c(1_780_647_000, 63800.0, 63900.0, 61666.0, 61666.0),
        ];
        let s = compute_range_stats(&candles).unwrap();
        let f = format_stats_footer(&s, true);
        assert_eq!(
            f,
            "high $64112 (Thu 04 16:26) // low $61666 (Fri 05 08:10) // change -3.34%"
        );
    }

    #[test]
    fn strip_ansi_removes_sgr() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m ok"), "red ok");
    }

    #[test]
    fn range_stats_single_candle() {
        let candles = vec![c(1_770_000_000, 100.0, 110.0, 95.0, 105.0)];
        let s = compute_range_stats(&candles).unwrap();
        assert_eq!(s.high, 110.0);
        assert_eq!(s.low, 95.0);
        assert_eq!(s.high_ts, s.low_ts);
        assert_eq!(s.change_pct, 0.0); // first == last
    }
}
