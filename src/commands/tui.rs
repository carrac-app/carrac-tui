use anyhow::Result;
use chrono::Utc;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::sync::mpsc;
use tokio::time::interval;

use crossterm::event::{Event, KeyEvent, KeyEventKind, MouseEvent};

use crate::app::state::{App, ChartFetchSpec, NewsFeedPreset, Update, View};
use crate::chart::provider::Provider;
use crate::client::Client;
use crate::commands::chart::{ChartResponse, fetch_days_for_tf};
use crate::config::Config;
use crate::helpers::agent_bundle;
use crate::tui;
use crate::tui::event::{Action, handle_input, handle_mouse};
use crate::tui::layout::build_layout;
use crate::tui::widgets;
use crate::tui::widgets::view_report as view_report_widget;

/// A periodic ticker that SKIPS missed ticks rather than bursting them. If a
/// fetch outruns its interval (e.g. a slow or partial server outage), the next
/// tick fires one interval after the fetch ends instead of immediately firing
/// every tick that elapsed — so pollers never amplify load on a struggling
/// server with back-to-back requests.
fn poll_interval(secs: u64) -> tokio::time::Interval {
    let mut t = interval(Duration::from_secs(secs));
    t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    t
}

/// Body for POST /api/alerts.
#[derive(serde::Serialize)]
struct CreateAlertBody {
    symbol: String,
    condition: String,
    price: f64,
    note: String,
}

/// Response from POST /api/alerts (wraps the created alert).
#[derive(serde::Deserialize, Default)]
struct CreateAlertResponse {
    #[serde(default)]
    alert: crate::types::alerts::AlertFull,
}

/// Input events forwarded from the crossterm read thread.
enum TuiInput {
    Key(KeyEvent),
    Mouse(MouseEvent),
}

// ─── persistence ────────────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct TuiState {
    #[serde(default)]
    selected_asset: String,
    #[serde(default)]
    last_quit: String,
    #[serde(default)]
    view: String,
    #[serde(default)]
    tf: String,
}

fn tui_state_path() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("cc", "carrac", "carrac")?;
    Some(dirs.cache_dir().join("tui-state.json"))
}

fn load_tui_state() -> TuiState {
    let path = match tui_state_path() {
        Some(p) => p,
        None => return TuiState::default(),
    };
    let body = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return TuiState::default(),
    };
    serde_json::from_str(&body).unwrap_or_default()
}

