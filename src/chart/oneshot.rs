//! One-shot chart composition: title + sextant body + range-stats footer.
//! Shared by the `render` stdin subcommand and `carrac chart` one-shot mode.

use crate::chart::data::ChartResponse;
use crate::chart::render_term::{pane_slice, render_sextant_candles_sized};
use crate::chart::stats::{compute_range_stats, format_stats_footer, format_title, strip_ansi};
use crate::commands::chart::constants::Y_LABEL_W;
use crate::types::positions::Position;

pub(crate) struct OneshotOpts {
    pub cols: usize,
    pub rows: usize,
    pub show_ema: bool,
    /// The dotted EMA30/60 ribbon (curl `?ema` maps here — the :help page
    /// has always described the option as the ribbon).
    pub show_ema_ribbon: bool,
    pub plain: bool,
    pub quiet: bool,
    pub window_label: String,
    pub visible_bars: u32,
    /// Position overlay (entry/stop/target hlines). None = no overlay.
    pub pos: Option<Position>,
}

pub(crate) fn render_oneshot(data: &ChartResponse, opts: &OneshotOpts) -> String {
    let plain = opts.plain || !crate::format::color_on_pub();
    // must mirror render_sextant_candles_sized's internal slice — footer describes what's on screen
    let canvas_cols = opts.cols.saturating_sub(Y_LABEL_W);
    let (start, end, _) = pane_slice(data.ohlcv.len(), canvas_cols, opts.visible_bars, 0, true);
    let shown = &data.ohlcv[start..end];
    let stats = compute_range_stats(shown);

    let body = render_sextant_candles_sized(
        data,
        opts.cols,
        opts.rows,
        crate::chart::render_term::CprSelect::Off, // CPR off for one-shot/curl (spec §4)
        opts.show_ema,
        opts.show_ema_ribbon,
        opts.pos.as_ref(),
        opts.visible_bars,
        0,   // offset_from_right: live edge
        1.0, // y_zoom
        0.0, // y_offset: auto-centered
    );

    let mut out = String::new();
    if !opts.quiet {
        // Both Some and None branches use the same bold/dim styling via format_title.
        out.push_str(&format_title(
            &data.symbol,
            &data.timeframe,
            &opts.window_label,
            stats.as_ref().map(|s| s.change_pct),
            plain,
        ));
        out.push('\n');
    }
    out.push_str(&body);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    // Volume and RSI sub-panes are only available for the block style. Sextant
    // one-shot output is chart body + title/footer only.
    if !opts.quiet
        && let Some(s) = &stats
    {
        out.push_str(&format_stats_footer(s, plain));
        out.push('\n');
    }
    if plain { strip_ansi(&out) } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> crate::chart::data::ChartResponse {
        let s = include_str!("../../tests/fixtures/chart_btc_4h.json");
        serde_json::from_str(s).expect("fixture deserializes")
    }

    fn default_opts() -> OneshotOpts {
        OneshotOpts {
            cols: 80,
            rows: 26,
            show_ema: false,
            show_ema_ribbon: false,
            plain: true,
            quiet: false,
            window_label: "10d".into(),
            visible_bars: 60,
            pos: None,
        }
    }

    #[test]
    fn oneshot_has_title_chart_footer() {
        let data = fixture();
        let opts = default_opts();
        let out = render_oneshot(&data, &opts);
        let lines: Vec<&str> = out.lines().collect();
        assert!(
            lines[0].starts_with("BTC · 4h · viewing 10d ·"),
            "got: {}",
            lines[0]
        );
        assert!(
            lines.last().unwrap().starts_with("high $"),
            "got: {}",
            lines.last().unwrap()
        );
        assert!(lines.len() > 10, "chart body present");
        assert!(!out.contains('\x1b'), "plain mode strips ANSI");
    }

    #[test]
    fn oneshot_quiet_drops_title_and_footer() {
        let data = fixture();
        let opts = OneshotOpts {
            quiet: true,
            ..default_opts()
        };
        let out = render_oneshot(&data, &opts);
        assert!(!out.contains("viewing"));
        assert!(!out.contains("high $"));
    }

    #[test]
    fn oneshot_empty_data_no_panic() {
        let mut data = fixture();
        data.ohlcv.clear();
        let opts = OneshotOpts {
            window_label: "1d".into(),
            visible_bars: 24,
            ..default_opts()
        };
        let out = render_oneshot(&data, &opts);
        assert!(!out.contains("high $"), "no footer without data");
        assert!(!out.contains('%'), "no pct in title without data");
    }

    #[test]
    fn oneshot_empty_data_title_styled() {
        // Stats=None branch must still emit the same bold/dim-styled title (plain flag → no ANSI).
        let mut data = fixture();
        data.ohlcv.clear();
        let opts = OneshotOpts {
            window_label: "1d".into(),
            visible_bars: 24,
            plain: true,
            ..default_opts()
        };
        let out = render_oneshot(&data, &opts);
        let first = out.lines().next().unwrap_or("");
        assert!(
            first.contains("BTC") && first.contains("viewing 1d"),
            "got: {first}"
        );
        assert!(!first.contains('%'), "no pct without data");
    }

    #[test]
    fn oneshot_stats_match_visible_slice() {
        use crate::chart::render_term::pane_slice;
        use crate::chart::stats::compute_range_stats;
        use crate::commands::chart::constants::Y_LABEL_W;

        let data = fixture();
        let opts = default_opts();
        let out = render_oneshot(&data, &opts);
        let footer = out.lines().last().unwrap().to_string();

        // Recompute expected from the same pane_slice the implementation uses
        let canvas_cols = opts.cols.saturating_sub(Y_LABEL_W);
        let (start, end, _) = pane_slice(data.ohlcv.len(), canvas_cols, opts.visible_bars, 0, true);
        let shown = &data.ohlcv[start..end];
        let stats = compute_range_stats(shown).expect("fixture has candles");

        let expected_high = format!("high ${:.0}", stats.high);
        assert!(
            footer.starts_with("high $"),
            "footer must start with high $, got: {footer}"
        );
        assert!(
            footer.starts_with(&expected_high),
            "footer high must match clamped slice high; got: {footer}, expected prefix: {expected_high}"
        );
    }

    #[test]
    fn oneshot_pos_some_renders_entry_label() {
        use crate::types::positions::{Direction, Position};

        let data = fixture();
        // Pick prices inside the BTC 4h fixture range (~60k-80k).
        let pos = Position {
            id: 1,
            asset: "BTC".into(),
            direction: Direction::Long,
            entry_price: Some(65000.0),
            stop_price: Some(63000.0),
            target_price: Some(70000.0),
            current_price: None,
            unrealized_pnl_pct: None,
            current_rr: None,
            risk_status: None,
            scanner_verdict: None,
            linked_play: None,
            time_in_trade: None,
            setup_type: None,
            opened_at: None,
            planned_rr: None,
            unrealized_pnl_usd: None,
            timeframe: None,
        };
        let opts = OneshotOpts {
            plain: false,
            pos: Some(pos),
            ..default_opts()
        };
        // NOTE: never force_enable_color() in tests — the OnceLock is
        // process-global and poisons every other test in the binary. The
        // hline label text appears in plain output too.
        let out = render_oneshot(&data, &opts);
        // The sextant renderer calls collect_hlines which emits "EN 65000" label.
        assert!(
            out.contains("EN"),
            "entry hline label 'EN' should appear in output; got:\n{out}"
        );
    }
}
