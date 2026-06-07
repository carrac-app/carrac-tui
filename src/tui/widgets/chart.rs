//! Embedded chart widget — keybind table + view state for use inside `carrac tui`.
//!
//! The standalone `carrac chart <SYM>` command (commands/chart.rs) is a richer
//! full-screen TUI with its own state machine that pre-dates this module.
//! This file is the **embeddable** version: a small `ChartWidgetState` the
//! parent owns + a `handle_key` that returns whether the chart consumed an
//! event. The render path is intentionally not implemented here yet — that
//! lands together with Track A (chart-in-AssetDetail integration).
//!
//! Design contract: when the host's focus is `AssetDetail`, the host calls
//! `handle_key` BEFORE the usual global key dispatch. Digits 1–9 are reserved
//! globally for tab switching (chart · positions · plays · …), so the chart's
//! timeframe control lives on `t`/`T` (cycle) and `d`/`w` (jump to 1D/1W), not
//! on number keys.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::chart::data::default_visible_bars;

/// View state for an embedded chart pane.
///
/// Holds presentation-side state only. The fetched OHLCV / overlay data
/// lives in `App` so the data refresh cycle stays a single source of truth.
#[derive(Clone, Debug)]
pub struct ChartWidgetState {
    pub tf: String,
    pub visible_bars: usize,
    pub offset_from_right: usize,
    pub y_zoom: f32,
    /// Vertical pan, in full visible-range units (0 = auto-centered;
    /// positive = panned up toward higher prices). Up/Down arrows.
    pub y_offset: f32,
    /// Effective [min, max] visible_bars the current canvas can render —
    /// written by the chart view each frame, consumed by the `[`/`]` clamp so
    /// the accumulator can't drift into invisibly-clamped territory.
    pub count_bounds: Option<(usize, usize)>,
    pub crosshair_col: Option<u16>,
    pub cpr_sel: crate::chart::render_term::CprSelect,
    pub show_ema: bool,
    /// Dotted EMA30/60 ribbon ('E'), web-parity trend coloring.
    pub show_ema_ribbon: bool,
    pub show_position: bool,
    pub show_volume: bool,
    pub show_rsi: bool,
    /// When true, CPR key-level overlays from `overlays_to_primitives` are included
    /// in PNG renders. Toggled with `o`.
    pub show_overlays: bool,
    /// When true, a MACD histogram + line pane is rendered below the chart.
    /// Toggled with `m`.
    pub show_macd: bool,
    /// When true, a 1-row market-session strip is drawn under the candles.
    /// Toggled with `z`. Auto-hides on 1D+ (the server sends no session data).
    pub show_sessions: bool,
    /// Candle drawing style: "block", "braille", "sextant", or "line". Cycled with `s`.
    pub style: String,
}

impl ChartWidgetState {
    /// Switch timeframe: reset pan and apply that TF's sensible default bar
    /// count (lower TFs show more bars). For the sub-cell styles the bar count
    /// also sets candle width, so this keeps each TF readable out of the box.
    pub fn set_tf(&mut self, tf: &str) {
        self.tf = tf.to_string();
        self.offset_from_right = 0;
        self.visible_bars = default_visible_bars(tf);
        // The y view (zoom + pan) is scale-specific — carrying it across a
        // TF switch left candles drawn off-center with no obvious cause.
        self.y_zoom = 1.0;
        self.y_offset = 0.0;
    }

    /// Record the canvas-derived `[`/`]` range (sub-cell styles) and snap the
    /// accumulator into it, so the zoom keys always bite on the next press.
    /// Intersected with the static product limits and kept non-empty so a
    /// degenerate canvas can't produce an inverted clamp.
    pub fn set_count_bounds(&mut self, lo: usize, hi: usize) {
        let lo = lo.clamp(VISIBLE_BARS_MIN, VISIBLE_BARS_MAX);
        let hi = hi.clamp(lo, VISIBLE_BARS_MAX);
        self.count_bounds = Some((lo, hi));
        self.visible_bars = self.visible_bars.clamp(lo, hi);
    }