fn save_tui_state(app: &App) {
    let path = match tui_state_path() {
        Some(p) => p,
        None => return,
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let state = TuiState {
        selected_asset: app.selected_asset.clone(),
        last_quit: Utc::now().to_rfc3339(),
        view: app.view.label().to_string(),
        tf: app.chart.tf.clone(),
    };
    if let Ok(json) = serde_json::to_string(&state) {
        let _ = std::fs::write(&path, json);
    }
}

fn view_from_label(s: &str) -> Option<View> {
    match s {
        "chart" => Some(View::Chart),
        "pos" | "positions" => Some(View::Positions),
        "plays" => Some(View::Plays),
        "alerts" => Some(View::Alerts),
        "brief" => Some(View::Report), // legacy saved-state alias for the report tab
        "report" => Some(View::Report),
        // "ai"/"ask" was the old AI cockpit tab; now removed — fall back to chart.
        "news" => Some(View::News),
        "details" => Some(View::Details),
        _ => None,
    }
}

// ─── background fetchers ────────────────────────────────────────────────────

/// Message for the chart fetcher: either a full new spec (asset/TF switch) or
/// "refetch whatever you are currently showing" (manual refresh). The refresh
/// path must NOT carry a spec — it runs in a task that only has the startup
/// spec frozen at spawn time, and sending that would overwrite the chart with
/// the original symbol/TF after the user switched away.
enum ChartTrigger {
    Spec(ChartFetchSpec),
    RefreshCurrent,
}

struct FetcherHandles {
    refresh_tx: mpsc::UnboundedSender<()>,
    chart_trigger_tx: mpsc::UnboundedSender<ChartTrigger>,
    /// Set to true when the user first visits the report view. Report fetchers
    /// sleep until this fires, then run on their own interval thereafter.
    report_visited_flag: Arc<AtomicBool>,
    report_notify: Arc<Notify>,
    /// Wakes the sentiment-strip fetcher on the first report visit. Kept as a
    /// SEPARATE Notify from `report_notify`: `Notify::notify_one` wakes exactly
    /// one waiter, so two tasks parked on one Notify would race for a single
    /// wakeup and the loser would sleep forever.
    sentiment_notify: Arc<Notify>,
    /// Set to true when the user first visits the news view. News fetcher
    /// sleeps until this fires, then runs on its own interval thereafter.
    news_visited_flag: Arc<AtomicBool>,
    news_notify: Arc<Notify>,
    /// One-shot trigger for immediate news refetch (e.g. preset change).
    news_trigger_tx: mpsc::UnboundedSender<NewsFeedPreset>,
    /// Trigger a server symbol-lookup search: send a query string; the
    /// background task fetches `/api/symbol-lookup?q=<query>&limit=15` and
    /// pushes `Update::SymbolSearch(results)` back via the Update channel.
    pub symbol_search_trigger_tx: mpsc::UnboundedSender<String>,
    /// Set to true when the user first visits the details view. Details fetcher
    /// sleeps until this fires, then runs on its own interval thereafter.
    details_visited_flag: Arc<AtomicBool>,
    details_notify: Arc<Notify>,
    /// Fetch the 1D mini-chart for a Details row (side panel). Debounced in
    /// the task; the trigger site skips symbols already cached.
    details_chart_tx: mpsc::UnboundedSender<String>,
}

impl FetcherHandles {
    /// Signal that the report view was visited. Idempotent.
    pub fn notify_report_visited(&self) {
        self.report_visited_flag.store(true, Ordering::Relaxed);
        self.report_notify.notify_one();
        self.sentiment_notify.notify_one();
    }

    /// Signal that the news view was visited. Idempotent.
    pub fn notify_news_visited(&self) {
        self.news_visited_flag.store(true, Ordering::Relaxed);
        self.news_notify.notify_one();
    }

    /// Trigger an immediate news refetch with a new preset (e.g. `[`/`]` key).
    pub fn trigger_news_refresh(&self, preset: NewsFeedPreset) {
        let _ = self.news_trigger_tx.send(preset);
    }

    /// Kick a server symbol-lookup search.  Fires on Enter in the asset
    /// selector when a query is typed.  Rate-limit is respected server-side
    /// (20/min); we fire only on explicit submit so no client-side throttle
    /// is needed.
    pub fn trigger_symbol_search(&self, query: String) {
        let _ = self.symbol_search_trigger_tx.send(query);
    }

    /// Signal that the details view was visited. Idempotent.
    pub fn notify_details_visited(&self) {
        self.details_visited_flag.store(true, Ordering::Relaxed);
        self.details_notify.notify_one();
    }

    /// Kick a 1D mini-chart fetch for the Details side panel.
    pub fn trigger_details_chart(&self, symbol: String) {
        let _ = self.details_chart_tx.send(symbol);
    }
}

fn spawn_fetchers(
    provider: Provider,
    tx: mpsc::UnboundedSender<Update>,
    initial_chart_spec: ChartFetchSpec,
) -> FetcherHandles {
    let (refresh_tx, mut refresh_rx) = mpsc::unbounded_channel::<()>();
    let (chart_trigger_tx, mut chart_trigger_rx) = mpsc::unbounded_channel::<ChartTrigger>();
    let report_visited_flag = Arc::new(AtomicBool::new(false));
    let report_notify = Arc::new(Notify::new());
    let sentiment_notify = Arc::new(Notify::new());
    let news_visited_flag = Arc::new(AtomicBool::new(false));
    let news_notify = Arc::new(Notify::new());
    let (news_trigger_tx, mut news_trigger_rx) = mpsc::unbounded_channel::<NewsFeedPreset>();
    let (details_chart_tx, mut details_chart_rx) = mpsc::unbounded_channel::<String>();
    let (symbol_search_trigger_tx, mut symbol_search_rx) = mpsc::unbounded_channel::<String>();
    let details_visited_flag = Arc::new(AtomicBool::new(false));
    let details_notify = Arc::new(Notify::new());

    // ── Chart fetcher — runs in BOTH modes (Carrac AND the keyless Binance
    // demo). Carrac-only fetchers (positions / plays / alerts / watchlist /
    // sentiment / report / news / details / symbol-search / details-chart) are
    // gated below behind the `Provider::Carrac` arm so the demo never error-
    // loops against api.binance.com or a missing base_url.
    //
    // 30 s ticker + 150 ms debounced trigger.
    //
    // Why debounce: rapid `,`/`.` favourite-cycle keypresses each return
    // `Action::RefreshChart`, which used to fire a network request per press.
    // Even though we drained the channel, every drained-then-resumed fetch
    // would `await` ~300–700 ms of network. With the debounce we sleep
    // briefly after the first trigger, drain everything that lands during
    // the sleep, then fire exactly one fetch with the final spec. A burst
    // of 8 presses produces 1 fetch instead of 8 sequential ones.
    {
        let provider = provider.clone();
        let tx = tx.clone();
        let initial = initial_chart_spec.clone();
        tokio::task::spawn(async move {
            let mut current = initial;
            let mut ticker = poll_interval(30);
            // Eat the immediate first tick — initial fetch is done below.
            ticker.tick().await;
            do_chart_fetch(&provider, &tx, &current).await;
            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        do_chart_fetch(&provider, &tx, &current).await;
                    }
                    Some(trigger) = chart_trigger_rx.recv() => {
                        if let ChartTrigger::Spec(spec) = trigger {
                            current = spec;
                        }
                        // Debounce: wait briefly to see if more triggers
                        // arrive (rapid favourite cycle, fast TF flips).
                        tokio::time::sleep(Duration::from_millis(150)).await;
                        while let Ok(later) = chart_trigger_rx.try_recv() {
                            if let ChartTrigger::Spec(spec) = later {
                                current = spec;
                            }
                        }
                        do_chart_fetch(&provider, &tx, &current).await;
                    }
                }
            }
        });
    }

    // ── Carrac-only fetchers ─────────────────────────────────────────────────
    // Skipped entirely in the keyless Binance demo. The `FetcherHandles`
    // channels still exist so the event loop's sends are harmless no-ops.
    if let Provider::Carrac(client) = &provider {
        let client = client.clone();

        // Positions — 15 s
        {
            let client = client.clone();
            let tx = tx.clone();
            tokio::task::spawn(async move {
                let mut ticker = poll_interval(30);
                loop {
                    ticker.tick().await;
                    match crate::commands::positions::fetch(&client, None).await {
                        Ok(resp) => {
                            let _ = tx.send(Update::Positions(resp));
                        }
                        Err(e) => {
                            let _ = tx.send(Update::FetchError {
                                source: "positions",
                                msg: format!("{e:#}"),
                            });
                        }
                    }
                }
            });
        }

        // Alerts — 30 s
        {
            let client = client.clone();
            let tx = tx.clone();
            tokio::task::spawn(async move {
                let mut ticker = poll_interval(30);
                loop {
                    ticker.tick().await;
                    match client
                        .get::<crate::types::alerts::AlertsResponse>("/api/alerts/triggered")
                        .await
                    {
                        Ok(resp) => {
                            let _ = tx.send(Update::Alerts(resp));
                        }
                        Err(e) => {
                            let _ = tx.send(Update::FetchError {
                                source: "alerts",
                                msg: format!("{e:#}"),
                            });
                        }
                    }
                }
            });
        }

        // Plays — 60 s
        {
            let client = client.clone();
            let tx = tx.clone();
            tokio::task::spawn(async move {
                let mut ticker = poll_interval(60);
                loop {
                    ticker.tick().await;
                    match crate::commands::plays::fetch_active(&client).await {
                        Ok(resp) => {
                            let _ = tx.send(Update::Plays(resp));
                        }
                        Err(e) => {
                            let _ = tx.send(Update::FetchError {
                                source: "plays",
                                msg: format!("{e:#}"),
                            });
                        }
                    }
                }
            });
        }

        // Watchlist — 60 s
        {
            let client = client.clone();
            let tx = tx.clone();
            tokio::task::spawn(async move {
                let mut ticker = poll_interval(60);
                loop {
                    ticker.tick().await;
                    let symbols = crate::helpers::watchlist::load();
                    match crate::helpers::batch_fetch::fetch_batch(
                        &client,
                        &symbols,
                        &["scanner", "market"],
                    )
                    .await
                    {
                        Ok(resp) => {
                            let _ = tx.send(Update::Watchlist(resp));
                        }
                        Err(_e) => { /* silent — watchlist is informational */ }
                    }
                }
            });
        }

        // Manual-refresh listener — when refresh_tx fires, re-poll all sources.
        {
            let client = client.clone();
            let tx2 = tx.clone();
            let chart_trigger_for_refresh = chart_trigger_tx.clone();
            let news_trigger_for_refresh = news_trigger_tx.clone();
            let report_flag_for_refresh = Arc::clone(&report_visited_flag);
            let news_flag_for_refresh = Arc::clone(&news_visited_flag);
            let details_flag_for_refresh = Arc::clone(&details_visited_flag);
            tokio::task::spawn(async move {
                while refresh_rx.recv().await.is_some() {
                    match crate::commands::positions::fetch(&client, None).await {
                        Ok(r) => {
                            let _ = tx2.send(Update::Positions(r));
                        }
                        Err(e) => {
                            let _ = tx2.send(Update::FetchError {
                                source: "positions",
                                msg: format!("{e:#}"),
                            });
                        }
                    }
                    match client
                        .get::<crate::types::alerts::AlertsResponse>("/api/alerts/triggered")
                        .await
                    {
                        Ok(r) => {
                            let _ = tx2.send(Update::Alerts(r));
                        }
                        Err(e) => {
                            let _ = tx2.send(Update::FetchError {
                                source: "alerts",
                                msg: format!("{e:#}"),
                            });
                        }
                    }
                    match crate::commands::plays::fetch_active(&client).await {
                        Ok(r) => {
                            let _ = tx2.send(Update::Plays(r));
                        }
                        Err(e) => {
                            let _ = tx2.send(Update::FetchError {
                                source: "plays",
                                msg: format!("{e:#}"),
                            });
                        }
                    }
                    {
                        let symbols = match crate::helpers::watchlist::fetch_remote(&client).await {
                            Some(remote) => {
                                crate::helpers::watchlist::save_cache(&remote);
                                let _ = tx2.send(Update::WatchlistSymbols(remote.clone()));
                                remote
                            }
                            None => crate::helpers::watchlist::load(),
                        };
                        if let Ok(r) = crate::helpers::batch_fetch::fetch_batch(
                            &client,
                            &symbols,
                            &["scanner", "market"],
                        )
                        .await
                        {
                            let _ = tx2.send(Update::Watchlist(r));
                        }
                    }
                    // Nudge the sentiment strip only if the report view was visited.
                    if report_flag_for_refresh.load(Ordering::Relaxed) {
                        let _ = tx2.send(Update::Sentiment(
                            client
                                .get::<crate::types::sentiment::SentimentResponse>(
                                    "/api/market-sentiment",
                                )
                                .await,
                        ));
                    }
                    // Nudge report sources only if the user has visited report view.
                    if report_flag_for_refresh.load(Ordering::Relaxed) {
                        let _ = tx2.send(Update::Report(
                            client
                                .get::<crate::types::report::ReportResponse>(
                                    "/api/report/morning/latest",
                                )
                                .await,
                        ));
                        let _ = tx2.send(Update::ReportAfternoon(
                            client
                                .get::<crate::types::report::ReportResponse>(
                                    "/api/report/afternoon/latest",
                                )
                                .await,
                        ));
                    }
                    // Nudge news fetcher only if the user has visited news view.
                    // Use Default preset as a nudge — the fetcher will use the
                    // current preset stored in the trigger channel or fall back.
                    if news_flag_for_refresh.load(Ordering::Relaxed) {
                        let _ = news_trigger_for_refresh.send(NewsFeedPreset::Default);
                    }
                    // Nudge details only if the user has visited details view.
                    if details_flag_for_refresh.load(Ordering::Relaxed) {
                        let _ = tx2.send(Update::DetailsMatrix(fetch_matrix(&client).await));
                    }
                    // Nudge the chart fetcher to refetch whatever it is
                    // currently showing. Never send a spec from here — this task
                    // only has the startup spec, which would clobber the chart
                    // after the user switched asset/TF.
                    let _ = chart_trigger_for_refresh.send(ChartTrigger::RefreshCurrent);
                }
            });
        }

        // Cold-start: fire positions/alerts/plays/watchlist immediately. Chart
        // already has its initial fetch baked into the dedicated task above.
        {
            let client = client.clone();
            let tx = tx.clone();
            tokio::task::spawn(async move {
                // One-way watchlist sync: pull the user's server-side list first so
                // the grid shows THEIR symbols, cache it locally (offline + the
                // background pollers re-read the file), and push it into app state.
                // Best-effort — falls back to the local cache when unreachable.
                let symbols = match crate::helpers::watchlist::fetch_remote(&client).await {
                    Some(remote) => {
                        crate::helpers::watchlist::save_cache(&remote);
                        let _ = tx.send(Update::WatchlistSymbols(remote.clone()));
                        remote
                    }
                    None => crate::helpers::watchlist::load(),
                };
                let (pos_res, alerts_res, plays_res, watchlist_res) = tokio::join!(
                    crate::commands::positions::fetch(&client, None),
                    client.get::<crate::types::alerts::AlertsResponse>("/api/alerts/triggered"),
                    crate::commands::plays::fetch_active(&client),
                    crate::helpers::batch_fetch::fetch_batch(
                        &client,
                        &symbols,
                        &["scanner", "market"]
                    ),
                );
                match pos_res {
                    Ok(r) => {
                        let _ = tx.send(Update::Positions(r));
                    }
                    Err(e) => {
                        let _ = tx.send(Update::FetchError {
                            source: "positions",
                            msg: format!("{e:#}"),
                        });
                    }
                }
                match alerts_res {
                    Ok(r) => {
                        let _ = tx.send(Update::Alerts(r));
                    }
                    Err(e) => {
                        let _ = tx.send(Update::FetchError {
                            source: "alerts",
                            msg: format!("{e:#}"),
                        });
                    }
                }
                match plays_res {
                    Ok(r) => {
                        let _ = tx.send(Update::Plays(r));
                    }
                    Err(e) => {
                        let _ = tx.send(Update::FetchError {
                            source: "plays",
                            msg: format!("{e:#}"),
                        });
                    }
                }
                if let Ok(r) = watchlist_res {
                    let _ = tx.send(Update::Watchlist(r));
                }
            });
        }

        // Sentiment — feeds the always-refreshing strip on top of the Report view.
        // Gated on the first report visit via its OWN notify (sharing
        // `report_notify` would steal the report fetcher's single wakeup),
        // then refreshes every 60 s.
        {
            let client = client.clone();
            let tx = tx.clone();
            let notify = Arc::clone(&sentiment_notify);
            tokio::task::spawn(async move {
                notify.notified().await;
                let _ = tx.send(Update::Sentiment(
                    client
                        .get::<crate::types::sentiment::SentimentResponse>("/api/market-sentiment")
                        .await,
                ));
                let mut sentiment_ticker = poll_interval(60);
                sentiment_ticker.tick().await; // eat the immediate tick
                loop {
                    sentiment_ticker.tick().await;
                    let _ = tx.send(Update::Sentiment(
                        client
                            .get::<crate::types::sentiment::SentimentResponse>(
                                "/api/market-sentiment",
                            )
                            .await,
                    ));
                }
            });
        }

        // Report — morning + afternoon, 5 min interval.
        //
        // Sleeps until `report_notify` fires (i.e. the user presses `6` for the
        // first time). Full content (no `?section=summary`).
        {
            let client = client.clone();
            let tx = tx.clone();
            let notify = Arc::clone(&report_notify);
            let flag = Arc::clone(&report_visited_flag);
            tokio::task::spawn(async move {
                // Wait for first report visit.
                notify.notified().await;
                // Immediate fetch on arrival.
                let _ = tx.send(Update::Report(
                    client
                        .get::<crate::types::report::ReportResponse>("/api/report/morning/latest")
                        .await,
                ));
                let _ = tx.send(Update::ReportAfternoon(
                    client
                        .get::<crate::types::report::ReportResponse>("/api/report/afternoon/latest")
                        .await,
                ));
                // Periodic refresh.
                let mut report_ticker = poll_interval(300);
                // Eat the immediate first tick — already fetched above.
                report_ticker.tick().await;
                loop {
                    if !flag.load(Ordering::Relaxed) {
                        break;
                    }
                    report_ticker.tick().await;
                    let _ = tx.send(Update::Report(
                        client
                            .get::<crate::types::report::ReportResponse>(
                                "/api/report/morning/latest",
                            )
                            .await,
                    ));
                    let _ = tx.send(Update::ReportAfternoon(
                        client
                            .get::<crate::types::report::ReportResponse>(
                                "/api/report/afternoon/latest",
                            )
                            .await,
                    ));
                }
            });
        }

        // News — 5-minute interval (matches server-side cache).
        {
            let client = client.clone();
            let tx = tx.clone();
            let notify = Arc::clone(&news_notify);
            let flag = Arc::clone(&news_visited_flag);
            tokio::task::spawn(async move {
                notify.notified().await;

                let mut current_preset = NewsFeedPreset::Default;
                do_news_fetch(&client, &tx, current_preset).await;

                let mut news_ticker = poll_interval(300);
                news_ticker.tick().await;

                loop {
                    if !flag.load(Ordering::Relaxed) {
                        break;
                    }
                    tokio::select! {
                        _ = news_ticker.tick() => {
                            do_news_fetch(&client, &tx, current_preset).await;
                        }
                        Some(preset) = news_trigger_rx.recv() => {
                            current_preset = preset;
                            while let Ok(p) = news_trigger_rx.try_recv() {
                                current_preset = p;
                            }
                            do_news_fetch(&client, &tx, current_preset).await;
                            news_ticker = poll_interval(300);
                            news_ticker.tick().await;
                        }
                    }
                }
            });
        }

        // Details — ONE call to /api/details/matrix returns the server's typed
        // indicator columns for the caller's watchlist (server-filtered + computed),
        // lazy on first visit then on a 30 s interval.
        {
            let client = client.clone();
            let tx = tx.clone();
            let notify = Arc::clone(&details_notify);
            let flag = Arc::clone(&details_visited_flag);
            tokio::task::spawn(async move {
                notify.notified().await;
                let _ = tx.send(Update::DetailsMatrix(fetch_matrix(&client).await));

                let mut details_ticker = poll_interval(30);
                details_ticker.tick().await;
                loop {
                    if !flag.load(Ordering::Relaxed) {
                        break;
                    }
                    details_ticker.tick().await;
                    let _ = tx.send(Update::DetailsMatrix(fetch_matrix(&client).await));
                }
            });
        }

        // Periodic watchlist re-poll — every 300 s, silently re-sync the server
        // watchlist so symbols added on the web eventually appear without restart.
        // Best-effort: any error is dropped (same fallback as cold-start).
        {
            let client = client.clone();
            let tx = tx.clone();
            tokio::task::spawn(async move {
                let mut ticker = poll_interval(300);
                // Eat the immediate tick — cold-start already fetched once.
                ticker.tick().await;
                loop {
                    ticker.tick().await;
                    if let Some(remote) = crate::helpers::watchlist::fetch_remote(&client).await {
                        crate::helpers::watchlist::save_cache(&remote);
                        let _ = tx.send(Update::WatchlistSymbols(remote));
                    }
                }
            });
        }

        // Symbol search — background task that handles on-demand queries from the
        // asset selector overlay.  Receives a query string via `symbol_search_rx`
        // and fetches `/api/symbol-lookup?q=<query>&limit=15`.  Results are pushed
        // as `Update::SymbolSearch` into the main Update channel.  Best-effort /
        // silent on error (the overlay simply shows no search section on failure).
        {
            let client = client.clone();
            let tx = tx.clone();
            tokio::task::spawn(async move {
                while let Some(query) = symbol_search_rx.recv().await {
                    // Drain any backlog — if the user typed + submitted multiple
                    // times rapidly, only execute the latest query.
                    let mut latest = query;
                    while let Ok(q) = symbol_search_rx.try_recv() {
                        latest = q;
                    }
                    if latest.trim().is_empty() {
                        continue;
                    }
                    let trimmed = latest.trim();
                    let mut encoded = String::with_capacity(trimmed.len());
                    for c in trimmed.chars() {
                        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                            encoded.push(c);
                        } else if c == ' ' {
                            encoded.push('+');
                        } else {
                            // Percent-encode the UTF-8 bytes. NOT `c as u32` —
                            // that emits invalid sequences ("%20AC" for '€')
                            // instead of the byte pairs ("%E2%82%AC").
                            let mut buf = [0u8; 4];
                            for b in c.encode_utf8(&mut buf).bytes() {
                                use std::fmt::Write;
                                let _ = write!(encoded, "%{b:02X}");
                            }
                        }
                    }
                    let path = format!("/api/symbol-lookup?q={encoded}&limit=15");
                    if let Ok(hits) = client
                        .get::<Vec<crate::types::search::SymbolHit>>(&path)
                        .await
                    {
                        let _ = tx.send(Update::SymbolSearch(hits));
                    }
                }
            });
        }

        // Details mini-chart fetcher — debounce rapid j/k, fetch the latest only.
        {
            let client = client.clone();
            let tx = tx.clone();
            tokio::task::spawn(async move {
                while let Some(sym) = details_chart_rx.recv().await {
                    let mut latest = sym;
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    while let Ok(later) = details_chart_rx.try_recv() {
                        latest = later;
                    }
                    let path = format!("/api/chart/{latest}?timeframe=1D&days=90");
                    if let Ok(data) = client.get::<ChartResponse>(&path).await {
                        let _ = tx.send(Update::DetailsChart {
                            symbol: latest,
                            data,
                        });
                    }
                }
            });
        }
    } // end Carrac-only fetchers

    FetcherHandles {
        refresh_tx,
        chart_trigger_tx,
        report_visited_flag,
        report_notify,
        sentiment_notify,
        news_visited_flag,
        news_notify,
        news_trigger_tx,
        symbol_search_trigger_tx,
        details_visited_flag,
        details_notify,
        details_chart_tx,
    }
}

