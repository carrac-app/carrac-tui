/// Full-screen standalone chart TUI (`carrac chart <SYM>`).
///
/// Contains the app state machine, background fetch task, key/mouse event loop,
/// and the frame-render callback. All pure data and render helpers live in
/// sibling modules (`data`, `canvas`, `render_term`, `render`, `kgp`).
use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::time::interval;

use crate::chart::data::default_visible_bars;
use crate::chart::data::{ChartResponse, enrich_cpr_with_sr_levels, fetch_days_for_tf, fetch_macd};
use crate::chart::provider::Provider;
use crate::chart::render_term::{
    CprSelect, ansi_to_ratatui_text, fmt_volume, pane_slice, render_line, render_line_sized,
    render_macd_pane, render_macd_to_buf, render_rsi_to_buf, render_sessions_to_buf,
    render_sextant_candles_sized, render_volume_to_buf, render_x_axis_2col,
};
use crate::chart::{kgp, primitives, render};
use crate::client::Client;
use crate::commands::chart::constants::{
    FOOTER_H, HEADER_H, HLINE_LABEL_SPILL, MACD_PANE_H, MAX_CANDLE_WIDTH, MAX_VISIBLE_BARS,
    RSI_PANE_H, VOL_PANE_H, Y_LABEL_W,
};
use crate::commands::chart::{ChartArgs, fmt_price};
use crate::config::Config;
use crate::types::positions::{Position, PositionsResponse};

// ─── Chart TUI state ─────────────────────────────────────────────────────────

struct ChartState {
    symbol: String,
    tf: String,
    /// Visible candles (render-time slice). Fetch window is fixed per TF.
    bars: u32,
    style: String,
    position_id: Option<u64>,
    cpr_sel: CprSelect,
    show_ema: bool,
    show_ema_ribbon: bool,
    show_position: bool,
    show_volume: bool,
    show_rsi: bool,
    /// When true, CPR key-level overlays from `overlays_to_primitives` are included
    /// in PNG renders. Toggled with `o`.
    show_overlays: bool,
    /// When true, a MACD histogram + line pane is rendered below the chart.
    /// Toggled with `m`.
    show_macd: bool,
    /// When true, a 1-row market-session strip is drawn under the candles.
    /// Toggled with `z`. Auto-hides on 1D+ (server sends no session data).
    show_sessions: bool,

    /// How many candles from the right edge we've panned back (0 = live/latest).
    offset_from_right: usize,
    /// Vertical zoom factor: 1.0 = natural, <1.0 = tighter price range, >1.0 = wider.
    y_zoom: f32,

    /// Crosshair: terminal column index pinned by mouse click (None = no crosshair).
    crosshair_col: Option<u16>,

    // Data
    chart_data: Option<ChartResponse>,
    pos_overlay: Option<Position>,

    // Status
    last_fetched: Option<Instant>,
    fetch_status: FetchStatus,
    should_quit: bool,
}

#[derive(Clone)]
enum FetchStatus {
    Idle,
    Fetching,
    StaleError(String), // fetch failed — error text shown while no chart data is on screen
}

impl ChartState {
    fn new(args: &ChartArgs) -> Self {
        let overlays: Vec<String> = args.indicators.iter().map(|s| s.to_uppercase()).collect();
        let cpr_sel = if overlays.is_empty() || overlays.contains(&"CPR".to_string()) {
            CprSelect::default()
        } else {
            CprSelect::Off
        };
        let show_ema = overlays.contains(&"EMA21".to_string());
        // Use TF-aware default if user didn't explicitly set --bars (default was 80)
        let bars = if args.bars == 80 {
            default_visible_bars(&args.tf) as u32
        } else {
            args.bars
        };

        ChartState {
            symbol: args.symbol.to_uppercase(),
            tf: args.tf.clone(),
            bars,
            style: args.style.clone(),
            position_id: args.position,
            cpr_sel,
            show_ema,
            show_ema_ribbon: false,
            show_position: args.position.is_some(),
            show_volume: true,
            show_rsi: true,
            show_overlays: true,
            show_macd: false,
            show_sessions: false,
            offset_from_right: 0,
            y_zoom: 1.0,
            crosshair_col: None,
            chart_data: None,
            pos_overlay: None,
            last_fetched: None,
            fetch_status: FetchStatus::Idle,
            should_quit: false,
        }
    }

    fn secs_since_fetch(&self) -> Option<u64> {
        self.last_fetched.map(|t| t.elapsed().as_secs())
    }

    fn tf_label(&self) -> &str {
        &self.tf
    }

    fn status_line(&self, refresh_secs: u64) -> String {
        let pan_suffix = if self.offset_from_right > 0 {
            format!("  · [+{} bars back]", self.offset_from_right)
        } else {
            String::new()
        };
        match &self.fetch_status {
            FetchStatus::Fetching => format!("↻ …{pan_suffix}"),
            FetchStatus::StaleError(_) => format!("↻ stale{pan_suffix}"),
            FetchStatus::Idle => {
                if let Some(elapsed) = self.secs_since_fetch() {
                    let remaining = refresh_secs.saturating_sub(elapsed);
                    format!("↻ {}s{}", remaining, pan_suffix)
                } else {
                    format!("↻ …{pan_suffix}")
                }
            }
        }
    }
}

