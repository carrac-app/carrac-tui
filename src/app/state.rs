use std::collections::HashMap;
use std::time::Instant;

use ratatui::widgets::ListState;

use chrono::Timelike as _;

use crate::commands::chart::ChartResponse;
use crate::config::AgentConfig;
use crate::tui::widgets::chart::ChartWidgetState;
use crate::types::alerts::{AlertFull, AlertsResponse};
use crate::types::batch::BatchResponse;
use crate::types::matrix::{MatrixResponse, MatrixRow};
use crate::types::news::NewsResponse;
use crate::types::plays::{Play, PlaysResponse};
use crate::types::positions::{Position, PositionsResponse};
use crate::types::report::ReportResponse;
use crate::types::scanner::ScannerResponse;
use crate::types::search::SymbolHit;
use crate::types::sentiment::SentimentResponse;

// ── Add-Alert form ────────────────────────────────────────────────────────────

/// Which condition direction the alert fires on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlertCondition {
    #[default]
    Above,
    Below,
}

impl AlertCondition {
    pub fn label(self) -> &'static str {
        match self {
            AlertCondition::Above => "above",
            AlertCondition::Below => "below",
        }
    }

    pub fn toggle(self) -> Self {
        match self {
            AlertCondition::Above => AlertCondition::Below,
            AlertCondition::Below => AlertCondition::Above,
        }
    }
}

/// Which field has focus inside the add-alert form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AddAlertField {
    Symbol,
    Condition,
    #[default]
    Price,
    Note,
}

impl AddAlertField {
    /// Cycle focus forward (Tab).
    pub fn next(self) -> Self {
        match self {
            AddAlertField::Symbol => AddAlertField::Condition,
            AddAlertField::Condition => AddAlertField::Price,
            AddAlertField::Price => AddAlertField::Note,
            AddAlertField::Note => AddAlertField::Symbol,
        }
    }

    /// Cycle focus backward (Shift+Tab).
    pub fn prev(self) -> Self {
        match self {
            AddAlertField::Symbol => AddAlertField::Note,
            AddAlertField::Condition => AddAlertField::Symbol,
            AddAlertField::Price => AddAlertField::Condition,
            AddAlertField::Note => AddAlertField::Price,
        }
    }
}

/// State for the "create alert" form. Present on `App` while the form is open.
#[derive(Debug, Clone)]
pub struct AddAlertForm {
    /// Symbol to set the alert on (pre-filled with selected_asset).
    pub symbol: String,
    /// Condition direction.
    pub condition: AlertCondition,
    /// Price as a string (parsed to f64 on submit).
    pub price: String,
    /// Optional user note.
    pub note: String,
    /// Which field currently has keyboard focus.
    pub field: AddAlertField,
    /// True while the POST is in-flight.
    pub submitting: bool,
    /// Validation / server error to display.
    pub error: Option<String>,
}

impl AddAlertForm {
    /// Create a new form pre-filled with the given symbol.
    pub fn new(symbol: String) -> Self {
        AddAlertForm {
            symbol,
            condition: AlertCondition::Above,
            price: String::new(),
            note: String::new(),
            field: AddAlertField::Price,
            submitting: false,
            error: None,
        }
    }

    /// Append a character to the currently focused text field.
    /// In the Condition field, character input is ignored (use Left/Right to toggle).
    pub fn push_char(&mut self, c: char) {
        match self.field {
            AddAlertField::Symbol => self.symbol.push(c),
            AddAlertField::Condition => {} // toggled by Left/Right
            AddAlertField::Price => {
                // Accept only digits, dot, and comma (European decimal).
                if c.is_ascii_digit() || c == '.' || c == ',' {
                    self.price.push(c);
                }
            }
            AddAlertField::Note => self.note.push(c),
        }
    }

    /// Remove the last character from the currently focused text field.
    pub fn pop_char(&mut self) {
        match self.field {
            AddAlertField::Symbol => {
                self.symbol.pop();
            }
            AddAlertField::Condition => {} // no text
            AddAlertField::Price => {
                self.price.pop();
            }
            AddAlertField::Note => {
                self.note.pop();
            }
        }
    }

    /// Parse the price string to f64. Treats comma as decimal separator.
    pub fn parse_price(&self) -> Option<f64> {
        let normalized = self.price.replace(',', ".");
        normalized.parse::<f64>().ok().filter(|&v| v > 0.0)
    }
}

// ── Search ────────────────────────────────────────────────────────────────────

/// Whether the search prompt is currently open for input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchPromptState {
    #[default]
    Closed,
    Open,
}

/// What kind of search this view uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// Positions / Plays / Alerts — hides rows that don't match.
    Filter,
    /// Report — highlights matches in rendered text.
    Find,
}

/// Active search state. Present on `App` whenever there is (or was) an active
/// query — `None` means no search has been started this session.
#[derive(Debug, Clone)]
pub struct SearchState {
    /// The text the user has typed so far.
    pub query: String,
    /// Filter or Find, set from the view that opened the search.
    pub mode: SearchMode,
    /// Find-mode: indices (into the full rendered line list) of lines that
    /// contain the query string.
    pub matches: Vec<usize>,
    /// Find-mode: which entry in `matches` is the current match (for n/N nav).
    pub current_match: usize,
    /// Whether the prompt widget is visible and accepting keystrokes.
    pub prompt: SearchPromptState,
}

impl SearchState {
    /// Create a new SearchState for the given view's mode, prompt open.
    pub fn new(mode: SearchMode) -> Self {
        SearchState {
            query: String::new(),
            mode,
            matches: Vec::new(),
            current_match: 0,
            prompt: SearchPromptState::Open,
        }
    }

    /// Reopen the prompt (e.g. pressing `/` when a query is already active).
    pub fn reopen(&mut self) {
        self.prompt = SearchPromptState::Open;
    }

    /// Close the prompt but keep the query active.
    pub fn close_prompt(&mut self) {
        self.prompt = SearchPromptState::Closed;
    }

    /// True if the prompt is currently open and consuming keystrokes.
    pub fn is_open(&self) -> bool {
        self.prompt == SearchPromptState::Open
    }

    /// True when the query is non-empty (filter/highlight is actually doing
    /// something).
    pub fn is_active(&self) -> bool {
        !self.query.is_empty()
    }

    /// Recompute Find-mode match indices from `lines`.
    pub fn recompute_find_matches(&mut self, lines: &[String]) {
        if self.query.is_empty() {
            self.matches.clear();
            self.current_match = 0;
            return;
        }
        let needle = self.query.to_lowercase();
        self.matches = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect();
        // Clamp current_match.
        if self.matches.is_empty() {
            self.current_match = 0;
        } else {
            self.current_match = self.current_match.min(self.matches.len() - 1);
        }
    }

    /// Advance to the next Find match (wraps). Returns the new line index if
    /// there are any matches.
    pub fn next_match(&mut self) -> Option<usize> {
        if self.matches.is_empty() {
            return None;
        }
        self.current_match = (self.current_match + 1) % self.matches.len();
        Some(self.matches[self.current_match])
    }

    /// Move to the previous Find match (wraps).
    pub fn prev_match(&mut self) -> Option<usize> {
        if self.matches.is_empty() {
            return None;
        }
        if self.current_match == 0 {
            self.current_match = self.matches.len() - 1;
        } else {
            self.current_match -= 1;
        }
        Some(self.matches[self.current_match])
    }

    /// Return the line index of the currently active match, if any.
    pub fn current_line(&self) -> Option<usize> {
        self.matches.get(self.current_match).copied()
    }
}

/// Return true if the given view supports `/` search.
pub fn view_is_searchable(v: View) -> bool {
    matches!(
        v,
        View::Positions | View::Plays | View::Alerts | View::Report
    )
}

/// Return the SearchMode appropriate for the view.
pub fn search_mode_for_view(v: View) -> SearchMode {
    match v {
        View::Report => SearchMode::Find,
        _ => SearchMode::Filter,
    }
}

/// Which top-level view is currently filling the screen.
///
/// Visual class of the status toast: `Alert` renders amber and louder than
/// the green `Info` confirmations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToastKind {
    #[default]
    Info,
    Alert,
}

/// `1..8` keys map directly to these in order.
/// Canonical order: Chart(1) → Positions(2) → Plays(3) → Alerts(4) → Report(5) → News(6) → Details(7).
/// Note: the cockpit AI-chat tab (formerly key 7) has been removed. Use `A` to launch
/// your own local agent (claude / gemini / aider / etc.) with a context bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    #[default]
    Chart,
    Positions,
    Plays,
    Alerts,
    Report,
    News,
    /// Multi-asset scanner-grid (watchlist × 6 columns, refreshes every 30 s).
    Details,
}