/// Fetch the screening matrix for the caller's watchlist in one call:
/// `/api/details/matrix?tab=watchlist`. The server resolves the watchlist from
/// the API key, filters to it, and computes every indicator column — so the
/// Details grid matches the web's Screening Matrix by construction.
async fn fetch_matrix(client: &Client) -> anyhow::Result<crate::types::matrix::MatrixResponse> {
    client
        .get::<crate::types::matrix::MatrixResponse>("/api/details/matrix?tab=watchlist")
        .await
}

async fn do_chart_fetch(
    provider: &Provider,
    tx: &mpsc::UnboundedSender<Update>,
    spec: &ChartFetchSpec,
) {
    let days = fetch_days_for_tf(&spec.tf);
    match provider.fetch_chart(&spec.symbol, &spec.tf, days).await {
        Ok(mut resp) => {
            // CPR S/R supplement: Carrac-only, no-op in demo.
            provider.enrich(&spec.symbol, &mut resp).await;
            let _ = tx.send(Update::Chart(resp));
        }
        Err(e) => {
            let _ = tx.send(Update::FetchError {
                source: "chart",
                msg: format!("{e:#}"),
            });
        }
    }
}

async fn do_news_fetch(
    client: &Client,
    tx: &mpsc::UnboundedSender<Update>,
    preset: NewsFeedPreset,
) {
    let path = format!("/api/market-news?feeds={}", preset.to_feed_ids());
    let _ = tx.send(Update::News(
        client.get::<crate::types::news::NewsResponse>(&path).await,
    ));
}