// ─── Background fetch update ─────────────────────────────────────────────────

// Short-lived channel message; the `Data` payload dominates by design and is
// consumed immediately, so the size skew doesn't warrant boxing.
#[allow(clippy::large_enum_variant)]
enum ChartUpdate {
    Data {
        chart: ChartResponse,
        pos: Option<Position>,
    },
    FetchError(String),
    Fetching,
}

// ─── Entry point ─────────────────────────────────────────────────────────────

pub async fn run(args: ChartArgs) -> Result<()> {
    let cfg = Config::load()?;
    let client = Client::new(&cfg)?;
    let provider = Provider::from_config(&cfg, client);

    if args.png || args.out.is_some() {
        // PNG render is a Carrac-only path (drawings, server overlays). In demo
        // mode the chart still renders in the terminal; PNG is not supported.
        if provider.is_demo() {
            return Err(anyhow!(
                "carrac chart --png needs a Carrac API key — carrac.cc/reg-tui"
            ));
        }
        return run_png(&provider, args).await;
    }

    if args.once {
        run_once(&provider, args).await
    } else {
        run_tui(provider, args).await
    }
}

// ─── Static one-shot mode ────────────────────────────────────────────────────

async fn run_once(provider: &Provider, args: ChartArgs) -> Result<()> {
    let symbol = args.symbol.to_uppercase();
    let style = args.style.to_lowercase();

    let days = fetch_days_for_tf(&args.tf);
    // NOTE: run_once intentionally does NOT enrich CPR S/R (mirrors prior
    // behaviour — the one-shot render does not fetch the monthly bar).
    let data = provider
        .fetch_chart(&symbol, &args.tf, days)
        .await
        .map_err(|e| anyhow!("carrac chart: fetch failed: {e}"))?;

    if data.ohlcv.is_empty() {
        return Err(anyhow!(
            "carrac chart: no candle data for {symbol} {}",
            args.tf
        ));
    }

    // Position overlays are a Carrac-only surface (no positions backend in the
    // keyless demo). Best-effort on Carrac; absent in demo.
    let pos_overlay: Option<Position> = match (args.position, provider) {
        (Some(pid), Provider::Carrac(client)) => {
            let resp: PositionsResponse = client
                .get("/api/positions/live")
                .await
                .map_err(|e| anyhow!("carrac chart: position fetch failed: {e}"))?;
            resp.positions.into_iter().find(|p| p.id == pid)
        }
        _ => None,
    };

    let overlays: Vec<String> = args.indicators.iter().map(|s| s.to_uppercase()).collect();
    let cpr_sel = if overlays.is_empty() || overlays.contains(&"CPR".to_string()) {
        CprSelect::default()
    } else {
        CprSelect::Off
    };
    let show_cpr = cpr_sel != CprSelect::Off; // legacy line renderer keeps a bool
    let show_ema = overlays.contains(&"EMA21".to_string());

    let (term_cols, term_rows) = crossterm::terminal::size()
        .map(|(c, r)| (c as usize, r as usize))
        .unwrap_or((120, 30));
    // Renderers subtract the y-label gutter internally, but hline labels
    // (live price, position lines) spill past the canvas — leave headroom so
    // those rows don't wrap on exactly-terminal-width output.
    let chart_cols = term_cols.saturating_sub(HLINE_LABEL_SPILL).min(200);
    // Derive rows from terminal height: subtract title + 2-line x-axis + footer.
    // Clamp so the body is always at least 8 rows and at most 60.
    let chart_rows = term_rows.saturating_sub(4).clamp(8, 60);

    // MACD OFF by default for --once (volume/RSI panes were block-style-only)
    let show_macd = false;

    let mut output = if style == "line" {
        render_line(
            &data,
            &symbol,
            chart_cols,
            show_cpr,
            show_ema,
            pos_overlay.as_ref(),
            args.bars,
        )
    } else {
        // sextant — the default candle style
        let opts = crate::chart::oneshot::OneshotOpts {
            cols: chart_cols,
            rows: chart_rows,
            show_ema,
            show_ema_ribbon: false,
            plain: false,
            quiet: false,
            window_label: format!("{} bars", args.bars),
            visible_bars: args.bars,
            pos: pos_overlay.clone(),
        };
        crate::chart::oneshot::render_oneshot(&data, &opts)
    };
    if show_macd {
        output += &render_macd_pane(&data, chart_cols, MACD_PANE_H as usize, args.bars, 0);
    }

    // Write to stdout; treat EPIPE (consumer closed the pipe, e.g. `| head`) as
    // silent success so we don't panic with a broken-pipe error.
    {
        use std::io::Write;
        if let Err(e) = std::io::stdout().write_all(output.as_bytes()) {
            if e.kind() == std::io::ErrorKind::BrokenPipe {
                return Ok(());
            }
            return Err(anyhow!("carrac chart: write error: {e}"));
        }
    }
    Ok(())
}

// ─── PNG render mode ─────────────────────────────────────────────────────────