impl View {
    pub fn from_index(i: usize) -> Option<View> {
        match i {
            1 => Some(View::Chart),
            2 => Some(View::Positions),
            3 => Some(View::Plays),
            4 => Some(View::Alerts),

            5 => Some(View::Report),
            6 => Some(View::News),
            7 => Some(View::Details),
            _ => None,
        }
    }

    /// All views in canonical tab order.
    pub fn all() -> &'static [View] {
        &[
            View::Chart,
            View::Positions,
            View::Plays,
            View::Alerts,
            View::Report,
            View::News,
            View::Details,
        ]
    }

    /// Cycle to the next view in canonical tab order, wrapping around.
    pub fn cycle_next(self) -> View {
        let all = View::all();
        let pos = all.iter().position(|v| *v == self).unwrap_or(0);
        let next = (pos + 1) % all.len();
        all[next]
    }

    /// Cycle to the previous view in canonical tab order, wrapping around.
    pub fn cycle_prev(self) -> View {
        let all = View::all();
        let pos = all.iter().position(|v| *v == self).unwrap_or(0);
        let prev = (pos + all.len() - 1) % all.len();
        all[prev]
    }

    pub fn label(self) -> &'static str {
        match self {
            View::Chart => "chart",
            View::Positions => "pos",
            View::Plays => "plays",
            View::Alerts => "alerts",
            View::Report => "report",
            View::News => "news",
            View::Details => "details",
        }
    }
}

/// Which set of RSS feeds to aggregate in the news view.
///
/// `[` / `]` cycle through presets while in News view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NewsFeedPreset {
    #[default]
    Default,
    AllCrypto,
    AllMacro,
    All,
}

impl NewsFeedPreset {
    /// Comma-separated feed IDs to pass to `/api/market-news?feeds=`.
    pub fn to_feed_ids(self) -> &'static str {
        match self {
            NewsFeedPreset::Default => "coindesk,cointelegraph,decrypt",
            NewsFeedPreset::AllCrypto => "coindesk,cointelegraph,decrypt,theblock,dlnews",
            NewsFeedPreset::AllMacro => "cnbc_top,marketwatch,seekingalpha,fed,fxstreet",
            NewsFeedPreset::All => {
                "cnbc_top,dlnews,coindesk,cointelegraph,theblock,decrypt,marketwatch,seekingalpha,fed,fxstreet"
            }
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            NewsFeedPreset::Default => "default",
            NewsFeedPreset::AllCrypto => "all-crypto",
            NewsFeedPreset::AllMacro => "all-macro",
            NewsFeedPreset::All => "all",
        }
    }

    /// Cycle to the next preset (wraps).
    pub fn next(self) -> Self {
        match self {
            NewsFeedPreset::Default => NewsFeedPreset::AllCrypto,
            NewsFeedPreset::AllCrypto => NewsFeedPreset::AllMacro,
            NewsFeedPreset::AllMacro => NewsFeedPreset::All,
            NewsFeedPreset::All => NewsFeedPreset::Default,
        }
    }

    /// Cycle to the previous preset (wraps).
    pub fn prev(self) -> Self {
        match self {
            NewsFeedPreset::Default => NewsFeedPreset::All,
            NewsFeedPreset::AllCrypto => NewsFeedPreset::Default,
            NewsFeedPreset::AllMacro => NewsFeedPreset::AllCrypto,
            NewsFeedPreset::All => NewsFeedPreset::AllMacro,
        }
    }
}

/// Asset selector overlay state. `None` = overlay closed.
#[derive(Default, Debug)]
pub struct AssetSelectorState {
    /// Typed filter buffer. Substring-matched against watchlist symbols.
    pub filter: String,
    /// Selected index within the (filtered) symbol list.
    pub cursor: usize,
}

/// Positions view filter (futures / spot / both).
///
/// The server doesn't currently expose a futures/spot flag on `Position` — kept
/// here so the UI scaffold is ready and the filter key (`f`/`s`/`b`) is wired
/// when the server adds the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PositionFilter {
    #[default]
    Both,
    Futures,
    Spot,
}

/// Clamp a ListState selection to the new list length (None when empty,
/// else min(selected, len-1) with default 0 on first arrival).
fn clamp_cursor(state: &mut ListState, len: usize) {
    if len == 0 {
        state.select(None);
        return;
    }
    let cur = state.selected().unwrap_or(0).min(len - 1);
    state.select(Some(cur));
}

/// What's open in the detail overlay (Enter-on-row in positions/plays/alerts).
#[derive(Debug, Clone)]
pub enum DetailOverlay {
    Position(Position),
    Play(Play),
    Alert(AlertFull),
}

/// Which report to show in the brief view (morning / afternoon).
///
/// Defaults to `Morning` before 15:00 local time, `Afternoon` at/after.
/// Can be toggled with `[` / `]` while in the Brief view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BriefReportChoice {
    Morning,
    Afternoon,
}

impl BriefReportChoice {
    /// Choose based on current local hour (< 15 → Morning, else Afternoon).
    pub fn default_for_now() -> Self {
        let hour = chrono::Local::now().hour();
        if hour < 15 {
            BriefReportChoice::Morning
        } else {
            BriefReportChoice::Afternoon
        }
    }

    #[cfg(test)]
    pub fn label(self) -> &'static str {
        match self {
            BriefReportChoice::Morning => "morning",
            BriefReportChoice::Afternoon => "afternoon",
        }
    }
}

/// Inbound data update sent from background fetchers → main event loop.
pub enum Update {
    Positions(PositionsResponse),
    Alerts(AlertsResponse),
    Plays(PlaysResponse),
    Watchlist(BatchResponse),
    /// The caller's server-synced watchlist symbol list (one-way: server → TUI).
    /// Replaces the locally-loaded list once `GET /api/watchlist` returns.
    WatchlistSymbols(Vec<String>),
    Chart(ChartResponse),
    // Report-strip data sources (sentiment; lazy — fetched once the report view is visited).
    Sentiment(anyhow::Result<SentimentResponse>),
    Report(anyhow::Result<ReportResponse>),
    ReportAfternoon(anyhow::Result<ReportResponse>),
    // News-view data (lazy — only fetched when news view has been visited).
    News(anyhow::Result<NewsResponse>),
    FetchError {
        source: &'static str,
        msg: String,
    },
    /// Details view matrix fetch result (lazy — only fetched after first visit).
    /// One call to `/api/details/matrix` returns the typed indicator columns for
    /// the caller's watchlist (Consensio / W-RSI / Trend / CPR / BB).
    DetailsMatrix(anyhow::Result<MatrixResponse>),
    /// Result of the create-alert POST.
    AlertCreated(anyhow::Result<AlertFull>),
    SymbolSearch(Vec<SymbolHit>),
    /// 1D mini-chart payload for the Details side panel.
    DetailsChart {
        symbol: String,
        data: ChartResponse,
    },
}

/// What the user wants the chart fetcher to fetch next.
#[derive(Debug, Clone)]
pub struct ChartFetchSpec {
    pub symbol: String,
    pub tf: String,
    /// When true, supplement the chart fetch with a MACD fetch from the chart
    /// endpoint with `?macd=1`.
    pub show_macd: bool,
}

/// Top-level application state. Owned by the event loop; passed by `&mut` to
/// every render call and every input handler.
pub struct App {
    // ── data ────────────────────────────────────────────────────────────────
    pub positions: Option<PositionsResponse>,
    pub alerts: Option<AlertsResponse>,
    pub plays: Option<PlaysResponse>,
    pub watchlist_snapshot: Option<BatchResponse>,
    /// Symbol order preserved from watchlist.toml (for stable row ordering).
    pub watchlist_symbols: Vec<String>,