// ─── PNG popup ───────────────────────────────────────────────────────────────

/// Return the cache directory for PNG chart files.
/// `~/.cache/carrac/png/` (via `directories` `ProjectDirs`, same base as tui-state).
pub(crate) fn png_cache_dir() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("cc", "carrac", "carrac")?;
    Some(dirs.cache_dir().join("png"))
}

/// Build the full path for a cached PNG file:
/// `~/.cache/carrac/png/<ASSET>-<TF>-<unix_timestamp>.png`
pub(crate) fn png_cache_path(asset: &str, tf: &str) -> Option<PathBuf> {
    let dir = png_cache_dir()?;
    let ts = Utc::now().timestamp();
    Some(dir.join(format!("{asset}-{tf}-{ts}.png")))
}

/// Remove PNG cache files older than 1 day. Silently ignores errors.
fn clean_old_png_cache() {
    let Some(dir) = png_cache_dir() else { return };
    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(24 * 60 * 60))
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata()
                && meta.is_file()
                && let Ok(modified) = meta.modified()
                && modified < cutoff
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Fetch (try server screenshot first, fall back to client-side plotters render)
/// and save PNG to the given path. Returns the path on success.
/// Render an already-fetched `ChartResponse` (+ optional drawings) to a PNG file,
/// fully client-side — NO network and NO server screenshot. Used by the agent
/// hand-off, which renders from the cockpit's in-memory data so the launch is fast.
fn render_chart_response_to_png(
    data: &crate::commands::chart::ChartResponse,
    drawings: &[crate::chart::render::Drawing],
    show_overlays: bool,
    out_path: &std::path::Path,
) -> anyhow::Result<()> {
    use crate::chart::render;
    if data.ohlcv.is_empty() {
        return Err(anyhow::anyhow!("png: no candle data"));
    }
    let ema_opt: Option<render::EmaRibbon> = data.indicators.as_ref().and_then(|ind| {
        ind.ema_ribbon.as_ref().map(|r| render::EmaRibbon {
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
            slow: vec![],
        })
    });
    let term_cols = crossterm::terminal::size().map_or(120, |(c, _)| c);
    let img_w = (u32::from(term_cols) * 8).clamp(600, 2000);
    let img_h = 480u32;
    let mut prims = crate::chart::primitives::drawings_to_primitives(drawings);
    if show_overlays {
        prims.extend(crate::chart::primitives::overlays_to_primitives(data));
    }
    let png_bytes = render::render_png_rich(
        &data.ohlcv,
        data.cpr.as_ref(),
        ema_opt.as_ref(),
        true,
        false,
        &prims,
        img_w,
        img_h,
    )
    .map_err(|e| anyhow::anyhow!("png: render failed: {e}"))?;
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("png: failed to create cache dir: {e}"))?;
    }
    crate::chart::kgp::write_png_file(&png_bytes, &out_path.to_string_lossy())
        .map_err(|e| anyhow::anyhow!("png: failed to write PNG file: {e}"))?;
    Ok(())
}