/// `carrac chart BTC --png` or `carrac chart BTC --out /tmp/chart.png`
///
/// Fetches the same OHLCV endpoint used by the ASCII renderer, plus the user's
/// drawings from /api/drawings?asset=&timeframe=. Renders client-side via
/// plotters and emits inline KGP (kitty) or saves to disk. Zero server load.
async fn run_png(provider: &Provider, args: ChartArgs) -> Result<()> {
    // Guarded by the caller: PNG is a Carrac-only path. Pull the inner client.
    let client = match provider {
        Provider::Carrac(c) => c,
        Provider::Binance(_) => {
            return Err(anyhow!(
                "carrac chart --png needs a Carrac API key — carrac.cc/reg-tui"
            ));
        }
    };
    let t0 = std::time::Instant::now();
    let symbol = args.symbol.to_uppercase();

    let overlays: Vec<String> = args.indicators.iter().map(|s| s.to_uppercase()).collect();
    let overlay_cpr = overlays.is_empty() || overlays.contains(&"CPR".to_string());
    let overlay_ema = overlays.contains(&"EMA21".to_string());

    // Fetch OHLCV (same endpoint as the ASCII path).
    eprintln!("fetching {} {} …", symbol, args.tf);
    let days = fetch_days_for_tf(&args.tf);
    let chart_path = format!("/api/chart/{}?timeframe={}&days={}", symbol, args.tf, days);
    let mut data: ChartResponse = client
        .get(&chart_path)
        .await
        .map_err(|e| anyhow!("carrac chart --png: fetch failed: {e}"))?;

    if data.ohlcv.is_empty() {
        return Err(anyhow!(
            "carrac chart --png: no candle data for {symbol} {}",
            args.tf
        ));
    }

    let fetch_ms = t0.elapsed().as_millis();

    // Fetch user drawings. Warn + continue on failure (graceful degrade for
    // users without per-user API keys).
    let drawings_path = format!("/api/drawings?asset={}&timeframe={}", symbol, args.tf);
    let drawings: Vec<render::Drawing> =
        match client.get::<render::DrawingsResponse>(&drawings_path).await {
            Ok(resp) => {
                if !resp.drawings.is_empty() {
                    eprintln!("drawings: {} overlays loaded", resp.drawings.len());
                }
                resp.drawings
            }
            Err(e) => {
                eprintln!("carrac chart --png: drawings fetch failed (continuing without): {e}");
                vec![]
            }
        };

    let drawings_ms = t0.elapsed().as_millis() - fetch_ms;

    // Enrich CprData with S/R levels computed from prior closed monthly bar.
    // Best-effort: on failure the chart still renders with P/TC/BC only.
    if overlay_cpr && data.cpr.is_some() {
        enrich_cpr_with_sr_levels(client, &symbol, &mut data).await;
    }

    if overlay_cpr && data.cpr.is_none() {
        eprintln!(
            "carrac chart --png: CPR data unavailable for {symbol} {}.",
            args.tf
        );
    }

    // Fetch MACD unconditionally for the rich PNG export path — the render
    // uses it when present; ignoring it when None is already handled.
    data.macd = fetch_macd(client, &symbol, &args.tf, days).await;

    // Bridge EmaRibbon: main's EmaPoint → render::SeriesPoint.
    let ema_opt: Option<render::EmaRibbon> = data.indicators.as_ref().and_then(|ind| {
        ind.ema_ribbon.as_ref().map(|r| {
            render::EmaRibbon {
                fast: r
                    .fast
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .map(|p| render::SeriesPoint {
                        time: p.time,
                        value: p.value,
                    })
                    .collect(),
                slow: vec![], // main's EmaRibbon only has fast; slow unavailable
            }
        })
    });

    // Determine image dimensions from terminal.
    let term_cols = crossterm::terminal::size().map(|(c, _)| c).unwrap_or(120);
    let img_w = (term_cols as u32 * 8).clamp(600, 2000);
    let img_h = 480u32;

    eprintln!("rendering PNG {}×{} …", img_w, img_h);
    let mut prims = primitives::drawings_to_primitives(&drawings);
    // overlays (key levels etc.) always included in the standalone --png path
    prims.extend(primitives::overlays_to_primitives(&data));
    let png = render::render_png_rich(
        &data.ohlcv,
        data.cpr.as_ref(),
        ema_opt.as_ref(),
        overlay_cpr,
        overlay_ema,
        &prims,
        data.macd.as_ref(),
        img_w,
        img_h,
    )
    .map_err(|e| anyhow!("carrac chart --png: render failed: {e}"))?;

    let render_ms = t0.elapsed().as_millis() - fetch_ms - drawings_ms;

    if let Some(out_path) = &args.out {
        kgp::write_png_file(&png, out_path)?;
    } else {
        // Print one-line header above the inline image.
        println!(
            "{} {}  {} candles",
            symbol,
            data.timeframe,
            data.ohlcv.len()
        );
        kgp::display_png(&png, term_cols)?;
    }

    if std::env::var("CARRAC_TIMING").is_ok() {
        eprintln!(
            "timing: fetch {}ms  drawings {}ms  render {}ms  total {}ms",
            fetch_ms,
            drawings_ms,
            render_ms,
            t0.elapsed().as_millis()
        );
    }

    Ok(())
}

// ─── Interactive TUI mode ────────────────────────────────────────────────────