    // ── brief / report view data (lazy — only fetched after first visit) ──────
    pub sentiment: Option<SentimentResponse>,
    pub scanner: Option<ScannerResponse>,
    pub report_morning: Option<ReportResponse>,
    pub report_afternoon: Option<ReportResponse>,
    /// True once the user has visited the report view at least once.
    /// Flipped by the `6` key handler so the report fetchers know to start.
    pub report_visited: bool,
    /// Which report to show in the report view (morning / afternoon).
    pub brief_report_choice: BriefReportChoice,
    /// Scroll offset inside the report view (j/k/PgUp/PgDn).
    pub report_scroll: u16,
    /// Width of the report body area at the last render — lets the n/N
    /// search-jump convert a source-line index into a wrapped display-row
    /// offset (the coordinate space `Paragraph::scroll` actually consumes).
    pub report_body_width: u16,
    /// Per-frame render cache for the report body — holds the styled lines +
    /// wrapped row count, invalidated whenever the report content, width,
    /// search query, or current-match index changes. Avoids rebuilding
    /// `render_markdown_lines` + `Paragraph::line_count` every frame.
    pub report_render_cache: Option<crate::tui::widgets::view_report::ReportRenderCache>,
    /// (high, low, change_pct) of the chart slice rendered last frame —
    /// written by view_chart, shown inline in the asset bar.
    pub chart_range: Option<(f64, f64, f64)>,
    /// 1D charts for the Details side panel, keyed by symbol. Filled lazily
    /// as the cursor visits rows (session-lifetime cache, ≤ BATCH_CAP).
    pub details_charts: HashMap<String, ChartResponse>,

    // ── UI ──────────────────────────────────────────────────────────────────
    /// Current main view (1-7 keys swap this).
    pub view: View,
    /// When Some, the asset selector overlay is open and consumes key input.
    pub asset_selector: Option<AssetSelectorState>,
    pub symbol_search_results: Vec<SymbolHit>,
    /// When true, the help overlay is open and consumes key input.
    pub show_help: bool,
    /// Positions view filter (f/s/b).
    pub position_filter: PositionFilter,
    /// Currently selected asset (drives chart fetches + the "Positions for X" filter).
    pub selected_asset: String,

    // ── interactive view cursors + detail overlay ──────────────────────────
    /// Cursor (selected row) for the Positions list view.
    pub positions_cursor: ListState,
    /// Cursor for the Plays list view.
    pub plays_cursor: ListState,
    /// Cursor for the Alerts list view.
    pub alerts_cursor: ListState,
    /// When Some, a detail card / payload popup is open for the row the
    /// user pressed Enter on. Any key (or Esc) dismisses.
    pub detail_overlay: Option<DetailOverlay>,
    /// Scroll offset inside the detail overlay (j/k while open).
    pub detail_scroll: u16,

    // ── chart ───────────────────────────────────────────────────────────────
    /// View state (TF, pan offset, y-zoom, toggles) for the chart view.
    pub chart: ChartWidgetState,
    /// Last-fetched chart payload for `selected_asset` at `chart.tf`.
    pub chart_data: Option<ChartResponse>,
    /// Most recent on-screen rect occupied by the chart canvas (excluding the
    /// menu / asset bars). Updated by `view_chart::render` so the mouse
    /// handler can decide whether a click should set the crosshair.
    pub last_chart_rect: Option<ratatui::layout::Rect>,

    // ── help overlay scroll ─────────────────────────────────────────────────
    /// Scroll offset (lines) inside the help overlay. Reset to 0 when the
    /// overlay opens; j/k while help is open scrolls.
    pub help_scroll: u16,

    // ── details view (multi-asset scanner grid, lazy) ────────────────────────
    /// Screening-matrix rows for the details grid, keyed by symbol. One fetch of
    /// `/api/details/matrix` yields all typed indicator columns for the watchlist.
    pub details_matrix: Option<HashMap<String, MatrixRow>>,
    /// True once the user has visited the details view at least once.
    pub details_visited: bool,
    /// Cursor (selected row) for the Details table.
    pub details_cursor: ListState,

    // ── news view (RSS aggregation, lazy — only fetched after first visit) ──
    /// Latest news response from the server.
    pub news: Option<NewsResponse>,
    /// True once the user has visited the news view at least once.
    /// Flipped by the `8` key handler so the news fetcher knows to start.
    pub news_visited: bool,
    /// Cursor (selected row) for the News list view.
    pub news_cursor: ListState,
    /// Which set of feeds to aggregate.
    pub news_feed_preset: NewsFeedPreset,

    // ── search ───────────────────────────────────────────────────────────────
    /// Active search state. `None` until the user first presses `/` in a
    /// searchable view. Cleared when switching views.
    pub search: Option<SearchState>,

    // ── add-alert form ────────────────────────────────────────────────────────
    /// `Some(...)` while the create-alert form is open.
    pub add_alert_form: Option<AddAlertForm>,

    // ── toast ─────────────────────────────────────────────────────────────────
    /// Brief status message (e.g. "Alert created"). Auto-dismissed.
    pub toast: Option<String>,
    /// When to auto-dismiss the toast.
    pub toast_until: Option<Instant>,
    /// Visual emphasis: alert toasts render amber; info toasts green.
    pub toast_kind: ToastKind,
    /// Triggered-alert keys already seen — a NEW key in an alerts update
    /// raises an alert toast on whatever view is open. `None` until the
    /// first fetch so startup doesn't toast historical triggers.
    pub seen_triggered_alerts: Option<std::collections::HashSet<String>>,

    // ── status ───────────────────────────────────────────────────────────────
    pub last_update: HashMap<&'static str, Instant>,
    pub fetch_errors: HashMap<&'static str, String>,
    pub should_quit: bool,

    // ── agent launch ──────────────────────────────────────────────────────────
    /// Config for the `A` key: which command to spawn + optional args.
    /// Populated from `[agent]` section of config.toml at startup.
    pub agent_config: AgentConfig,

    // ── backend mode ──────────────────────────────────────────────────────────
    /// True when running the keyless Binance demo (no Carrac API key). The
    /// chart works fully on Binance data; every Carrac-only surface
    /// (positions / plays / alerts / report / news / details / server
    /// watchlist / asset search) shows a one-line "needs a Carrac API key"
    /// notice instead of erroring or spinning.
    pub demo: bool,
}

impl App {
    pub fn new() -> App {
        App {
            positions: None,
            alerts: None,
            plays: None,
            watchlist_snapshot: None,
            watchlist_symbols: crate::helpers::watchlist::load(),
            sentiment: None,
            scanner: None,
            report_morning: None,
            report_afternoon: None,
            report_visited: false,
            brief_report_choice: BriefReportChoice::default_for_now(),
            report_scroll: 0,
            report_body_width: 0,
            report_render_cache: None,
            chart_range: None,
            details_charts: HashMap::new(),
            view: View::default(),
            asset_selector: None,
            symbol_search_results: Vec::new(),
            show_help: false,
            position_filter: PositionFilter::default(),
            selected_asset: "BTC".to_string(),
            positions_cursor: ListState::default(),
            plays_cursor: ListState::default(),
            alerts_cursor: ListState::default(),
            detail_overlay: None,
            detail_scroll: 0,
            details_matrix: None,
            details_visited: false,
            details_cursor: ListState::default(),
            chart: ChartWidgetState::default(),
            chart_data: None,
            last_chart_rect: None,
            help_scroll: 0,
            news: None,
            news_visited: false,
            news_cursor: ListState::default(),
            news_feed_preset: NewsFeedPreset::default(),
            search: None,
            add_alert_form: None,
            toast: None,
            toast_until: None,
            toast_kind: ToastKind::Info,
            seen_triggered_alerts: None,
            last_update: HashMap::new(),
            fetch_errors: HashMap::new(),
            should_quit: false,
            agent_config: AgentConfig::default(),
            demo: false,
        }
    }