/// Fetch an asset's chart + drawings and render a client-side PNG into `out_path`.
/// Used for the agent hand-off on asset-focused views where the asset isn't
/// already in `app.chart_data` (e.g. the Details grid). Skips the slow server
/// screenshot for a snappy hand-off. Returns true on success.
async fn render_asset_to_png(
    client: &Client,
    asset: &str,
    tf: &str,
    show_overlays: bool,
    out_path: &std::path::Path,
) -> bool {
    use crate::commands::chart::fetch_days_for_tf;
    let days = fetch_days_for_tf(tf);
    let chart_path = format!("/api/chart/{asset}?timeframe={tf}&days={days}");
    let Ok(data) = client
        .get::<crate::commands::chart::ChartResponse>(&chart_path)
        .await
    else {
        return false;
    };
    let dpath = format!("/api/drawings?asset={asset}&timeframe={tf}");
    let drawings = client
        .get::<crate::chart::render::DrawingsResponse>(&dpath)
        .await
        .map(|r| r.drawings)
        .unwrap_or_default();
    render_chart_response_to_png(&data, &drawings, show_overlays, out_path).is_ok()
}

async fn fetch_png_to_file(
    client: &Client,
    asset: &str,
    tf: &str,
    out_path: &std::path::Path,
    show_overlays: bool,
) -> anyhow::Result<()> {
    use crate::chart::render;
    use crate::commands::chart::fetch_days_for_tf;

    // 1. Try server-side screenshot endpoint first.
    let server_ok = try_server_screenshot(client, asset, tf, out_path).await;
    if server_ok {
        return Ok(());
    }

    // 2. Fall back to client-side plotters render.
    let days = fetch_days_for_tf(tf);
    let chart_path = format!("/api/chart/{asset}?timeframe={tf}&days={days}");
    let mut data: crate::commands::chart::ChartResponse = client
        .get(&chart_path)
        .await
        .map_err(|e| anyhow::anyhow!("png: chart fetch failed: {e}"))?;

    if data.ohlcv.is_empty() {
        return Err(anyhow::anyhow!("png: no candle data for {asset} {tf}"));
    }

    // Fetch drawings (best-effort).
    let drawings_path = format!("/api/drawings?asset={asset}&timeframe={tf}");
    let drawings: Vec<render::Drawing> =
        match client.get::<render::DrawingsResponse>(&drawings_path).await {
            Ok(resp) => resp.drawings,
            Err(_) => vec![],
        };

    // Enrich CprData with S1-S3 / R1-R3 from prior closed monthly bar.
    // Best-effort: on failure the chart still renders with P/TC/BC only.
    if data.cpr.is_some() {
        crate::commands::chart::enrich_cpr_with_sr_levels(client, asset, &mut data).await;
    }

    // Bridge EMA ribbon.
    let ema_opt: Option<render::EmaRibbon> = data.indicators.as_ref().and_then(|ind| {
        ind.ema_ribbon.as_ref().map(|r| render::EmaRibbon {
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
            slow: vec![],
        })
    });

    let term_cols = crossterm::terminal::size().map_or(120, |(c, _)| c);
    let img_w = (u32::from(term_cols) * 8).clamp(600, 2000);
    let img_h = 480u32;

    let mut prims = crate::chart::primitives::drawings_to_primitives(&drawings);
    if show_overlays {
        prims.extend(crate::chart::primitives::overlays_to_primitives(&data));
    }
    let png_bytes = render::render_png_rich(
        &data.ohlcv,
        data.cpr.as_ref(),
        ema_opt.as_ref(),
        true,  // overlay_cpr
        false, // overlay_ema
        &prims,
        img_w,
        img_h,
    )
    .map_err(|e| anyhow::anyhow!("png: render failed: {e}"))?;

    // Write to cache file.
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("png: failed to create cache dir: {e}"))?;
    }
    crate::chart::kgp::write_png_file(&png_bytes, &out_path.to_string_lossy())
        .map_err(|e| anyhow::anyhow!("png: failed to write PNG file: {e}"))?;

    Ok(())
}

/// Try fetching a PNG from the server screenshot endpoint.
/// Returns true if a valid PNG was written to `out_path`.
async fn try_server_screenshot(
    client: &Client,
    asset: &str,
    tf: &str,
    out_path: &std::path::Path,
) -> bool {
    let path = format!("/api/screenshot/{asset}/{tf}");
    match client.get_bytes(&path).await {
        Ok(bytes) if bytes.len() > 4 && bytes[0..4] == *b"\x89PNG" => {
            if let Some(parent) = out_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if std::fs::write(out_path, &bytes).is_ok() {
                return true;
            }
        }
        _ => {}
    }
    false
}

/// Probe whether `command` is available on PATH by attempting to spawn it
/// with a benign argument that most CLI tools handle without side effects.
/// Returns true if the OS found the executable.
fn command_on_path(command: &str) -> bool {
    // Pure PATH lookup — do NOT spawn the command. Spawning e.g. `claude --version`
    // pays the agent's full startup cost just to validate it exists, which made the
    // hand-off feel slow. A path containing a separator is checked directly.
    if command.contains('/') {
        return std::path::Path::new(command).is_file();
    }
    match std::env::var_os("PATH") {
        Some(paths) => std::env::split_paths(&paths).any(|dir| dir.join(command).is_file()),
        None => false,
    }
}