async fn run_tui(provider: Provider, args: ChartArgs) -> Result<()> {
    use crate::tui;
    use ratatui::layout::{Constraint, Direction, Layout};
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

    let refresh_secs: u64 = std::env::var("CX_CHART_REFRESH_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);

    let watchlist = crate::helpers::watchlist::load();

    let mut state = ChartState::new(&args);

    // ── terminal setup ───────────────────────────────────────────────────────
    let mut terminal = tui::enter()?;

    // ── channels ─────────────────────────────────────────────────────────────
    let (tx, mut rx) = mpsc::unbounded_channel::<ChartUpdate>();
    // trigger_tx: send params to trigger an immediate re-fetch.
    // Tuple: (symbol, tf, pos_id, show_macd)
    let (trigger_tx, mut trigger_rx) =
        mpsc::unbounded_channel::<(String, String, Option<u64>, bool)>();

    // ── background fetcher task ───────────────────────────────────────────────
    {
        let provider = provider.clone();
        let tx2 = tx.clone();
        let symbol0 = state.symbol.clone();
        let tf0 = state.tf.clone();
        let pos0 = state.position_id;

        tokio::spawn(async move {
            let mut cur_symbol = symbol0;
            let mut cur_tf = tf0;
            let mut cur_pos = pos0;
            let mut cur_show_macd = false; // MACD is off at startup

            // fire first fetch immediately
            let _ = tx2.send(ChartUpdate::Fetching);
            do_fetch(
                &provider,
                &tx2,
                &cur_symbol,
                &cur_tf,
                cur_pos,
                cur_show_macd,
            )
            .await;

            let mut ticker = interval(Duration::from_secs(refresh_secs));
            ticker.tick().await; // consume the first immediate tick

            loop {
                tokio::select! {
                    biased;
                    // Manual trigger with possibly new params (TF/symbol/macd change)
                    Some((sym, tf, pos, show_macd)) = trigger_rx.recv() => {
                        cur_symbol = sym;
                        cur_tf = tf;
                        cur_pos = pos;
                        cur_show_macd = show_macd;
                        let _ = tx2.send(ChartUpdate::Fetching);
                        do_fetch(&provider, &tx2, &cur_symbol, &cur_tf, cur_pos, cur_show_macd).await;
                        // Reset the ticker after manual refresh
                        ticker = interval(Duration::from_secs(refresh_secs));
                        ticker.tick().await;
                    }
                    // Auto-refresh tick
                    _ = ticker.tick() => {
                        let _ = tx2.send(ChartUpdate::Fetching);
                        do_fetch(&provider, &tx2, &cur_symbol, &cur_tf, cur_pos, cur_show_macd).await;
                    }
                }
            }
        });
    }

    // ── input thread (key + mouse) ────────────────────────────────────────────
    enum InputEvent {
        Key(crossterm::event::KeyEvent),
        MouseDown(u16), // column of left-click
    }
    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<InputEvent>();
    std::thread::spawn(move || {
        loop {
            match crossterm::event::read() {
                Ok(Event::Key(key)) if key_tx.send(InputEvent::Key(key)).is_err() => {
                    break;
                }
                Ok(Event::Mouse(me))
                    if me.kind == MouseEventKind::Down(MouseButton::Left)
                        && key_tx.send(InputEvent::MouseDown(me.column)).is_err() =>
                {
                    break;
                }
                _ => {}
            }
        }
    });

    // ── render tick ───────────────────────────────────────────────────────────
    let mut render_tick = interval(Duration::from_millis(100));

    // helper: trigger a refetch (TF, symbol, or macd toggle changed)
    macro_rules! refetch {
        () => {
            let _ = trigger_tx.send((
                state.symbol.clone(),
                state.tf.clone(),
                state.position_id,
                state.show_macd,
            ));
        };
    }

    // ── event loop ────────────────────────────────────────────────────────────
    loop {
        tokio::select! {
            biased;

            Some(input_ev) = key_rx.recv() => {
                match input_ev {
                    InputEvent::MouseDown(col) => {
                        // Pin/toggle crosshair at the clicked column
                        if state.crosshair_col == Some(col) {
                            state.crosshair_col = None; // click same column again → clear
                        } else {
                            state.crosshair_col = Some(col);
                        }
                    }
                    InputEvent::Key(key) if key.kind == KeyEventKind::Press => {
                    match key.code {
                        // quit
                        KeyCode::Char('q') | KeyCode::Esc => {
                            state.should_quit = true;
                        }
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            state.should_quit = true;
                        }

                        // TF switching — triggers refetch, resets pan offset
                        KeyCode::Char('1') => {
                            state.tf = "1H".to_string();
                            state.offset_from_right = 0;
                            refetch!();
                        }
                        KeyCode::Char('4') => {
                            state.tf = "4H".to_string();
                            state.offset_from_right = 0;
                            refetch!();
                        }
                        KeyCode::Char('d') | KeyCode::Char('D') => {
                            state.tf = "1D".to_string();
                            state.offset_from_right = 0;
                            refetch!();
                        }
                        KeyCode::Char('w') | KeyCode::Char('W') => {
                            state.tf = "1W".to_string();
                            state.offset_from_right = 0;
                            refetch!();
                        }

                        // manual refresh
                        KeyCode::Char('r') => {
                            refetch!();
                        }

                        // visible-bar zoom — render-time only, NO refetch. For the
                        // sub-cell styles this is also the candle width control:
                        // more bars = thinner candles, fewer = wider.
                        KeyCode::Char('+') | KeyCode::Char('=') | KeyCode::Char(']') => {
                            let max_bars = state.chart_data.as_ref()
                                .map(|d| d.ohlcv.len() as u32)
                                .unwrap_or(2500);
                            state.bars = (state.bars + 10).min(max_bars);
                        }
                        KeyCode::Char('-') | KeyCode::Char('[') => {
                            state.bars = state.bars.saturating_sub(10).max(10);
                        }

                        // cycle the CPR set: monthly → weekly → daily → off
                        KeyCode::Char('c') => {
                            state.cpr_sel = state.cpr_sel.cycle();
                        }

                        // toggle EMA21
                        KeyCode::Char('e') => {
                            state.show_ema = !state.show_ema;
                            }
                            KeyCode::Char('E') => {
                                state.show_ema_ribbon = !state.show_ema_ribbon;
                        }

                        // toggle position overlay
                        KeyCode::Char('p') => {
                            if state.position_id.is_some() {
                                state.show_position = !state.show_position;
                            }
                        }

                        // toggle volume pane
                        KeyCode::Char('v') => {
                            state.show_volume = !state.show_volume;
                        }

                        // toggle RSI pane
                        KeyCode::Char('R') => {
                            state.show_rsi = !state.show_rsi;
                        }

                        // toggle overlay primitives (key levels etc.) in PNG renders
                        KeyCode::Char('o') => {
                            state.show_overlays = !state.show_overlays;
                        }

                        // toggle MACD pane — trigger a refetch when turning ON so
                        // data loads immediately; turning OFF just hides (no fetch)
                        KeyCode::Char('m') => {
                            state.show_macd = !state.show_macd;
                            if state.show_macd {
                                refetch!();
                            }
                        }

                        // toggle market-session strip
                        KeyCode::Char('z') => {
                            state.show_sessions = !state.show_sessions;
                        }

                        // cycle candle drawing style: block → braille → sextant → line
                        KeyCode::Char('s') => {
                            state.style = match state.style.to_lowercase().as_str() {
                                "sextant" => "line",
                                _ => "sextant",
                            }.to_string();
                        }

                        // ── hjkl panning + y-zoom ───────────────────────────
                        // h / H — pan left (older candles)
                        KeyCode::Char('h') => {
                            let total = state.chart_data.as_ref().map(|d| d.ohlcv.len()).unwrap_or(0);
                            let eff_bars = default_visible_bars(&state.tf).min(total / 2 + 1).max(20);
                            let max_offset = total.saturating_sub(eff_bars);
                            let step = eff_bars.div_ceil(10).max(1);
                            state.offset_from_right = (state.offset_from_right + step).min(max_offset);
                        }
                        KeyCode::Char('H') => {
                            let total = state.chart_data.as_ref().map(|d| d.ohlcv.len()).unwrap_or(0);
                            let eff_bars = default_visible_bars(&state.tf).min(total / 2 + 1).max(20);
                            let max_offset = total.saturating_sub(eff_bars);
                            let step = eff_bars.div_ceil(2).max(1);
                            state.offset_from_right = (state.offset_from_right + step).min(max_offset);
                        }
                        // l / L — pan right (newer candles, toward live)
                        KeyCode::Char('l') => {
                            let total = state.chart_data.as_ref().map(|d| d.ohlcv.len()).unwrap_or(0);
                            let eff_bars = default_visible_bars(&state.tf).min(total / 2 + 1).max(20);
                            let step = eff_bars.div_ceil(10).max(1);
                            state.offset_from_right = state.offset_from_right.saturating_sub(step);
                        }
                        KeyCode::Char('L') => {
                            let total = state.chart_data.as_ref().map(|d| d.ohlcv.len()).unwrap_or(0);
                            let eff_bars = default_visible_bars(&state.tf).min(total / 2 + 1).max(20);
                            let step = eff_bars.div_ceil(2).max(1);
                            state.offset_from_right = state.offset_from_right.saturating_sub(step);
                        }
                        // k / K — zoom in on price axis (tighter range)
                        KeyCode::Char('k') => {
                            state.y_zoom = (state.y_zoom * 0.85).max(0.25);
                        }
                        KeyCode::Char('K') => {
                            state.y_zoom = (state.y_zoom * 0.85 * 0.85).max(0.25);
                        }
                        // j / J — zoom out on price axis (wider range)
                        KeyCode::Char('j') => {
                            state.y_zoom = (state.y_zoom / 0.85).min(4.0);
                        }
                        KeyCode::Char('J') => {
                            state.y_zoom = (state.y_zoom / 0.85 / 0.85).min(4.0);
                        }
                        // 0 — reset pan + y-zoom to defaults
                        KeyCode::Char('0') => {
                            state.offset_from_right = 0;
                            state.y_zoom = 1.0;
                        }

                        // cycle watchlist assets
                        KeyCode::Char(',') => {
                            if !watchlist.is_empty() {
                                let cur_idx = watchlist.iter().position(|s| s == &state.symbol);
                                let next_idx = match cur_idx {
                                    Some(i) => i.saturating_sub(1),
                                    None => 0,
                                };
                                state.symbol = watchlist[next_idx].clone();
                                refetch!();
                            }
                        }
                        KeyCode::Char('.') => {
                            if !watchlist.is_empty() {
                                let cur_idx = watchlist.iter().position(|s| s == &state.symbol);
                                let next_idx = match cur_idx {
                                    Some(i) => (i + 1).min(watchlist.len() - 1),
                                    None => 0,
                                };
                                state.symbol = watchlist[next_idx].clone();
                                refetch!();
                            }
                        }

                        // clear crosshair
                        KeyCode::Char('x') => {
                            state.crosshair_col = None;
                        }

                        _ => {
                            if std::env::var_os("CX_CHART_DEBUG_KEYS").is_some() {
                                eprintln!("[carrac-chart key] code={:?} modifiers={:?}", key.code, key.modifiers);
                            }
                        }
                    }
                    }
                    InputEvent::Key(_) => {} // non-press key events (release, repeat) — ignore
                }
            }

            _ = render_tick.tick() => {
                // drain pending updates
                while let Ok(update) = rx.try_recv() {
                    match update {
                        ChartUpdate::Data { chart, pos } => {
                            state.chart_data = Some(chart);
                            state.pos_overlay = pos;
                            state.last_fetched = Some(Instant::now());
                            state.fetch_status = FetchStatus::Idle;
                        }
                        ChartUpdate::FetchError(e) => {
                            state.fetch_status = FetchStatus::StaleError(e);
                        }
                        ChartUpdate::Fetching => {
                            state.fetch_status = FetchStatus::Fetching;
                        }
                    }
                }

                // draw
                terminal.draw(|frame| {
                    let area = frame.area();
                    let total_h = area.height;

                    // Sub-pane heights
                    let xaxis_h: u16 = 1; // combined tick+label row
                    let session_h: u16 = if state.show_sessions
                        && state.chart_data.as_ref().map(|d| !d.sessions.is_empty()).unwrap_or(false)
                    { 1 } else { 0 };
                    let vol_h: u16 = if state.show_volume { VOL_PANE_H } else { 0 };
                    let rsi_h: u16 = if state.show_rsi { RSI_PANE_H } else { 0 };
                    let macd_h: u16 = if state.show_macd { MACD_PANE_H } else { 0 };
                    let footer_h = FOOTER_H;
                    let header_h = HEADER_H;
                    let chart_h = total_h.saturating_sub(header_h + xaxis_h + footer_h + session_h + vol_h + rsi_h + macd_h);

                    let mut constraints = vec![
                        Constraint::Length(header_h),
                        Constraint::Length(chart_h),
                        Constraint::Length(xaxis_h),
                    ];
                    if session_h > 0  { constraints.push(Constraint::Length(session_h)); }
                    if state.show_volume { constraints.push(Constraint::Length(vol_h)); }
                    if state.show_rsi    { constraints.push(Constraint::Length(rsi_h)); }
                    if state.show_macd   { constraints.push(Constraint::Length(macd_h)); }
                    constraints.push(Constraint::Length(footer_h));

                    let chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints(constraints)
                        .split(area);

                    let width = area.width as usize;

                    // ── header ────────────────────────────────────────────────
                    let header_text = build_header(&state, width, refresh_secs);
                    let header_para = Paragraph::new(header_text)
                        .style(Style::default().add_modifier(Modifier::BOLD));
                    frame.render_widget(header_para, chunks[0]);

                    // ── chart body ────────────────────────────────────────────
                    let chart_rows = chart_h as usize;
                    let chart_width = width;

                    let style_str = state.style.to_lowercase();
                    // The count-driven sub-cell styles (braille, sextant) share one pitch
                    // across candles, x-axis and the volume/RSI panes so all line up.
                    let subcell = style_str.as_str() != "line";

                    let (chart_ratatui, xaxis_ratatui) = if let Some(ref data) = state.chart_data {
                        let pos = if state.show_position { state.pos_overlay.as_ref() } else { None };
                        let chart_cols = chart_width.saturating_sub(Y_LABEL_W).min(MAX_VISIBLE_BARS);
                        let canvas_cols = chart_cols.saturating_sub(Y_LABEL_W);
                        // One slice for body + x-axis, so labels stay aligned under the candles.
                        let total = data.ohlcv.len();
                        let (start, end, candle_width_tui) = pane_slice(
                            total, canvas_cols, state.bars, state.offset_from_right, subcell,
                        );
                        let candle_slice = &data.ohlcv[start..end];

                        // Chart body (no x-axis rows appended — renders chart_rows lines only)
                        let raw_body = match style_str.as_str() {
                            "line" => render_line_sized(data, chart_cols, chart_rows, state.cpr_sel, state.show_ema, pos, state.bars, state.offset_from_right, state.y_zoom, 0.0),
                            _ => render_sextant_candles_sized(data, chart_cols, chart_rows, state.cpr_sel, state.show_ema, state.show_ema_ribbon, pos, state.bars, state.offset_from_right, state.y_zoom, 0.0),
                        };
                        let xaxis_raw = render_x_axis_2col(candle_slice, canvas_cols, candle_width_tui, Y_LABEL_W, data);
                        (ansi_to_ratatui_text(&raw_body), ansi_to_ratatui_text(&xaxis_raw))
                    } else {
                        let placeholder = match &state.fetch_status {
                            FetchStatus::StaleError(e) => format!("  chart fetch error: {e}"),
                            _ => "  Loading chart data…".to_string(),
                        };
                        (ratatui::text::Text::raw(placeholder), ratatui::text::Text::raw(""))
                    };

                    let chart_para = Paragraph::new(chart_ratatui)
                        .block(Block::default().borders(Borders::NONE))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(chart_para, chunks[1]);

                    // ── x-axis ────────────────────────────────────────────────
                    let xaxis_para = Paragraph::new(xaxis_ratatui)
                        .block(Block::default().borders(Borders::NONE))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(xaxis_para, chunks[2]);

                    // ── sub-panes ─────────────────────────────────────────────
                    // Volume and RSI panes are written DIRECTLY into frame.buffer_mut() rather
                    // than via Paragraph::new(ansi_to_ratatui_text(…)). Direct writes eliminate
                    // every possible ANSI-carryover or stale-cell phantom artifact: every cell
                    // in the pane area is written exactly once (or set to Cell::default() for
                    // empty cells), so no old content can persist between frames.
                    let mut sub_idx = 3usize;
                    if session_h > 0 {
                        let sess_area = chunks[sub_idx];
                        if let Some(ref data) = state.chart_data {
                            let chart_cols = chart_width.saturating_sub(Y_LABEL_W).min(MAX_VISIBLE_BARS);
                            let buf = frame.buffer_mut();
                            render_sessions_to_buf(data, chart_cols, sess_area, buf, state.bars, state.offset_from_right, subcell);
                        }
                        sub_idx += 1;
                    }
                    if state.show_volume {
                        let vol_area = chunks[sub_idx];
                        if let Some(ref data) = state.chart_data {
                            let chart_cols = chart_width.saturating_sub(Y_LABEL_W).min(MAX_VISIBLE_BARS);
                            let buf = frame.buffer_mut();
                            render_volume_to_buf(data, chart_cols, vol_area, buf, state.bars, state.offset_from_right, subcell);
                        }
                        sub_idx += 1;
                    }
                    if state.show_rsi {
                        let rsi_area = chunks[sub_idx];
                        if let Some(ref data) = state.chart_data {
                            let chart_cols = chart_width.saturating_sub(Y_LABEL_W).min(MAX_VISIBLE_BARS);
                            let buf = frame.buffer_mut();
                            render_rsi_to_buf(data, chart_cols, rsi_area, buf, state.bars, state.offset_from_right, subcell);
                        }
                        sub_idx += 1;
                    }
                    if state.show_macd {
                        let macd_area = chunks[sub_idx];
                        if let Some(ref data) = state.chart_data {
                            let chart_cols = chart_width.saturating_sub(Y_LABEL_W).min(MAX_VISIBLE_BARS);
                            let buf = frame.buffer_mut();
                            render_macd_to_buf(data, chart_cols, macd_area, buf, state.bars, state.offset_from_right, subcell);
                        }
                        sub_idx += 1;
                    }

                    // ── footer ────────────────────────────────────────────────
                    let footer_text = build_footer(&state);
                    let footer_para = Paragraph::new(footer_text)
                        .block(Block::default().borders(Borders::TOP))
                        .style(Style::default().fg(Color::DarkGray));
                    frame.render_widget(footer_para, chunks[sub_idx]);

                    // ── crosshair overlay ─────────────────────────────────────
                    // Draw a vertical │ line across chart + xaxis + volume + rsi panes
                    // at the pinned column, in bright yellow. Drawn last so it's topmost.
                    if let Some(xhair_col) = state.crosshair_col {
                        let xhair_style = ratatui::style::Style::default()
                            .fg(ratatui::style::Color::Indexed(93)); // bright yellow
                        let buf = frame.buffer_mut();
                        // Span across all panes except header (chunk[0]) and footer
                        let pane_top = chunks[1].y;
                        let pane_bot = chunks[sub_idx].y; // footer top = one past last data pane
                        for row in pane_top..pane_bot {
                            if xhair_col < area.width {
                                let cell = buf.cell_mut((xhair_col, row));
                                if let Some(c) = cell {
                                    c.set_symbol("│");
                                    c.set_style(xhair_style);
                                }
                            }
                        }
                    }
                })?;
            }

            _ = tokio::signal::ctrl_c() => {
                state.should_quit = true;
            }
        }

        if state.should_quit {
            break;
        }
    }

    crate::tui::exit(terminal)?;
    Ok(())
}