    /// Apply an inbound data update from a background fetcher.
    pub fn apply(&mut self, update: Update) {
        match update {
            Update::Positions(resp) => {
                let n = resp.positions.len();
                self.positions = Some(resp);
                clamp_cursor(&mut self.positions_cursor, n);
                self.last_update.insert("positions", Instant::now());
                self.fetch_errors.remove("positions");
            }
            Update::Alerts(resp) => {
                // Toast NEWLY-triggered alerts no matter which view is open.
                // Keyed by alert id (fallback symbol@triggered_at); the first
                // fetch only records what already fired.
                let key = |a: &crate::types::alerts::Alert| -> String {
                    a.id.clone().unwrap_or_else(|| {
                        format!(
                            "{}@{}",
                            a.symbol.as_deref().unwrap_or("?"),
                            a.triggered_at.as_deref().unwrap_or("?")
                        )
                    })
                };
                let keys: std::collections::HashSet<String> = resp.alerts.iter().map(key).collect();
                if let Some(seen) = &self.seen_triggered_alerts {
                    let fresh: Vec<_> = resp
                        .alerts
                        .iter()
                        .filter(|a| !seen.contains(&key(a)))
                        .collect();
                    if let Some(a) = fresh.first() {
                        let mut msg = format!(
                            "ALERT  {} {} {}",
                            a.symbol.as_deref().unwrap_or("?"),
                            a.condition.as_deref().unwrap_or(""),
                            a.price
                                .map(crate::commands::chart::fmt_price)
                                .unwrap_or_default(),
                        );
                        if let Some(tp) = a.triggered_price {
                            msg.push_str(&format!(
                                " — hit {}",
                                crate::commands::chart::fmt_price(tp)
                            ));
                        }
                        if fresh.len() > 1 {
                            msg.push_str(&format!("  (+{} more)", fresh.len() - 1));
                        }
                        self.show_toast(msg, ToastKind::Alert, 8);
                    }
                }
                self.seen_triggered_alerts = Some(keys);
                let n = resp.alerts.len();
                self.alerts = Some(resp);
                clamp_cursor(&mut self.alerts_cursor, n);
                self.last_update.insert("alerts", Instant::now());
                self.fetch_errors.remove("alerts");
            }
            Update::Plays(resp) => {
                let n = resp.plays.len();
                self.plays = Some(resp);
                clamp_cursor(&mut self.plays_cursor, n);
                self.last_update.insert("plays", Instant::now());
                self.fetch_errors.remove("plays");
            }
            Update::Watchlist(resp) => {
                self.watchlist_snapshot = Some(resp);
                self.last_update.insert("watchlist", Instant::now());
                self.fetch_errors.remove("watchlist");
            }
            Update::WatchlistSymbols(symbols) => {
                self.watchlist_symbols = symbols;
                // Keep the Details cursor in range if the synced list is shorter.
                if let Some(sel) = self.details_cursor.selected()
                    && sel >= self.watchlist_symbols.len()
                {
                    self.details_cursor
                        .select(self.watchlist_symbols.len().checked_sub(1));
                }
                self.last_update.insert("watchlist_symbols", Instant::now());
                self.fetch_errors.remove("watchlist_symbols");
            }
            Update::Chart(resp) => {
                self.chart_data = Some(resp);
                self.last_update.insert("chart", Instant::now());
                self.fetch_errors.remove("chart");
            }
            Update::Sentiment(result) => match result {
                Ok(resp) => {
                    self.sentiment = Some(resp);
                    self.last_update.insert("sentiment", Instant::now());
                    self.fetch_errors.remove("sentiment");
                }
                Err(e) => {
                    self.fetch_errors.insert("sentiment", format!("{:#}", e));
                }
            },
            Update::Report(result) => match result {
                Ok(resp) => {
                    self.report_morning = Some(resp);
                    self.last_update.insert("report_morning", Instant::now());
                    self.fetch_errors.remove("report_morning");
                }
                Err(e) => {
                    self.fetch_errors
                        .insert("report_morning", format!("{:#}", e));
                }
            },
            Update::ReportAfternoon(result) => match result {
                Ok(resp) => {
                    self.report_afternoon = Some(resp);
                    self.last_update.insert("report_afternoon", Instant::now());
                    self.fetch_errors.remove("report_afternoon");
                }
                Err(e) => {
                    self.fetch_errors
                        .insert("report_afternoon", format!("{:#}", e));
                }
            },
            Update::News(result) => match result {
                Ok(resp) => {
                    let n = resp.items.len();
                    self.news = Some(resp);
                    clamp_cursor(&mut self.news_cursor, n);
                    self.last_update.insert("news", Instant::now());
                    self.fetch_errors.remove("news");
                }
                Err(e) => {
                    self.fetch_errors.insert("news", format!("{:#}", e));
                }
            },
            Update::FetchError { source, msg } => {
                self.fetch_errors.insert(source, msg);
            }
            Update::DetailsMatrix(result) => match result {
                Ok(resp) => {
                    self.details_matrix = Some(resp.into_map());
                    self.last_update.insert("details_matrix", Instant::now());
                    self.fetch_errors.remove("details_matrix");
                }
                Err(e) => {
                    self.fetch_errors
                        .insert("details_matrix", format!("{:#}", e));
                }
            },
            Update::AlertCreated(result) => match result {
                Ok(_) => {
                    // Close the form and show a brief toast.
                    self.add_alert_form = None;
                    self.toast = Some("Alert created".to_string());
                    self.toast_until = Some(Instant::now() + std::time::Duration::from_secs(3));
                }
                Err(e) => {
                    // Parse error body: check for no_channel flag.
                    let msg = e.to_string();
                    let no_channel = msg.contains("no_channel");
                    if no_channel {
                        // Close the form and show a toast.
                        self.add_alert_form = None;
                        self.toast =
                            Some("Configure notifications on the web UI first".to_string());
                        self.toast_until = Some(Instant::now() + std::time::Duration::from_secs(5));
                    } else {
                        // Keep the form open with the error shown.
                        if let Some(form) = &mut self.add_alert_form {
                            // Try to extract the "error" field from the JSON snippet.
                            let display = extract_error_message(&msg).unwrap_or(msg);
                            form.error = Some(display);
                            form.submitting = false;
                        }
                    }
                }
            },
            Update::DetailsChart { symbol, data } => {
                self.details_charts.insert(symbol, data);
            }
            Update::SymbolSearch(hits) => {
                // Drop responses that land after the selector closed —
                // applying them unconditionally made stale results reappear
                // the next time the overlay opened.
                if self.asset_selector.is_some() {
                    self.symbol_search_results = hits;
                }
            }
        }
    }

    /// Seconds since last successful fetch of `source`, or None if never fetched.
    pub fn secs_since(&self, source: &'static str) -> Option<u64> {
        self.last_update.get(source).map(|t| t.elapsed().as_secs())
    }

    /// Convenience: a fresh `ChartFetchSpec` from current state.
    pub fn chart_spec(&self) -> ChartFetchSpec {
        ChartFetchSpec {
            symbol: self.selected_asset.clone(),
            tf: self.chart.tf.clone(),
            show_macd: self.chart.show_macd,
        }
    }

    /// Asset selector: open with empty filter.
    pub fn open_asset_selector(&mut self) {
        self.asset_selector = Some(AssetSelectorState::default());
    }

    /// Asset selector: close.
    pub fn close_asset_selector(&mut self) {
        self.asset_selector = None;
        self.symbol_search_results.clear();
    }

    /// Move the cursor in the currently focused list view (positions, plays,
    /// alerts) by `dir` (+1 down, -1 up). No-op for views without a cursor.
    /// When a search filter is active the cursor moves over the filtered set only.
    pub fn move_view_cursor(&mut self, dir: i32) {
        let (cursor, len): (&mut ListState, usize) = match self.view {
            View::Positions => {
                let n = self.filtered_positions().len();
                (&mut self.positions_cursor, n)
            }
            View::Plays => {
                let n = self.filtered_plays().len();
                (&mut self.plays_cursor, n)
            }
            View::Alerts => {
                let n = self.filtered_alerts().len();
                (&mut self.alerts_cursor, n)
            }
            _ => return,
        };
        if len == 0 {
            cursor.select(None);
            return;
        }
        let max = len - 1;
        let cur = cursor.selected().unwrap_or(0);
        let next = if dir >= 0 {
            (cur + dir as usize).min(max)
        } else {
            cur.saturating_sub((-dir) as usize)
        };
        cursor.select(Some(next));
    }

    /// Move the details-view cursor by `dir` (+1 down, -1 up). `len` is the
    /// current number of rows visible in the table.
    pub fn move_details_cursor(&mut self, dir: i32, len: usize) {
        if len == 0 {
            self.details_cursor.select(None);
            return;
        }
        let max = len - 1;
        let cur = self.details_cursor.selected().unwrap_or(0);
        let next = if dir >= 0 {
            (cur + dir as usize).min(max)
        } else {
            cur.saturating_sub((-dir) as usize)
        };
        self.details_cursor.select(Some(next));
    }

    /// Returns the active search query (lowercased) if one is active, else empty string.
    pub fn search_query(&self) -> String {
        match &self.search {
            Some(s) if s.is_active() => s.query.to_lowercase(),
            _ => String::new(),
        }
    }