/// Write agent context bundle, then suspend the TUI and spawn the configured
/// agent command with its cwd set to the workspace directory.  Waits for the
/// agent to exit, then restores the TUI and triggers a data refresh.
///
/// On the chart view, renders a PNG first (best-effort) and places it in the
/// workspace so the agent sees the chart image.
async fn launch_agent(
    terminal: &mut tui::Tui,
    app: &mut App,
    client: &Client,
    agent: &crate::config::AgentConfig,
) -> anyhow::Result<()> {
    use crossterm::{
        event::DisableMouseCapture,
        event::EnableMouseCapture,
        execute,
        terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    };

    // ── Validate command ─────────────────────────────────────────────────────
    let cmd = agent.command.trim();
    if cmd.is_empty() || !command_on_path(cmd) {
        let config_path = crate::config::config_path_display();
        app.toast = Some(if cmd.is_empty() {
            format!("Set your agent in {config_path} → [agent] command = \"claude\"")
        } else {
            format!(
                "agent command '{cmd}' not found on PATH — fix [agent] command in {config_path}"
            )
        });
        app.toast_until = Some(std::time::Instant::now() + Duration::from_secs(5));
        return Ok(());
    }

    // ── Prepare workspace ─────────────────────────────────────────────────────
    let ws = if let Some(d) = agent_bundle::workspace_dir() {
        d
    } else {
        app.toast = Some("agent: could not determine workspace dir".to_string());
        app.toast_until = Some(std::time::Instant::now() + Duration::from_secs(4));
        return Ok(());
    };
    if let Err(e) = agent_bundle::prepare_workspace(&ws) {
        app.toast = Some(format!("agent: workspace error: {e}"));
        app.toast_until = Some(std::time::Instant::now() + Duration::from_secs(4));
        return Ok(());
    }

    // ── For asset-focused views: render a chart PNG into the workspace ────────
    // Chart view uses the IN-MEMORY data the cockpit already fetched (fast — no
    // /api/chart re-fetch); the Details grid fetches+renders the focused asset
    // (`selected_asset`, matching the CONTEXT.md line) since it isn't in
    // `app.chart_data`. Other views render nothing — and write_orientation then
    // omits the chart.png instruction so the agent doesn't hunt for a missing
    // file (the bug this fixes).
    let png_path_in_ws = ws.join("chart.png");
    let png_result: Option<&std::path::Path> = match app.view {
        View::Chart => match &app.chart_data {
            Some(data) => {
                let dpath = format!(
                    "/api/drawings?asset={}&timeframe={}",
                    app.selected_asset, app.chart.tf
                );
                let drawings = client
                    .get::<crate::chart::render::DrawingsResponse>(&dpath)
                    .await
                    .map(|r| r.drawings)
                    .unwrap_or_default();
                // Hand the agent the exact, machine-readable drawing levels (the
                // same payload baked into the PNG) so it can reason over precise
                // price levels — not just read them off the image.
                if !drawings.is_empty()
                    && let Ok(j) = serde_json::to_string_pretty(&drawings)
                {
                    let _ = std::fs::write(ws.join("drawings.json"), j);
                }
                render_chart_response_to_png(
                    data,
                    &drawings,
                    app.chart.show_overlays,
                    &png_path_in_ws,
                )
                .ok()
                .map(|()| png_path_in_ws.as_path())
            }
            None => None,
        },
        View::Details if !app.selected_asset.is_empty() => {
            if render_asset_to_png(
                client,
                &app.selected_asset,
                &app.chart.tf,
                app.chart.show_overlays,
                &png_path_in_ws,
            )
            .await
            {
                Some(png_path_in_ws.as_path())
            } else {
                None
            }
        }
        _ => None,
    };

    // ── Write context bundle ─────────────────────────────────────────────────
    if let Err(e) = agent_bundle::write_bundle(&ws, app, png_result) {
        app.toast = Some(format!("agent: bundle write failed: {e}"));
        app.toast_until = Some(std::time::Instant::now() + Duration::from_secs(4));
        return Ok(());
    }

    // ── Suspend TUI ──────────────────────────────────────────────────────────
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
    )?;
    terminal.show_cursor()?;

    // Print a welcome banner so the user is oriented before the agent's own UI.
    let viewing = if app.view == View::Chart {
        format!(
            "{} · {}  (chart.png + chart-context.json are in this folder)",
            app.selected_asset, app.chart.tf
        )
    } else {
        format!(
            "the {} view  (its data + agent context files are in this folder)",
            app.view.label()
        )
    };
    // Display only the command's basename — the configured value may be a full
    // filesystem path, and the banner must never echo user paths.
    let cmd_display = std::path::Path::new(cmd)
        .file_name()
        .map_or_else(|| cmd.to_string(), |s| s.to_string_lossy().into_owned());
    println!(
        "\n  ── Carrac.cc TUI ────────────────────────────────────────────\n  Launched your agent ({cmd_display}) from the Carrac terminal.\n  You're viewing: {viewing}\n  The agent can read those files and use its MCP tools + the web to dig deeper.\n  Press Ctrl-D (or /exit) to close the agent and return to the TUI.\n  ──────────────────────────────────────────────────────────────\n"
    );
    let _ = std::io::Write::flush(&mut std::io::stdout());

    // ── Spawn agent and wait ─────────────────────────────────────────────────
    let mut child = std::process::Command::new(cmd);
    child.args(&agent.args);
    child.current_dir(&ws);
    // stdin/stdout/stderr are inherited so the agent has a full TTY.
    let status = child.status();

    // ── Restore TUI ─────────────────────────────────────────────────────────
    enable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture,
    )?;
    terminal.clear()?;

    match status {
        Ok(s) if s.success() => {
            app.toast = Some("agent exited — welcome back".to_string());
        }
        Ok(s) => {
            let code = s.code().unwrap_or(-1);
            app.toast = Some(format!("agent exited (code {code})"));
        }
        Err(e) => {
            app.toast = Some(format!("agent spawn failed: {e}"));
        }
    }
    app.toast_until = Some(std::time::Instant::now() + Duration::from_secs(3));

    Ok(())
}