// ─── async fetch helper ───────────────────────────────────────────────────────

async fn do_fetch(
    provider: &Provider,
    tx: &mpsc::UnboundedSender<ChartUpdate>,
    symbol: &str,
    tf: &str,
    pos_id: Option<u64>,
    show_macd: bool,
) {
    let days = fetch_days_for_tf(tf);
    match provider.fetch_chart(symbol, tf, days).await {
        Ok(mut data) => {
            // CPR S/R + MACD supplement: Carrac-only, no-op in demo.
            provider
                .enrich(symbol, tf, days, &mut data, show_macd)
                .await;
            // Position overlay is a Carrac-only surface (no positions backend
            // in the keyless demo).
            let pos: Option<Position> = match (pos_id, provider) {
                (Some(pid), Provider::Carrac(client)) => {
                    match client.get::<PositionsResponse>("/api/positions/live").await {
                        Ok(resp) => resp.positions.into_iter().find(|p| p.id == pid),
                        Err(_) => None,
                    }
                }
                _ => None,
            };
            let _ = tx.send(ChartUpdate::Data { chart: data, pos });
        }
        Err(e) => {
            let _ = tx.send(ChartUpdate::FetchError(format!("{e:#}")));
        }
    }
}

// ─── header builder ──────────────────────────────────────────────────────────