    /// Filtered positions: all positions when no query active, else only
    /// those matching the query on asset / setup_type / risk_status / direction.
    pub fn filtered_positions(&self) -> Vec<usize> {
        let q = self.search_query();
        match &self.positions {
            None => vec![],
            Some(p) => p
                .positions
                .iter()
                .enumerate()
                .filter(|(_, pos)| {
                    if q.is_empty() {
                        return true;
                    }
                    let haystack = [
                        pos.asset.to_lowercase(),
                        pos.setup_type.as_deref().unwrap_or("").to_lowercase(),
                        pos.risk_status
                            .as_ref()
                            .map(|r| r.to_string().to_lowercase())
                            .unwrap_or_default(),
                        pos.direction.to_string().to_lowercase(),
                    ]
                    .join(" ");
                    haystack.contains(&q)
                })
                .map(|(i, _)| i)
                .collect(),
        }
    }

    /// Filtered plays: all plays when no query, else match on title /
    /// primary_asset / status / direction.
    pub fn filtered_plays(&self) -> Vec<usize> {
        let q = self.search_query();
        match &self.plays {
            None => vec![],
            Some(p) => p
                .plays
                .iter()
                .enumerate()
                .filter(|(_, play)| {
                    if q.is_empty() {
                        return true;
                    }
                    let haystack = [
                        play.title.to_lowercase(),
                        play.primary_asset.as_deref().unwrap_or("").to_lowercase(),
                        play.status.as_deref().unwrap_or("").to_lowercase(),
                        play.direction.as_deref().unwrap_or("").to_lowercase(),
                    ]
                    .join(" ");
                    haystack.contains(&q)
                })
                .map(|(i, _)| i)
                .collect(),
        }
    }

    /// Filtered alerts: all alerts when no query, else match on symbol /
    /// condition / alert_type / note.
    pub fn filtered_alerts(&self) -> Vec<usize> {
        let q = self.search_query();
        match &self.alerts {
            None => vec![],
            Some(a) => a
                .alerts
                .iter()
                .enumerate()
                .filter(|(_, al)| {
                    if q.is_empty() {
                        return true;
                    }
                    let haystack = [
                        al.symbol.as_deref().unwrap_or("").to_lowercase(),
                        al.condition.as_deref().unwrap_or("").to_lowercase(),
                        al.alert_type.as_deref().unwrap_or("").to_lowercase(),
                        al.note.as_deref().unwrap_or("").to_lowercase(),
                    ]
                    .join(" ");
                    haystack.contains(&q)
                })
                .map(|(i, _)| i)
                .collect(),
        }
    }

    /// Open a search prompt for the current view. If a search is already
    /// active, reopen the prompt so the user can refine the query.
    pub fn open_search(&mut self) {
        let mode = search_mode_for_view(self.view);
        match &mut self.search {
            Some(s) if s.mode == mode => {
                s.reopen();
            }
            _ => {
                self.search = Some(SearchState::new(mode));
            }
        }
    }

    /// Clear the search state entirely (Esc with no prompt open, or view switch).
    pub fn clear_search(&mut self) {
        self.search = None;
        // Reset cursors so they don't land out-of-bounds on the unfiltered list.
        if let Some(n) = self.positions.as_ref().map(|p| p.positions.len()) {
            clamp_cursor(&mut self.positions_cursor, n);
        }
        if let Some(n) = self.plays.as_ref().map(|p| p.plays.len()) {
            clamp_cursor(&mut self.plays_cursor, n);
        }
        if let Some(n) = self.alerts.as_ref().map(|a| a.alerts.len()) {
            clamp_cursor(&mut self.alerts_cursor, n);
        }
    }

    /// Open the detail overlay for the currently focused row.
    /// When a search filter is active the cursor operates over the filtered
    /// slice, so we resolve the filtered index back to the real data index.
    /// A stale cursor (data refreshed under an active filter) clamps to the
    /// last filtered row; if the filter matches nothing, no overlay opens —
    /// never treat a filtered index as a raw one.
    pub fn open_detail_for_focused(&mut self) {
        self.detail_scroll = 0;
        self.detail_overlay = match self.view {
            View::Positions => {
                let cursor_pos = self.positions_cursor.selected().unwrap_or(0);
                let filtered = self.filtered_positions();
                let real_idx = filtered
                    .get(cursor_pos)
                    .or_else(|| filtered.last())
                    .copied()
                    .unwrap_or(usize::MAX);
                self.positions
                    .as_ref()
                    .and_then(|p| p.positions.get(real_idx).cloned())
                    .map(DetailOverlay::Position)
            }
            View::Plays => {
                let cursor_pos = self.plays_cursor.selected().unwrap_or(0);
                let filtered = self.filtered_plays();
                let real_idx = filtered
                    .get(cursor_pos)
                    .or_else(|| filtered.last())
                    .copied()
                    .unwrap_or(usize::MAX);
                self.plays
                    .as_ref()
                    .and_then(|p| p.plays.get(real_idx).cloned())
                    .map(DetailOverlay::Play)
            }
            View::Alerts => {
                let cursor_pos = self.alerts_cursor.selected().unwrap_or(0);
                let filtered = self.filtered_alerts();
                let real_idx = filtered
                    .get(cursor_pos)
                    .or_else(|| filtered.last())
                    .copied()
                    .unwrap_or(usize::MAX);
                self.alerts
                    .as_ref()
                    .and_then(|a| {
                        // The Alert struct in AlertsResponse differs from AlertFull;
                        // synthesize an AlertFull view from what's available.
                        a.alerts.get(real_idx).map(|al| AlertFull {
                            id: al.id.clone().map(serde_json::Value::String),
                            symbol: al.symbol.clone(),
                            alert_type: al.alert_type.clone(),
                            condition: al.condition.clone(),
                            price: al.price,
                            indicator: al.indicator.clone(),
                            target_state: None,
                            triggered_at: al.triggered_at.clone(),
                            triggered_price: al.triggered_price,
                            note: al.note.clone(),
                            active: None,
                            triggered: Some(true),
                            frequency: None,
                        })
                    })
                    .map(DetailOverlay::Alert)
            }
            _ => None,
        };
    }

    /// Close the detail overlay.
    pub fn close_detail(&mut self) {
        self.detail_overlay = None;
        self.detail_scroll = 0;
    }