/// Suspend TUI, display PNG via kitten icat (or xdg-open fallback), then restore.
///
/// This function MUST be called from the main thread (owns the terminal).
/// It takes the terminal by mutable reference so it can leave/re-enter.
fn display_png_inline(terminal: &mut tui::Tui, png_path: &std::path::Path) -> anyhow::Result<()> {
    use crossterm::{
        event::DisableMouseCapture,
        event::EnableMouseCapture,
        execute,
        terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    };
    use std::io::Write as _;

    // ── 1. Suspend TUI ───────────────────────────────────────────────────────
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
    )?;
    terminal.show_cursor()?;

    // ── 2. Clear screen so icat has a clean canvas ───────────────────────────
    print!("\x1b[2J\x1b[H");
    let _ = std::io::stdout().flush();

    // ── 3. Decide display method ─────────────────────────────────────────────
    let kitty_available = std::process::Command::new("kitty")
        .arg("+kitten")
        .arg("icat")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());

    if kitty_available {
        // ── 3a. kitty icat — synchronous, inherits terminal ─────────────────
        let _ = std::process::Command::new("kitty")
            .args(["+kitten", "icat", "--align", "center", "--clear"])
            .arg(png_path)
            .status();

        // Print prompt and wait for keypress.
        print!("\n  (any key to return)");
        let _ = std::io::stdout().flush();

        // Re-enable raw mode briefly to capture a single keypress.
        enable_raw_mode()?;
        // Block until any key event arrives.
        loop {
            match crossterm::event::read() {
                Ok(crossterm::event::Event::Key(k))
                    if k.kind == crossterm::event::KeyEventKind::Press =>
                {
                    break;
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        disable_raw_mode()?;
    } else {
        // ── 3b. OS-default-viewer fallback — opens the image in a separate window ──
        crate::helpers::open::open_external(&png_path.to_string_lossy());

        println!(
            "  PNG saved: {}\n  (opened in your image viewer; press any key to return)",
            png_path.display()
        );
        let _ = std::io::stdout().flush();

        enable_raw_mode()?;
        loop {
            match crossterm::event::read() {
                Ok(crossterm::event::Event::Key(k))
                    if k.kind == crossterm::event::KeyEventKind::Press =>
                {
                    break;
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        disable_raw_mode()?;
    }

    // ── 4. Restore TUI ───────────────────────────────────────────────────────
    // Re-enter alternate screen + raw mode + mouse capture.
    enable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture,
    )?;
    // Force a full redraw to repaint the TUI on top of any icat artifacts.
    terminal.clear()?;

    Ok(())
}

// ─── draw ────────────────────────────────────────────────────────────────────

fn draw(terminal: &mut tui::Tui, app: &mut App) -> Result<()> {
    terminal.draw(|frame| {
        let area = frame.area();
        let layout = build_layout(area);

        widgets::menubar::render(frame, layout.menubar, app);
        widgets::asset_bar::render(frame, layout.asset_bar, app);

        // In the keyless demo only the Chart view is backed by data; every
        // other tab is a Carrac-only surface and shows the one-line notice.
        if app.demo && app.view != View::Chart {
            app.last_chart_rect = None;
            widgets::render_gated_notice(frame, layout.main);
        } else {
            match app.view {
                View::Chart => widgets::view_chart::render(frame, layout.main, app),
                View::Positions => {
                    app.last_chart_rect = None;
                    widgets::view_positions::render(frame, layout.main, app);
                }
                View::Plays => {
                    app.last_chart_rect = None;
                    widgets::view_plays::render(frame, layout.main, app);
                }
                View::Alerts => {
                    app.last_chart_rect = None;
                    widgets::view_alerts::render(frame, layout.main, app);
                }
                View::Report => {
                    app.last_chart_rect = None;
                    view_report_widget::render(frame, layout.main, app);
                }
                View::News => {
                    app.last_chart_rect = None;
                    widgets::view_news::render(frame, layout.main, app);
                }
                View::Details => {
                    app.last_chart_rect = None;
                    widgets::view_details::render(frame, layout.main, app);
                }
            }
        }

        // Overlays last so they sit on top.
        if app.detail_overlay.is_some() {
            widgets::overlay_detail::render(frame, area, app);
        }
        if app.asset_selector.is_some() {
            widgets::overlay_asset::render(frame, area, app);
        }
        if app.show_help {
            widgets::overlay_help::render(frame, area, app);
        }
        // Search prompt renders over everything (incl. detail overlay) when open.
        if app
            .search
            .as_ref()
            .is_some_and(super::super::app::state::SearchState::is_open)
        {
            widgets::overlay_search::render(frame, layout.main, app);
        }
        // Add-alert form renders on top of everything.
        if app.add_alert_form.is_some() {
            widgets::overlay_add_alert::render(frame, area, app);
        }
        // Toast: a centered boxed message over whatever view is open — alert
        // toasts amber, info confirmations green. (The old single dim line at
        // the bottom was easy to miss entirely.)
        if let Some(toast) = &app.toast {
            use crate::app::state::ToastKind;
            use ratatui::layout::Alignment;
            use ratatui::style::{Color, Modifier, Style};
            use ratatui::text::{Line, Span};
            use ratatui::widgets::{Block, Borders, Clear, Paragraph};
            let (color, prefix) = match app.toast_kind {
                ToastKind::Alert => (Color::Yellow, "🔔 "),
                ToastKind::Info => (Color::Green, "✓ "),
            };
            let msg = format!("{prefix}{toast}");
            // +6 not +4: the bell/check prefix renders double-width, and a
            // phantom wrap row makes the box one line too tall.
            let w =
                (msg.chars().count() as u16 + 6).clamp(24, area.width.saturating_sub(4).max(24));
            let style = Style::default().fg(color).add_modifier(Modifier::BOLD);
            let para = Paragraph::new(Line::from(Span::styled(msg, style)))
                .alignment(Alignment::Center)
                .wrap(ratatui::widgets::Wrap { trim: true })
                .block(Block::default().borders(Borders::ALL).border_style(style));
            let h = (para.line_count(w.saturating_sub(2)).min(3) as u16) + 2;
            let rect = ratatui::layout::Rect {
                x: area.x + area.width.saturating_sub(w) / 2,
                y: area.y + area.height.saturating_sub(h) / 2,
                width: w,
                height: h,
            };
            frame.render_widget(Clear, rect);
            frame.render_widget(para, rect);
        }
    })?;
    // Tick toast auto-dismiss.
    app.tick_toast();
    Ok(())
}

// ─── entry point ─────────────────────────────────────────────────────────────

pub async fn run() -> Result<()> {
    // Clean up PNG cache files older than 1 day on startup.
    clean_old_png_cache();

    let cfg = Config::load()?;
    let client = Client::new(&cfg)?;
    let provider = Provider::from_config(&cfg, client.clone());

    let saved = load_tui_state();
    let mut app = App::new();
    // Demo mode (keyless Binance): Carrac-only surfaces show a one-liner
    // pointing at carrac.cc/reg instead of erroring or spinning.
    app.demo = provider.is_demo();
    if app.demo {
        // No server watchlist in demo — seed `,`/`.` cycling + the asset
        // selector with a small hardcoded crypto set.
        app.watchlist_symbols = App::DEMO_WATCHLIST
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
    }
    // Pass agent config from file config to app state.
    app.agent_config = cfg.agent.clone();
    if !saved.selected_asset.is_empty() {
        app.selected_asset = saved.selected_asset;
    }
    if let Some(v) = view_from_label(&saved.view) {
        app.view = v;
    }
    if !saved.tf.is_empty() {
        app.chart.tf = saved.tf;
    }

    let mut terminal = tui::enter()?;
    draw(&mut terminal, &mut app)?;

    let (tx, mut rx) = mpsc::unbounded_channel::<Update>();
    let handles = spawn_fetchers(provider.clone(), tx.clone(), app.chart_spec());

    // If the persisted view is Report or News, fire the lazy fetchers
    // immediately so the view has data when it renders on startup.
    if app.view == View::Report {
        app.report_visited = true;
        handles.notify_report_visited();
    }
    if app.view == View::News {
        app.news_visited = true;
        handles.notify_news_visited();
    }
    if app.view == View::Details {
        app.details_visited = true;
        handles.notify_details_visited();
    }

    let mut render_tick = interval(Duration::from_millis(50));

    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<TuiInput>();
    // When the user launches an external agent (the `I` key), the agent takes
    // over the terminal. This flag tells the input thread to stop reading stdin
    // so it doesn't steal keystrokes from the child (otherwise every other key
    // goes to the TUI instead of the agent). Set around the launch_agent call.
    let agent_running = Arc::new(AtomicBool::new(false));
    let agent_running_input = agent_running.clone();
    std::thread::spawn(move || {
        loop {
            // While the agent owns the terminal, don't touch stdin at all.
            if agent_running_input.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(40));
                continue;
            }
            // Poll with a short timeout so we never sit blocked in read() across
            // the hand-off — lets us notice the flag within ~40ms.
            match crossterm::event::poll(Duration::from_millis(40)) {
                Ok(true) => match crossterm::event::read() {
                    Ok(Event::Key(k)) => {
                        if input_tx.send(TuiInput::Key(k)).is_err() {
                            break;
                        }
                    }
                    Ok(Event::Mouse(m)) => {
                        if input_tx.send(TuiInput::Mouse(m)).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {} // Resize, focus, paste — ignored
                    Err(_) => break,
                },
                Ok(false) => {} // timeout — loop and re-check the flag
                Err(_) => break,
            }
        }
    });

    loop {
        tokio::select! {
            biased;

            Some(input) = input_rx.recv() => {
                let action = match input {
                    TuiInput::Key(key) if key.kind == KeyEventKind::Press => {
                        handle_input(&mut app, key)
                    }
                    TuiInput::Key(_) => Action::Continue,
                    TuiInput::Mouse(m) => handle_mouse(&mut app, m),
                };
                // Details side panel: whenever the cursor sits on a row whose
                // 1D chart isn't cached yet, kick a (debounced) fetch. Skipped
                // in demo (Details is a gated Carrac-only surface).
                if !app.demo && app.view == View::Details {
                    let idx = app.details_cursor.selected().unwrap_or(0);
                    if let Some(sym) = app.watchlist_symbols.get(idx)
                        && !app.details_charts.contains_key(sym) {
                            handles.trigger_details_chart(sym.clone());
                        }
                }
                match action {
                    Action::ForceRefresh => {
                        // Acknowledge every press with the red countdown flash, but
                        // coalesce rapid presses into one actual refresh — each
                        // refresh re-polls every source (incl. the chunked watchlist
                        // batch), so spamming would otherwise storm the server.
                        let debounced = app
                            .manual_refresh_at
                            .is_some_and(|t| t.elapsed().as_millis() < 1000);
                        app.manual_refresh_at = Some(std::time::Instant::now());
                        if !debounced {
                            let _ = handles.refresh_tx.send(());
                        }
                    }
                    Action::RefreshChart => {
                        let _ = handles.chart_trigger_tx.send(ChartTrigger::Spec(app.chart_spec()));
                    }
                    Action::ReportVisited => {
                        if !app.report_visited {
                            app.report_visited = true;
                            handles.notify_report_visited();
                        }
                    }
                    Action::NewsVisited => {
                        if !app.news_visited {
                            app.news_visited = true;
                            handles.notify_news_visited();
                        }
                    }
                    Action::RefreshNews => {
                        handles.trigger_news_refresh(app.news_feed_preset);
                    }
                    Action::TriggerSymbolSearch(query) => {
                        // Server symbol-lookup is a Carrac-only surface; in the
                        // keyless demo show the notice instead of a dead query.
                        if app.demo {
                            app.toast = Some(widgets::GATED_DEMO_NOTICE.to_string());
                            app.toast_until = Some(std::time::Instant::now() + Duration::from_secs(4));
                        } else {
                            handles.trigger_symbol_search(query);
                        }
                    }
                    Action::DetailsVisited => {
                        if !app.details_visited {
                            app.details_visited = true;
                            handles.notify_details_visited();
                        }
                    }
                    Action::JumpToChart { symbol } => {
                        app.selected_asset = symbol;
                        app.view = View::Chart;
                        let _ = handles.chart_trigger_tx.send(ChartTrigger::Spec(app.chart_spec()));
                    }
                    Action::LaunchAgent { agent } => {
                        // Pause the input thread so the child agent gets SOLE
                        // ownership of stdin (otherwise keystrokes are split
                        // between the TUI and the agent). Cleared even on error.
                        agent_running.store(true, Ordering::Relaxed);
                        let r = launch_agent(&mut terminal, &mut app, &client, &agent).await;
                        agent_running.store(false, Ordering::Relaxed);
                        r?;
                        // Trigger a data refresh after returning from the agent.
                        let _ = handles.refresh_tx.send(());
                        draw(&mut terminal, &mut app)?;
                    }
                    Action::SubmitAlert { symbol, condition, price, note } => {
                        let tx2 = tx.clone();
                        let client2 = client.clone();
                        let refresh_tx2 = handles.refresh_tx.clone();
                        tokio::task::spawn(async move {
                            let body = CreateAlertBody { symbol, condition, price, note };
                            let result = client2
                                .post::<CreateAlertResponse, _>("/api/alerts", &body)
                                .await
                                .map(|r| r.alert);
                            // On success, also kick a refresh so the alerts list updates.
                            if result.is_ok() {
                                let _ = refresh_tx2.send(());
                            }
                            let _ = tx2.send(Update::AlertCreated(result));
                        });
                    }
                    Action::ShowChartPng { asset, tf } => {
                        // Block on the async fetch from within the tokio runtime,
                        // then do the synchronous TUI-suspend/icat/restore dance.
                        let png_path = png_cache_path(&asset, &tf);
                        match png_path {
                            None => {
                                app.toast = Some("png: could not determine cache path".to_string());
                                app.toast_until = Some(std::time::Instant::now() + Duration::from_secs(3));
                            }
                            Some(path) => {
                                // Fetch PNG (async — can be awaited here because we're inside
                                // the tokio select! arm, still within the async context).
                                match fetch_png_to_file(&client, &asset, &tf, &path, app.chart.show_overlays).await {
                                    Ok(()) => {
                                        // Display synchronously (suspends + restores TUI).
                                        if let Err(e) = display_png_inline(&mut terminal, &path) {
                                            // Restore might partially fail; log as toast.
                                            app.toast = Some(format!("png display error: {e}"));
                                            app.toast_until = Some(std::time::Instant::now() + Duration::from_secs(4));
                                        }
                                        // Force a redraw after return.
                                        draw(&mut terminal, &mut app)?;
                                    }
                                    Err(e) => {
                                        app.toast = Some(format!("png fetch failed: {e:#}"));
                                        app.toast_until = Some(std::time::Instant::now() + Duration::from_secs(4));
                                    }
                                }
                            }
                        }
                    }
                    Action::Continue => {}
                }
            }

            _ = render_tick.tick() => {
                draw(&mut terminal, &mut app)?;
            }

            Some(update) = rx.recv() => {
                app.apply(update);
            }

            _ = tokio::signal::ctrl_c() => {
                app.should_quit = true;
            }
        }

        if app.should_quit {
            break;
        }
    }

    tui::exit(terminal)?;
    save_tui_state(&app);

    Ok(())
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_cache_path_format() {
        // Verify the path has the expected shape: …/<ASSET>-<TF>-<unix_ts>.png
        let path = png_cache_path("BTC", "4H");
        assert!(
            path.is_some(),
            "png_cache_path should return Some when ProjectDirs resolves"
        );
        let path = path.unwrap();

        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("path should have a filename");

        // Must start with "<asset>-<tf>-"
        assert!(
            file_name.starts_with("BTC-4H-"),
            "filename should start with 'BTC-4H-', got: {file_name}"
        );
        // Must end with ".png"
        assert!(
            file_name.ends_with(".png"),
            "filename should end with '.png', got: {file_name}"
        );
        // The timestamp segment (between second '-' and '.png') must be numeric.
        let stem = file_name.trim_end_matches(".png");
        let ts_str = stem.trim_start_matches("BTC-4H-");
        assert!(
            ts_str.chars().all(|c| c.is_ascii_digit()),
            "timestamp segment should be all digits, got: {ts_str}"
        );

        // Parent directory should contain "png".
        let dir = path.parent().unwrap();
        assert!(
            dir.ends_with("png"),
            "cache dir should end in 'png', got: {}",
            dir.display()
        );
    }

    #[test]
    fn png_cache_path_different_symbols_produce_different_paths() {
        let btc = png_cache_path("BTC", "1D");
        let eth = png_cache_path("ETH", "1D");
        // Both must be Some.
        let (b, e) = (btc.unwrap(), eth.unwrap());
        let bname = b.file_name().unwrap().to_str().unwrap();
        let ename = e.file_name().unwrap().to_str().unwrap();
        assert!(bname.starts_with("BTC-1D-"));
        assert!(ename.starts_with("ETH-1D-"));
    }
}