fn build_header(state: &ChartState, width: usize, refresh_secs: u64) -> String {
    let last_price = state.chart_data.as_ref().and_then(|d| {
        let c = d.ohlcv.last()?;
        let prev = d.ohlcv.get(d.ohlcv.len().saturating_sub(2))?;
        let pct = (c.close - prev.close) / prev.close * 100.0;
        Some((c.close, pct))
    });

    let price_str = match last_price {
        Some((price, pct)) => {
            let sign = if pct >= 0.0 { "+" } else { "" };
            format!("{} ({}{:.2}%)", fmt_price(price), sign, pct)
        }
        None => "—".to_string(),
    };

    let bars_str = format!("{} zoom", state.bars);

    let status = state.status_line(refresh_secs);

    // Crosshair info replaces/appends price string when active
    let xhair_str = state.crosshair_col.and_then(|col| {
        // Map terminal column → candle index → full OHLCV + timestamp
        let data = state.chart_data.as_ref()?;
        let canvas_col = (col as usize).checked_sub(Y_LABEL_W)?;
        let total = data.ohlcv.len();
        let chart_cols = width.saturating_sub(Y_LABEL_W).min(MAX_VISIBLE_BARS);
        let canvas_cols = chart_cols.saturating_sub(Y_LABEL_W);
        let eff_bars = (state.bars as usize).min(total / 2 + 1).max(20);
        let candle_width_hover = (canvas_cols / eff_bars).clamp(1, MAX_CANDLE_WIDTH);
        let slice_n = (canvas_cols / candle_width_hover).min(total);
        let ci = canvas_col / candle_width_hover; // dynamic cols per candle
        let end = total.saturating_sub(state.offset_from_right);
        let start = end.saturating_sub(slice_n);
        let candles = &data.ohlcv[start..end];
        let c = candles.get(ci)?;
        #[allow(deprecated)]
        let dt = DateTime::<Utc>::from_timestamp(c.time, 0).unwrap_or_default();
        let ts_str = match state.tf.to_uppercase().as_str() {
            "1M" | "5M" | "15M" | "1H" | "4H" => dt.format("%m-%d %H:%M").to_string(),
            _ => dt.format("%m-%d").to_string(),
        };
        // Volume for this candle (from vol_map if available)
        let vol_map: std::collections::HashMap<i64, f64> =
            data.volume.iter().map(|v| (v.time, v.value)).collect();
        let vol_str = vol_map
            .get(&c.time)
            .map(|&v| fmt_volume(v))
            .unwrap_or_else(|| "—".to_string());
        Some(format!(
            "O {} H {} L {} C {}  V {}  {}",
            fmt_price(c.open),
            fmt_price(c.high),
            fmt_price(c.low),
            fmt_price(c.close),
            vol_str,
            ts_str
        ))
    });

    let left = if let Some(xs) = &xhair_str {
        format!(
            " {} · {} · {} · [x] {}",
            state.symbol,
            state.tf_label(),
            bars_str,
            xs
        )
    } else {
        format!(
            " {} · {} · {} · {}",
            state.symbol,
            state.tf_label(),
            bars_str,
            price_str
        )
    };
    let right = format!("{} ", status);

    let pad = width.saturating_sub(left.len() + right.len());
    format!("{}{}{}", left, " ".repeat(pad), right)
}