    /// `[`/`]` clamp range: the canvas-derived bounds when the view has
    /// reported them (already intersected by `set_count_bounds`), else the
    /// static limits.
    fn effective_bar_bounds(&self) -> (usize, usize) {
        self.count_bounds
            .unwrap_or((VISIBLE_BARS_MIN, VISIBLE_BARS_MAX))
    }
}

impl Default for ChartWidgetState {
    fn default() -> Self {
        Self {
            tf: "4H".to_string(),
            visible_bars: default_visible_bars("4H"),
            offset_from_right: 0,
            y_zoom: 1.0,
            y_offset: 0.0,
            count_bounds: None,
            crosshair_col: None,
            cpr_sel: crate::chart::render_term::CprSelect::default(),
            show_ema: false,
            show_ema_ribbon: false,
            show_position: false,
            // Clean first paint: the chart opens bare and the user adds panes.
            show_volume: false,
            show_rsi: false,
            show_overlays: true,
            show_macd: false,
            show_sessions: false,
            style: "sextant".to_string(),
        }
    }
}

const VISIBLE_BARS_MIN: usize = 10;
const VISIBLE_BARS_MAX: usize = 200;
const Y_ZOOM_MIN: f32 = 0.1;
const Y_ZOOM_MAX: f32 = 10.0;

/// What the host should do after `handle_key`.
#[derive(Debug, PartialEq, Eq)]
pub enum ChartAction {
    /// Key was for the chart and handled. Do not dispatch further.
    Consumed,
    /// Key changed the timeframe — host should trigger a re-fetch.
    RetimeframeFetch,
    /// Key was not a chart binding — host should fall through to global dispatch.
    Ignored,
}