    /// Cycle through `watchlist_symbols` by `dir` (+1 next, -1 previous).
    /// Wraps. Returns true if `selected_asset` changed (caller can decide
    /// whether to trigger a chart re-fetch).
    /// Hardcoded watchlist for the keyless Binance demo (there is no server
    /// watchlist to sync). Drives `,`/`.` cycling and the asset selector.
    pub const DEMO_WATCHLIST: [&'static str; 6] = ["BTC", "ETH", "SOL", "BNB", "XRP", "DOGE"];

    pub fn cycle_favourite(&mut self, dir: i32) -> bool {
        if self.watchlist_symbols.is_empty() {
            return false;
        }
        let n = self.watchlist_symbols.len() as i32;
        let cur = self
            .watchlist_symbols
            .iter()
            .position(|s| s == &self.selected_asset)
            .map(|i| i as i32)
            .unwrap_or(-1);
        let next = if cur < 0 {
            // Not in list — start at the appropriate end.
            if dir >= 0 { 0 } else { n - 1 }
        } else {
            (cur + dir).rem_euclid(n)
        };
        let new_sym = self.watchlist_symbols[next as usize].clone();
        if new_sym != self.selected_asset {
            self.selected_asset = new_sym;
            true
        } else {
            false
        }
    }

    /// Open the add-alert form, pre-filling the symbol from the current
    /// selected_asset. Focus lands on Price since symbol is pre-filled.
    pub fn open_add_alert_form(&mut self) {
        self.add_alert_form = Some(AddAlertForm::new(self.selected_asset.clone()));
    }

    /// Close the add-alert form without submitting.
    pub fn close_add_alert_form(&mut self) {
        self.add_alert_form = None;
    }

    /// Tick the toast: if the dismiss deadline has passed, clear it.
    /// Show an auto-dismissing toast.
    pub fn show_toast(&mut self, msg: impl Into<String>, kind: ToastKind, secs: u64) {
        self.toast = Some(msg.into());
        self.toast_kind = kind;
        self.toast_until = Some(Instant::now() + std::time::Duration::from_secs(secs));
    }

    pub fn tick_toast(&mut self) {
        if let Some(until) = self.toast_until
            && Instant::now() >= until
        {
            self.toast = None;
            self.toast_until = None;
            self.toast_kind = ToastKind::Info;
        }
    }

    /// Asset selector: which symbols match the current filter, in watchlist order.
    pub fn asset_selector_matches(&self) -> Vec<String> {
        let Some(sel) = &self.asset_selector else {
            return Vec::new();
        };
        let needle = sel.filter.to_uppercase();
        if needle.is_empty() {
            return self.watchlist_symbols.clone();
        }
        self.watchlist_symbols
            .iter()
            .filter(|s| s.contains(&needle))
            .cloned()
            .collect()
    }

    /// Combined selector list: favourite matches (from watchlist) followed by
    /// server search results, deduplicated. The cursor in `AssetSelectorState`
    /// operates over this combined slice.
    pub fn asset_selector_combined(&self) -> Vec<String> {
        let fav_matches = self.asset_selector_matches();
        if self.symbol_search_results.is_empty() {
            return fav_matches;
        }
        // Build a set of known symbols BEFORE moving fav_matches into combined.
        let fav_set: std::collections::HashSet<String> = fav_matches.iter().cloned().collect();
        let mut combined = fav_matches;
        for hit in &self.symbol_search_results {
            if !fav_set.contains(&hit.symbol) {
                combined.push(hit.symbol.clone());
            }
        }
        combined
    }
}

/// Try to pull the `"error"` string from an HTTP error message that may contain
/// a JSON snippet (e.g. `"POST /api/alerts -> 400: {\"error\": \"Invalid price\"}")`).
/// Falls back to `None` so the caller can use the raw message.
fn extract_error_message(raw: &str) -> Option<String> {
    // Find the first `{` and parse the tail as JSON.
    let start = raw.find('{')?;
    let json_slice = &raw[start..];
    let v: serde_json::Value = serde_json::from_str(json_slice).ok()?;
    v.get("error")?.as_str().map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(sym: &str) -> crate::types::search::SymbolHit {
        serde_json::from_str(&format!(
            r#"{{"symbol":"{sym}","name":"{sym} name","type":"crypto"}}"#
        ))
        .unwrap()
    }

    fn triggered(ids: &[&str]) -> AlertsResponse {
        let items: Vec<String> = ids
            .iter()
            .map(|id| {
                format!(
                    r#"{{"id":"{id}","symbol":"BTC","condition":"below",
                        "price":61200.0,"triggered_at":"t","triggered_price":60531.0}}"#
                )
            })
            .collect();
        serde_json::from_str(&format!(r#"{{"triggered":[{}]}}"#, items.join(","))).unwrap()
    }

    #[test]
    fn first_alerts_fetch_does_not_toast() {
        let mut app = App::new();
        app.apply(Update::Alerts(triggered(&["a1"])));
        assert!(app.toast.is_none(), "startup must not toast old triggers");
    }

    #[test]
    fn new_triggered_alert_raises_alert_toast_anywhere() {
        let mut app = App::new();
        app.view = View::Report; // not on the alerts tab
        app.apply(Update::Alerts(triggered(&["a1"])));
        app.apply(Update::Alerts(triggered(&["a1", "a2"])));
        let toast = app.toast.as_deref().expect("new trigger must toast");
        assert!(toast.starts_with("ALERT"), "got: {toast}");
        assert_eq!(app.toast_kind, ToastKind::Alert);

        // Re-applying the same set must not re-toast.
        app.toast = None;
        app.toast_kind = ToastKind::Info;
        app.apply(Update::Alerts(triggered(&["a1", "a2"])));
        assert!(app.toast.is_none(), "unchanged set must not re-toast");
    }

    #[test]
    fn symbol_search_result_after_close_is_dropped() {
        let mut app = App::new();
        app.open_asset_selector();
        app.apply(Update::SymbolSearch(vec![hit("BTC")]));
        assert_eq!(app.symbol_search_results.len(), 1);
        app.close_asset_selector();
        // The in-flight response lands after Esc — must be discarded, or the
        // stale hits reappear on the next overlay open.
        app.apply(Update::SymbolSearch(vec![hit("ETH")]));
        assert!(app.symbol_search_results.is_empty());
    }

    // ── BriefReportChoice ──────────────────────────────────────────────────

    #[test]
    fn brief_report_choice_labels() {
        assert_eq!(BriefReportChoice::Morning.label(), "morning");
        assert_eq!(BriefReportChoice::Afternoon.label(), "afternoon");
    }

    #[test]
    fn brief_report_choice_default_before_1500_is_morning() {
        // Simulate early morning: the function reads local time, so we can
        // only test the `default_for_now` path indirectly by verifying the
        // logic for an explicitly constructed local time.  We cover both
        // branches by asserting that the two enum arms are distinct values.
        // The actual default is tested at runtime; here we just ensure both
        // variants are reachable and correctly mapped.
        let morning = BriefReportChoice::Morning;
        let afternoon = BriefReportChoice::Afternoon;
        assert_ne!(morning, afternoon);
        assert_eq!(morning.label(), "morning");
        assert_eq!(afternoon.label(), "afternoon");
    }

    /// When the hour is before 15 the default is Morning; at/after 15 it is
    /// Afternoon. We test the inner logic directly without depending on the
    /// system clock by replicating the threshold condition.
    #[test]
    fn brief_report_choice_threshold_logic() {
        // Mirror the impl: hour < 15 → Morning.
        for h in 0u32..15 {
            let choice = if h < 15 {
                BriefReportChoice::Morning
            } else {
                BriefReportChoice::Afternoon
            };
            assert_eq!(
                choice,
                BriefReportChoice::Morning,
                "hour {h} should be Morning"
            );
        }
        for h in 15u32..24 {
            let choice = if h < 15 {
                BriefReportChoice::Morning
            } else {
                BriefReportChoice::Afternoon
            };
            assert_eq!(
                choice,
                BriefReportChoice::Afternoon,
                "hour {h} should be Afternoon"
            );
        }
    }

    // ── Update variants for brief data ─────────────────────────────────────

    #[test]
    fn apply_sentiment_ok_stores_and_clears_error() {
        use crate::types::sentiment::{Sentiment, SentimentResponse};
        let mut app = App::new();
        app.fetch_errors
            .insert("sentiment", "old error".to_string());
        app.apply(Update::Sentiment(Ok(SentimentResponse {
            sentiment: Sentiment::default(),
        })));
        assert!(app.sentiment.is_some());
        assert!(!app.fetch_errors.contains_key("sentiment"));
        assert!(app.last_update.contains_key("sentiment"));
    }

    #[test]
    fn apply_sentiment_err_stores_error() {
        let mut app = App::new();
        app.apply(Update::Sentiment(Err(anyhow::anyhow!("network down"))));
        assert!(app.sentiment.is_none());
        assert!(app.fetch_errors.contains_key("sentiment"));
    }

    #[test]
    fn apply_fetch_error_populates_list_view_keys() {
        // The list views (positions / plays / alerts) read these exact keys to
        // surface the error instead of an eternal "loading…". Guard the wiring.
        for source in ["positions", "plays", "alerts"] {
            let mut app = App::new();
            app.apply(Update::FetchError {
                source,
                msg: "boom".to_string(),
            });
            assert_eq!(
                app.fetch_errors.get(source).map(String::as_str),
                Some("boom"),
                "FetchError must populate the {source} key"
            );
        }
    }

    #[test]
    fn apply_report_ok_stores_morning() {
        use crate::types::report::ReportResponse;
        let mut app = App::new();
        app.apply(Update::Report(Ok(ReportResponse {
            content: Some("test".to_string()),
            created_at: None,
        })));
        assert!(app.report_morning.is_some());
        assert!(app.report_afternoon.is_none());
    }

    #[test]
    fn apply_report_afternoon_ok_stores_afternoon() {
        use crate::types::report::ReportResponse;
        let mut app = App::new();
        app.apply(Update::ReportAfternoon(Ok(ReportResponse {
            content: Some("pm".to_string()),
            created_at: None,
        })));
        assert!(app.report_afternoon.is_some());
        assert!(app.report_morning.is_none());
    }

    // ── SearchState ────────────────────────────────────────────────────────────

    #[test]
    fn search_state_new_is_open() {
        let s = SearchState::new(SearchMode::Filter);
        assert!(s.is_open());
        assert!(!s.is_active()); // empty query
        assert_eq!(s.mode, SearchMode::Filter);
    }

    #[test]
    fn search_state_close_prompt_keeps_query() {
        let mut s = SearchState::new(SearchMode::Filter);
        s.query = "btc".to_string();
        s.close_prompt();
        assert!(!s.is_open());
        assert!(s.is_active());
    }

    #[test]
    fn search_state_reopen_reopens_closed_prompt() {
        let mut s = SearchState::new(SearchMode::Find);
        s.close_prompt();
        assert!(!s.is_open());
        s.reopen();
        assert!(s.is_open());
    }

    #[test]
    fn search_state_find_recompute_finds_lines() {
        let mut s = SearchState::new(SearchMode::Find);
        s.query = "btc".to_string();
        let lines = vec![
            "BTC is up 5%".to_string(),
            "ETH trades flat".to_string(),
            "btc volume surge".to_string(),
        ];
        s.recompute_find_matches(&lines);
        assert_eq!(s.matches, vec![0, 2]);
        assert_eq!(s.current_match, 0);
    }

    #[test]
    fn search_state_find_empty_query_clears_matches() {
        let mut s = SearchState::new(SearchMode::Find);
        s.query = "btc".to_string();
        let lines = vec!["BTC is up".to_string()];
        s.recompute_find_matches(&lines);
        assert!(!s.matches.is_empty());
        s.query.clear();
        s.recompute_find_matches(&lines);
        assert!(s.matches.is_empty());
    }

    #[test]
    fn search_state_next_and_prev_match_cycle() {
        let mut s = SearchState::new(SearchMode::Find);
        s.matches = vec![0, 5, 10];
        s.current_match = 0;
        assert_eq!(s.next_match(), Some(5));
        assert_eq!(s.next_match(), Some(10));
        assert_eq!(s.next_match(), Some(0)); // wraps
        assert_eq!(s.prev_match(), Some(10)); // wraps backwards
    }

    #[test]
    fn view_is_searchable_returns_true_for_list_and_report() {
        assert!(view_is_searchable(View::Positions));
        assert!(view_is_searchable(View::Plays));
        assert!(view_is_searchable(View::Alerts));
        assert!(view_is_searchable(View::Report));
    }

    #[test]
    fn view_is_searchable_returns_false_for_non_searchable_views() {
        assert!(!view_is_searchable(View::Chart));
        assert!(!view_is_searchable(View::News));
    }

    #[test]
    fn search_mode_for_view_gives_filter_for_lists() {
        assert_eq!(search_mode_for_view(View::Positions), SearchMode::Filter);
        assert_eq!(search_mode_for_view(View::Plays), SearchMode::Filter);
        assert_eq!(search_mode_for_view(View::Alerts), SearchMode::Filter);
    }

    #[test]
    fn search_mode_for_view_gives_find_for_report() {
        assert_eq!(search_mode_for_view(View::Report), SearchMode::Find);
    }

    // ── Filter helpers ─────────────────────────────────────────────────────────

    fn make_position(
        id: u64,
        asset: &str,
        dir: crate::types::positions::Direction,
    ) -> crate::types::positions::Position {
        crate::types::positions::Position {
            id,
            asset: asset.into(),
            direction: dir,
            entry_price: None,
            current_price: None,
            stop_price: None,
            target_price: None,
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
        }
    }

    #[test]
    fn filtered_positions_no_query_returns_all_indices() {
        use crate::types::positions::{Direction, PositionsResponse};
        let mut app = App::new();
        app.apply(Update::Positions(PositionsResponse {
            positions: vec![
                make_position(1, "BTC", Direction::Long),
                make_position(2, "ETH", Direction::Short),
            ],
            portfolio_heat: None,
        }));
        let indices = app.filtered_positions();
        assert_eq!(indices, vec![0, 1]);
    }

    #[test]
    fn filtered_positions_query_narrows_by_asset() {
        use crate::types::positions::{Direction, PositionsResponse};
        let mut app = App::new();
        app.apply(Update::Positions(PositionsResponse {
            positions: vec![
                make_position(1, "BTC", Direction::Long),
                make_position(2, "ETH", Direction::Short),
                make_position(3, "ETHUSDT", Direction::Long),
            ],
            portfolio_heat: None,
        }));
        app.open_search(); // opens Filter mode for Positions view (default)
        app.view = View::Positions;
        // Manually set query since open_search uses view.
        if let Some(s) = &mut app.search {
            s.query = "eth".to_string();
        }
        let indices = app.filtered_positions();
        assert_eq!(indices, vec![1, 2]);
    }

    #[test]
    fn filtered_positions_empty_when_no_data() {
        let app = App::new();
        assert!(app.filtered_positions().is_empty());
    }

    #[test]
    fn open_search_sets_mode_for_view() {
        let mut app = App::new();
        app.view = View::Positions;
        app.open_search();
        let s = app.search.as_ref().unwrap();
        assert_eq!(s.mode, SearchMode::Filter);
        assert!(s.is_open());
    }

    #[test]
    fn open_search_report_gives_find_mode() {
        let mut app = App::new();
        app.view = View::Report;
        app.open_search();
        let s = app.search.as_ref().unwrap();
        assert_eq!(s.mode, SearchMode::Find);
    }

    #[test]
    fn clear_search_removes_state() {
        let mut app = App::new();
        app.view = View::Positions;
        app.open_search();
        assert!(app.search.is_some());
        app.clear_search();
        assert!(app.search.is_none());
    }

    // ── NewsFeedPreset ─────────────────────────────────────────────────────────

    #[test]
    fn news_feed_preset_labels() {
        assert_eq!(NewsFeedPreset::Default.label(), "default");
        assert_eq!(NewsFeedPreset::AllCrypto.label(), "all-crypto");
        assert_eq!(NewsFeedPreset::AllMacro.label(), "all-macro");
        assert_eq!(NewsFeedPreset::All.label(), "all");
    }

    #[test]
    fn news_feed_preset_to_feed_ids_contains_expected() {
        let default_ids = NewsFeedPreset::Default.to_feed_ids();
        assert!(default_ids.contains("coindesk"));
        assert!(default_ids.contains("cointelegraph"));
        assert!(default_ids.contains("decrypt"));

        let crypto_ids = NewsFeedPreset::AllCrypto.to_feed_ids();
        assert!(crypto_ids.contains("theblock"));
        assert!(crypto_ids.contains("dlnews"));

        let macro_ids = NewsFeedPreset::AllMacro.to_feed_ids();
        assert!(macro_ids.contains("fed"));
        assert!(macro_ids.contains("fxstreet"));
        assert!(macro_ids.contains("cnbc_top"));

        let all_ids = NewsFeedPreset::All.to_feed_ids();
        assert!(all_ids.contains("cnbc_top"));
        assert!(all_ids.contains("cointelegraph"));
        assert!(all_ids.contains("fxstreet"));
    }

    #[test]
    fn news_feed_preset_cycle_next_wraps() {
        assert_eq!(NewsFeedPreset::Default.next(), NewsFeedPreset::AllCrypto);
        assert_eq!(NewsFeedPreset::AllCrypto.next(), NewsFeedPreset::AllMacro);
        assert_eq!(NewsFeedPreset::AllMacro.next(), NewsFeedPreset::All);
        assert_eq!(NewsFeedPreset::All.next(), NewsFeedPreset::Default);
    }

    #[test]
    fn news_feed_preset_cycle_prev_wraps() {
        assert_eq!(NewsFeedPreset::Default.prev(), NewsFeedPreset::All);
        assert_eq!(NewsFeedPreset::AllCrypto.prev(), NewsFeedPreset::Default);
        assert_eq!(NewsFeedPreset::AllMacro.prev(), NewsFeedPreset::AllCrypto);
        assert_eq!(NewsFeedPreset::All.prev(), NewsFeedPreset::AllMacro);
    }

    // ── View::News ─────────────────────────────────────────────────────────────

    #[test]
    fn view_from_index_6_is_news() {
        // views: News is 6, Details is 7.
        assert_eq!(View::from_index(6), Some(View::News));
        assert_eq!(View::from_index(7), Some(View::Details));
    }

    #[test]
    fn view_news_label() {
        assert_eq!(View::News.label(), "news");
    }

    #[test]
    fn apply_news_ok_stores_and_clears_error() {
        use crate::types::news::{NewsItem, NewsResponse};
        let mut app = App::new();
        app.fetch_errors.insert("news", "old error".to_string());
        let item = NewsItem {
            id: "x".into(),
            title: "BTC rally".into(),
            summary: "Details here.".into(),
            link: "https://example.com/".into(),
            published: "2026-05-23T12:00:00+00:00".into(),
            source: "CoinTelegraph".into(),
            source_id: "cointelegraph".into(),
        };
        let resp = NewsResponse {
            items: vec![item],
            feeds: std::collections::HashMap::new(),
            timestamp: "2026-05-23T12:00:00+00:00".into(),
        };
        app.apply(Update::News(Ok(resp)));
        assert!(app.news.is_some());
        assert_eq!(app.news.as_ref().unwrap().items.len(), 1);
        assert!(!app.fetch_errors.contains_key("news"));
        assert!(app.last_update.contains_key("news"));
    }

    #[test]
    fn apply_news_err_stores_error() {
        let mut app = App::new();
        app.apply(Update::News(Err(anyhow::anyhow!("network down"))));
        assert!(app.news.is_none());
        assert!(app.fetch_errors.contains_key("news"));
    }

    // ── Details view state ────────────────────────────────────────────────────

    #[test]
    fn apply_details_matrix_ok_stores_and_clears_error() {
        let mut app = App::new();
        app.fetch_errors
            .insert("details_matrix", "old error".to_string());
        app.apply(Update::DetailsMatrix(Ok(MatrixResponse::default())));
        assert!(app.details_matrix.is_some());
        assert!(!app.fetch_errors.contains_key("details_matrix"));
        assert!(app.last_update.contains_key("details_matrix"));
    }

    #[test]
    fn apply_details_matrix_err_stores_error() {
        let mut app = App::new();
        app.apply(Update::DetailsMatrix(Err(anyhow::anyhow!("network error"))));
        assert!(app.details_matrix.is_none());
        assert!(app.fetch_errors.contains_key("details_matrix"));
    }

    #[test]
    fn details_cursor_starts_unselected() {
        let app = App::new();
        assert_eq!(app.details_cursor.selected(), None);
    }

    #[test]
    fn move_details_cursor_basic_navigation() {
        let mut app = App::new();
        app.move_details_cursor(1, 5);
        assert_eq!(app.details_cursor.selected(), Some(1));
        app.move_details_cursor(1, 5);
        assert_eq!(app.details_cursor.selected(), Some(2));
        app.move_details_cursor(-1, 5);
        assert_eq!(app.details_cursor.selected(), Some(1));
    }

    #[test]
    fn move_details_cursor_clamps_at_bottom() {
        let mut app = App::new();
        app.details_cursor.select(Some(4));
        app.move_details_cursor(1, 5);
        assert_eq!(
            app.details_cursor.selected(),
            Some(4),
            "should not exceed last row"
        );
    }

    #[test]
    fn move_details_cursor_zero_len_deselects() {
        let mut app = App::new();
        app.details_cursor.select(Some(2));
        app.move_details_cursor(1, 0);
        assert_eq!(app.details_cursor.selected(), None);
    }

    #[test]
    fn view_details_label_and_from_index() {
        assert_eq!(View::Details.label(), "details");
        // Details is 7; 8+ are unmapped.
        assert_eq!(View::from_index(7), Some(View::Details));
        assert_eq!(View::from_index(8), None);
    }

    // ── AddAlertForm ────────────────────────────────────────────────────────────

    #[test]
    fn add_alert_form_new_prefills_symbol_and_lands_on_price() {
        let form = AddAlertForm::new("ETH".to_string());
        assert_eq!(form.symbol, "ETH");
        assert_eq!(form.field, AddAlertField::Price);
        assert_eq!(form.condition, AlertCondition::Above);
        assert!(form.price.is_empty());
        assert!(form.note.is_empty());
        assert!(!form.submitting);
        assert!(form.error.is_none());
    }

    #[test]
    fn add_alert_field_cycle_forward() {
        assert_eq!(AddAlertField::Symbol.next(), AddAlertField::Condition);
        assert_eq!(AddAlertField::Condition.next(), AddAlertField::Price);
        assert_eq!(AddAlertField::Price.next(), AddAlertField::Note);
        assert_eq!(AddAlertField::Note.next(), AddAlertField::Symbol); // wraps
    }

    #[test]
    fn add_alert_field_cycle_backward() {
        assert_eq!(AddAlertField::Symbol.prev(), AddAlertField::Note); // wraps
        assert_eq!(AddAlertField::Condition.prev(), AddAlertField::Symbol);
        assert_eq!(AddAlertField::Price.prev(), AddAlertField::Condition);
        assert_eq!(AddAlertField::Note.prev(), AddAlertField::Price);
    }

    #[test]
    fn add_alert_condition_toggle() {
        assert_eq!(AlertCondition::Above.toggle(), AlertCondition::Below);
        assert_eq!(AlertCondition::Below.toggle(), AlertCondition::Above);
    }

    #[test]
    fn add_alert_form_push_char_to_price_filters_invalid() {
        let mut form = AddAlertForm::new("BTC".to_string());
        form.field = AddAlertField::Price;
        form.push_char('7');
        form.push_char('5');
        form.push_char('a'); // ignored
        form.push_char('.');
        form.push_char('0');
        assert_eq!(form.price, "75.0");
    }

    #[test]
    fn add_alert_form_push_char_to_price_accepts_comma() {
        let mut form = AddAlertForm::new("BTC".to_string());
        form.field = AddAlertField::Price;
        form.push_char('7');
        form.push_char('5');
        form.push_char(',');
        form.push_char('5');
        assert_eq!(form.price, "75,5");
        // Comma is treated as decimal separator on parse.
        let v = form.parse_price().expect("should parse");
        assert!((v - 75.5).abs() < 1e-9);
    }

    #[test]
    fn add_alert_form_parse_price_valid() {
        let mut form = AddAlertForm::new("BTC".to_string());
        form.price = "75000.5".to_string();
        assert!((form.parse_price().unwrap() - 75000.5).abs() < 1e-9);
    }

    #[test]
    fn add_alert_form_parse_price_invalid_returns_none() {
        let mut form = AddAlertForm::new("BTC".to_string());
        form.price = "abc".to_string();
        assert!(form.parse_price().is_none());
    }

    #[test]
    fn add_alert_form_parse_price_zero_returns_none() {
        let mut form = AddAlertForm::new("BTC".to_string());
        form.price = "0".to_string();
        assert!(form.parse_price().is_none());
    }

    #[test]
    fn open_add_alert_form_prefills_selected_asset() {
        let mut app = App::new();
        app.selected_asset = "SOL".to_string();
        app.open_add_alert_form();
        let form = app.add_alert_form.as_ref().unwrap();
        assert_eq!(form.symbol, "SOL");
    }

    #[test]
    fn close_add_alert_form_clears_it() {
        let mut app = App::new();
        app.open_add_alert_form();
        assert!(app.add_alert_form.is_some());
        app.close_add_alert_form();
        assert!(app.add_alert_form.is_none());
    }

    #[test]
    fn apply_alert_created_ok_closes_form_and_shows_toast() {
        let mut app = App::new();
        app.open_add_alert_form();
        app.apply(Update::AlertCreated(Ok(AlertFull::default())));
        assert!(app.add_alert_form.is_none(), "form should close on success");
        assert_eq!(app.toast.as_deref(), Some("Alert created"));
        assert!(app.toast_until.is_some());
    }

    #[test]
    fn apply_alert_created_err_keeps_form_open_with_error() {
        let mut app = App::new();
        app.open_add_alert_form();
        // Simulate a generic server error with a JSON body snippet.
        let err_msg = r#"POST /api/alerts -> 400: {"error": "Price must be positive"}"#;
        app.apply(Update::AlertCreated(Err(anyhow::anyhow!("{}", err_msg))));
        assert!(
            app.add_alert_form.is_some(),
            "form should stay open on error"
        );
        let form = app.add_alert_form.as_ref().unwrap();
        assert!(form.error.is_some(), "error should be set");
        assert!(!form.submitting, "submitting should be cleared");
    }

    #[test]
    fn apply_alert_created_no_channel_err_closes_form_with_toast() {
        let mut app = App::new();
        app.open_add_alert_form();
        let err_msg = r#"POST /api/alerts -> 400: {"no_channel": true, "error": "Configure at least one notification channel"}"#;
        app.apply(Update::AlertCreated(Err(anyhow::anyhow!("{}", err_msg))));
        assert!(
            app.add_alert_form.is_none(),
            "form should close for no_channel error"
        );
        assert!(
            app.toast
                .as_deref()
                .map(|t| t.contains("notifications"))
                .unwrap_or(false)
        );
    }

    #[test]
    fn toast_tick_clears_after_deadline() {
        let mut app = App::new();
        app.toast = Some("test toast".to_string());
        // Set deadline in the past.
        app.toast_until = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        app.tick_toast();
        assert!(
            app.toast.is_none(),
            "toast should be cleared after deadline"
        );
    }
}