// ─── footer builder ───────────────────────────────────────────────────────────

fn build_footer(state: &ChartState) -> String {
    let cpr_ind = match state.cpr_sel {
        CprSelect::Off => "[c] CPR:off".to_string(),
        sel => format!("[c] CPR:{}", sel.label()),
    };
    let ema_ind = if state.show_ema {
        "[e] EMA21:ON"
    } else {
        "[e] EMA21:off"
    };
    let pos_ind = if state.position_id.is_some() {
        if state.show_position {
            "[p] pos:ON"
        } else {
            "[p] pos:off"
        }
    } else {
        "[p] —"
    };
    let vol_ind = if state.show_volume {
        "[v] vol:ON"
    } else {
        "[v] vol:off"
    };
    let rsi_ind = if state.show_rsi {
        "[R] RSI:ON"
    } else {
        "[R] RSI:off"
    };
    let macd_ind = if state.show_macd {
        "[m] MACD:ON"
    } else {
        "[m] MACD:off"
    };
    let sess_ind = if state.show_sessions {
        "[z] sess:ON"
    } else {
        "[z] sess:off"
    };
    let overlays_ind = if state.show_overlays {
        "[o] ovr:ON"
    } else {
        "[o] ovr:off"
    };
    let style_ind = format!("[s] style:{}", state.style.to_lowercase());

    format!(
        " [q/Esc] quit  [1] 1H  [4] 4H  [d] 1D  [w] 1W  [r] refresh  {pos_ind}  {cpr_ind}  {ema_ind}  [,/.] asset\n [h/l] pan  [H/L] big pan  [j/k] y-zoom  [J/K] big zoom  [0] reset  [+/-/[/]] bars  {vol_ind}  {rsi_ind}  {macd_ind}  {sess_ind}  {overlays_ind}  {style_ind}"
    )
}