/// Dispatch a key event into a chart's view state.
///
/// Caller is responsible for only calling this when the chart owns focus
/// (i.e. `app.focus == FocusedPane::AssetDetail`). Returns `Ignored` for any
/// key the chart doesn't claim — the host then runs its normal global
/// handler.
pub fn handle_key(state: &mut ChartWidgetState, key: KeyEvent) -> ChartAction {
    if key.kind != KeyEventKind::Press {
        return ChartAction::Ignored;
    }

    match key.code {
        // ── TF jumps — `d`/`w` to daily/weekly; cycle all TFs with t/T (host).
        //    Digits 1–9 are reserved by the host for tab switching, so they
        //    never reach here.
        KeyCode::Char('d') => {
            state.set_tf("1D");
            ChartAction::RetimeframeFetch
        }
        KeyCode::Char('w') => {
            state.set_tf("1W");
            ChartAction::RetimeframeFetch
        }

        // ── pan history (h/l small, H/L big) ──────────────────────────────
        KeyCode::Char('h') => {
            state.offset_from_right = state.offset_from_right.saturating_add(1);
            ChartAction::Consumed
        }
        KeyCode::Char('l') => {
            state.offset_from_right = state.offset_from_right.saturating_sub(1);
            ChartAction::Consumed
        }
        KeyCode::Char('H') => {
            let big = (state.visible_bars / 2).max(1);
            state.offset_from_right = state.offset_from_right.saturating_add(big);
            ChartAction::Consumed
        }
        KeyCode::Char('L') => {
            let big = (state.visible_bars / 2).max(1);
            state.offset_from_right = state.offset_from_right.saturating_sub(big);
            ChartAction::Consumed
        }

        // ── y-pan via Alt+k / Alt+j — MUST precede the plain j/k zoom arms
        //    (an unguarded Char('j') arm matches any modifier combination).
        KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::ALT) => {
            state.y_offset = (state.y_offset + 0.15).min(3.0);
            ChartAction::Consumed
        }
        KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::ALT) => {
            state.y_offset = (state.y_offset - 0.15).max(-3.0);
            ChartAction::Consumed
        }

        // ── y-zoom: k zooms IN (candles larger), j zooms OUT (smaller).
        //    Lower y_zoom = tighter price range = taller candles.
        KeyCode::Char('k') => {
            state.y_zoom = (state.y_zoom * 0.9).max(Y_ZOOM_MIN);
            ChartAction::Consumed
        }
        KeyCode::Char('j') => {
            state.y_zoom = (state.y_zoom * 1.1).min(Y_ZOOM_MAX);
            ChartAction::Consumed
        }
        KeyCode::Char('K') => {
            state.y_zoom = (state.y_zoom * 0.7).max(Y_ZOOM_MIN);
            ChartAction::Consumed
        }
        KeyCode::Char('J') => {
            state.y_zoom = (state.y_zoom * 1.3).min(Y_ZOOM_MAX);
            ChartAction::Consumed
        }

        // ── visible-bar count (horizontal zoom) ───────────────────────────
        // `]` more bars, `[` fewer. In the sub-cell styles (braille/sextant)
        // candles auto-size to fill the width, so this also sets candle width:
        // more bars = thinner candles, fewer = wider.
        KeyCode::Char(']') | KeyCode::Char('+') | KeyCode::Char('=') => {
            let (lo, hi) = state.effective_bar_bounds();
            state.visible_bars = (state.visible_bars + 5).clamp(lo, hi);
            ChartAction::Consumed
        }
        KeyCode::Char('[') | KeyCode::Char('-') => {
            let (lo, hi) = state.effective_bar_bounds();
            state.visible_bars = state.visible_bars.saturating_sub(5).clamp(lo, hi);
            ChartAction::Consumed
        }

        // ── y-pan: Up/Down (Alt+k/Alt+j handled before the y-zoom arms) ──
        KeyCode::Up => {
            state.y_offset = (state.y_offset + 0.15).min(3.0);
            ChartAction::Consumed
        }
        KeyCode::Down => {
            state.y_offset = (state.y_offset - 0.15).max(-3.0);
            ChartAction::Consumed
        }

        // ── reset pan + zoom ──────────────────────────────────────────────
        KeyCode::Char('0') => {
            state.offset_from_right = 0;
            state.y_zoom = 1.0;
            state.y_offset = 0.0;
            ChartAction::Consumed
        }

        // ── overlay toggles ───────────────────────────────────────────────
        KeyCode::Char('c') => {
            state.cpr_sel = state.cpr_sel.cycle();
            ChartAction::Consumed
        }
        KeyCode::Char('e') => {
            state.show_ema = !state.show_ema;
            ChartAction::Consumed
        }
        KeyCode::Char('E') => {
            state.show_ema_ribbon = !state.show_ema_ribbon;
            ChartAction::Consumed
        }
        KeyCode::Char('p') => {
            state.show_position = !state.show_position;
            ChartAction::Consumed
        }
        KeyCode::Char('v') => {
            state.show_volume = !state.show_volume;
            ChartAction::Consumed
        }
        KeyCode::Char('R') => {
            state.show_rsi = !state.show_rsi;
            ChartAction::Consumed
        }
        KeyCode::Char('o') => {
            state.show_overlays = !state.show_overlays;
            ChartAction::Consumed
        }
        KeyCode::Char('m') => {
            state.show_macd = !state.show_macd;
            // When turning MACD ON, signal a refetch so the fetcher loads the
            // MACD series immediately. Turning it OFF just hides (no fetch).
            if state.show_macd {
                ChartAction::RetimeframeFetch
            } else {
                ChartAction::Consumed
            }
        }

        // ── toggle market-session strip ──────────────────────────────────
        KeyCode::Char('z') => {
            state.show_sessions = !state.show_sessions;
            ChartAction::Consumed
        }

        // ── cycle candle drawing style: block → braille → sextant → line ──
        KeyCode::Char('s') => {
            state.style = match state.style.to_lowercase().as_str() {
                "sextant" => "line",
                _ => "sextant",
            }
            .to_string();
            ChartAction::Consumed
        }

        // ── crosshair clear (left-click sets it; cleared by host) ─────────
        KeyCode::Char('x') => {
            state.crosshair_col = None;
            ChartAction::Consumed
        }

        // ── anything else is for the host ─────────────────────────────────
        _ => ChartAction::Ignored,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn bracket_zoom_clamps_to_canvas_bounds() {
        let mut s = ChartWidgetState {
            visible_bars: 40,
            ..Default::default()
        };
        // Canvas range 6..35 intersects the static floor (10) → 10..35, and
        // setting the bounds snaps the accumulator into range.
        s.set_count_bounds(6, 35);
        assert_eq!(s.count_bounds, Some((10, 35)));
        assert_eq!(s.visible_bars, 35, "accumulator snapped into range");
        let _ = handle_key(&mut s, press(KeyCode::Char(']')));
        assert_eq!(
            s.visible_bars, 35,
            "] at canvas max must not drift invisibly"
        );
        let _ = handle_key(&mut s, press(KeyCode::Char('[')));
        assert_eq!(s.visible_bars, 30, "[ must bite immediately");
        s.visible_bars = 12;
        let _ = handle_key(&mut s, press(KeyCode::Char('[')));
        assert_eq!(s.visible_bars, 10, "[ stops at the effective floor");
        let _ = handle_key(&mut s, press(KeyCode::Char('[')));
        assert_eq!(s.visible_bars, 10, "[ at the floor must not drift below");
    }

    #[test]
    fn bracket_zoom_without_bounds_uses_static_limits() {
        let mut s = ChartWidgetState {
            count_bounds: None,
            ..Default::default()
        };
        s.visible_bars = VISIBLE_BARS_MAX - 2;
        let _ = handle_key(&mut s, press(KeyCode::Char(']')));
        assert_eq!(s.visible_bars, VISIBLE_BARS_MAX);
    }

    #[test]
    fn y_pan_keys_shift_and_reset() {
        let mut s = ChartWidgetState::default();
        let _ = handle_key(&mut s, press(KeyCode::Up));
        assert!(s.y_offset > 0.0, "Up pans toward higher prices");
        let _ = handle_key(&mut s, press(KeyCode::Down));
        let _ = handle_key(&mut s, press(KeyCode::Down));
        assert!(s.y_offset < 0.0, "Down pans toward lower prices");
        let _ = handle_key(&mut s, press(KeyCode::Char('0')));
        assert_eq!(s.y_offset, 0.0, "0 resets the y pan");
    }

    #[test]
    fn set_tf_resets_y_view() {
        let mut s = ChartWidgetState {
            y_zoom: 0.5,
            ..Default::default()
        };
        s.y_offset = 1.2;
        s.set_tf("1D");
        assert_eq!(s.y_zoom, 1.0);
        assert_eq!(s.y_offset, 0.0);
    }

    #[test]
    fn default_style_is_sextant() {
        let s = ChartWidgetState::default();
        assert_eq!(s.style, "sextant", "default style should be sextant");
    }

    #[test]
    fn default_panes_are_off_for_a_clean_first_paint() {
        let s = ChartWidgetState::default();
        assert!(!s.show_rsi, "RSI pane should be OFF by default");
        assert!(!s.show_volume, "volume pane should be OFF by default");
    }

    #[test]
    fn tf_switch_requests_fetch_and_resets_pan() {
        let mut s = ChartWidgetState {
            offset_from_right: 5,
            ..Default::default()
        };
        let action = handle_key(&mut s, press(KeyCode::Char('d')));
        assert_eq!(action, ChartAction::RetimeframeFetch);
        assert_eq!(s.tf, "1D");
        assert_eq!(s.offset_from_right, 0);
    }

    #[test]
    fn tf_jump_keys() {
        // Digits are host tab-switch keys; the chart only handles d/w jumps.
        for (k, tf) in [(KeyCode::Char('d'), "1D"), (KeyCode::Char('w'), "1W")] {
            let mut s = ChartWidgetState::default();
            handle_key(&mut s, press(k));
            assert_eq!(s.tf, tf);
        }
        // A digit must NOT be claimed by the chart (host owns it for tabs).
        let mut s = ChartWidgetState::default();
        assert_eq!(
            handle_key(&mut s, press(KeyCode::Char('1'))),
            ChartAction::Ignored
        );
    }

    #[test]
    fn z_toggles_sessions() {
        let mut s = ChartWidgetState::default();
        assert!(!s.show_sessions, "sessions off by default");
        assert_eq!(
            handle_key(&mut s, press(KeyCode::Char('z'))),
            ChartAction::Consumed
        );
        assert!(s.show_sessions, "z turns sessions on");
        let _ = handle_key(&mut s, press(KeyCode::Char('z')));
        assert!(!s.show_sessions, "z toggles sessions back off");
    }

    #[test]
    fn pan_h_increases_offset() {
        let mut s = ChartWidgetState::default();
        let _ = handle_key(&mut s, press(KeyCode::Char('h')));
        assert_eq!(s.offset_from_right, 1);
    }

    #[test]
    fn big_pan_uses_half_window() {
        let mut s = ChartWidgetState {
            visible_bars: 40,
            ..Default::default()
        };
        let _ = handle_key(&mut s, press(KeyCode::Char('H')));
        assert_eq!(s.offset_from_right, 20);
    }

    #[test]
    fn pan_l_does_not_underflow() {
        let mut s = ChartWidgetState::default();
        let _ = handle_key(&mut s, press(KeyCode::Char('l')));
        assert_eq!(s.offset_from_right, 0);
    }

    #[test]
    fn zero_resets_pan_and_zoom() {
        let mut s = ChartWidgetState {
            offset_from_right: 10,
            ..Default::default()
        };
        s.y_zoom = 2.0;
        let _ = handle_key(&mut s, press(KeyCode::Char('0')));
        assert_eq!(s.offset_from_right, 0);
        assert!((s.y_zoom - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn density_increase_capped() {
        let mut s = ChartWidgetState {
            visible_bars: VISIBLE_BARS_MAX - 2,
            ..Default::default()
        };
        let _ = handle_key(&mut s, press(KeyCode::Char(']')));
        assert_eq!(s.visible_bars, VISIBLE_BARS_MAX);
    }

    #[test]
    fn density_decrease_floored() {
        let mut s = ChartWidgetState {
            visible_bars: VISIBLE_BARS_MIN + 2,
            ..Default::default()
        };
        let _ = handle_key(&mut s, press(KeyCode::Char('[')));
        assert_eq!(s.visible_bars, VISIBLE_BARS_MIN);
    }

    #[test]
    fn y_zoom_k_in_j_out() {
        let mut s = ChartWidgetState {
            y_zoom: 1.0,
            ..Default::default()
        };
        let _ = handle_key(&mut s, press(KeyCode::Char('k')));
        assert!(
            s.y_zoom < 1.0,
            "k zooms in → smaller y_zoom → larger candles"
        );
        let mut s = ChartWidgetState {
            y_zoom: 1.0,
            ..Default::default()
        };
        let _ = handle_key(&mut s, press(KeyCode::Char('j')));
        assert!(
            s.y_zoom > 1.0,
            "j zooms out → larger y_zoom → smaller candles"
        );
    }

    #[test]
    fn y_zoom_clamped() {
        // k decreases y_zoom (zoom in) → clamp at MIN.
        let mut s = ChartWidgetState {
            y_zoom: Y_ZOOM_MIN,
            ..Default::default()
        };
        let _ = handle_key(&mut s, press(KeyCode::Char('k')));
        assert!(s.y_zoom >= Y_ZOOM_MIN);
        // j increases y_zoom (zoom out) → clamp at MAX.
        let mut s = ChartWidgetState {
            y_zoom: Y_ZOOM_MAX,
            ..Default::default()
        };
        let _ = handle_key(&mut s, press(KeyCode::Char('j')));
        assert!(s.y_zoom <= Y_ZOOM_MAX);
    }

    #[test]
    fn unknown_keys_ignored() {
        let mut s = ChartWidgetState::default();
        // `y` has no chart binding — it must fall through to the host.
        let action = handle_key(&mut s, press(KeyCode::Char('y')));
        assert_eq!(action, ChartAction::Ignored);
    }

    #[test]
    fn esc_q_tab_arent_chart_keys() {
        // These belong to the host (quit/cycle-focus). Chart must NOT claim them.
        for k in [
            KeyCode::Esc,
            KeyCode::Char('q'),
            KeyCode::Tab,
            KeyCode::Char('?'),
            KeyCode::Char('r'),
        ] {
            let mut s = ChartWidgetState::default();
            assert_eq!(
                handle_key(&mut s, press(k)),
                ChartAction::Ignored,
                "{:?} should be Ignored",
                k
            );
        }
    }

    #[test]
    fn toggles_flip() {
        let mut s = ChartWidgetState::default();
        let prev = s.show_volume;
        let _ = handle_key(&mut s, press(KeyCode::Char('v')));
        assert_ne!(prev, s.show_volume);
        let _ = handle_key(&mut s, press(KeyCode::Char('v')));
        assert_eq!(prev, s.show_volume);
    }

    #[test]
    fn o_toggles_show_overlays() {
        let mut s = ChartWidgetState::default();
        assert!(s.show_overlays, "show_overlays should default to true");
        let action = handle_key(&mut s, press(KeyCode::Char('o')));
        assert_eq!(action, ChartAction::Consumed);
        assert!(!s.show_overlays, "o should flip show_overlays to false");
        let _ = handle_key(&mut s, press(KeyCode::Char('o')));
        assert!(
            s.show_overlays,
            "second o should restore show_overlays to true"
        );
    }

    #[test]
    fn m_toggles_show_macd() {
        let mut s = ChartWidgetState::default();
        assert!(!s.show_macd, "show_macd should default to false");
        // Turning MACD ON signals a refetch so data loads immediately.
        let action = handle_key(&mut s, press(KeyCode::Char('m')));
        assert_eq!(action, ChartAction::RetimeframeFetch);
        assert!(s.show_macd, "m should flip show_macd to true");
        // Turning MACD OFF just hides — no fetch needed.
        let action2 = handle_key(&mut s, press(KeyCode::Char('m')));
        assert_eq!(action2, ChartAction::Consumed);
        assert!(!s.show_macd, "second m should restore show_macd to false");
    }

    #[test]
    fn style_cycles_sextant_line_and_back() {
        // candle styles: sextant ⇄ line.
        let mut s = ChartWidgetState::default();
        assert_eq!(s.style, "sextant");
        let _ = handle_key(&mut s, press(KeyCode::Char('s')));
        assert_eq!(s.style, "line");
        let _ = handle_key(&mut s, press(KeyCode::Char('s')));
        assert_eq!(s.style, "sextant");
    }

    #[test]
    fn alt_jk_pans_y_axis() {
        let mut s = ChartWidgetState::default();
        let alt = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT);
        let _ = handle_key(&mut s, alt('k'));
        assert!(s.y_offset > 0.0, "Alt+k pans up");
        assert_eq!(s.y_zoom, 1.0, "Alt+k must not zoom");
        let _ = handle_key(&mut s, alt('j'));
        let _ = handle_key(&mut s, alt('j'));
        assert!(s.y_offset < 0.0, "Alt+j pans down");
        assert_eq!(s.y_zoom, 1.0, "Alt+j must not zoom");
    }

    #[test]
    fn x_clears_crosshair() {
        let mut s = ChartWidgetState {
            crosshair_col: Some(42),
            ..Default::default()
        };
        let _ = handle_key(&mut s, press(KeyCode::Char('x')));
        assert!(s.crosshair_col.is_none());
    }

    #[test]
    fn key_release_is_ignored() {
        // Important: we only handle Press. Release/Repeat must fall through.
        let mut s = ChartWidgetState::default();
        let mut key = press(KeyCode::Char('1'));
        key.kind = KeyEventKind::Release;
        let action = handle_key(&mut s, key);
        assert_eq!(action, ChartAction::Ignored);
        // TF must not have changed.
        assert_eq!(s.tf, "4H");
    }
}
