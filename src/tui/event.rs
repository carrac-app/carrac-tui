use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::app::state::{
    AddAlertField, App, BriefReportChoice, PositionFilter, SearchMode, View, view_is_searchable,
};
use crate::config::AgentConfig;
use crate::tui::widgets::chart::{self, ChartAction};

/// Return value from `handle_input`.
pub enum Action {
    /// Nothing special; the caller keeps running.
    Continue,
    /// Signal the fetchers to refresh immediately.
    ForceRefresh,
    /// Refetch the chart with the current symbol+tf (chart fetcher).
    RefreshChart,
    /// The user navigated to the brief view for the first time (or again).
    /// The event loop uses this to lazily wake the brief fetchers.
    /// The user navigated to the report view for the first time (or again).
    /// The event loop uses this to lazily wake the report fetchers.
    ReportVisited,
    /// The user navigated to the news view for the first time (or again).
    NewsVisited,
    /// The user changed the news feed preset — trigger an immediate refetch.
    RefreshNews,
    /// The user navigated to the details view for the first time (or again).
    DetailsVisited,
    /// User pressed Enter/l in Details view on a row: jump to chart for that symbol.
    JumpToChart { symbol: String },
    /// User submitted the add-alert form with valid data.
    SubmitAlert {
        symbol: String,
        condition: String,
        price: f64,
        note: String,
    },
    /// User pressed P: render PNG chart for asset+tf and display via kitten icat.
    ShowChartPng { asset: String, tf: String },
    /// User pressed A: launch their own AI agent with a context bundle.
    /// The event loop writes the bundle then suspends the TUI and spawns the agent.
    LaunchAgent { agent: AgentConfig },
    /// User pressed Enter in the asset selector with a typed query — fire a
    /// server symbol-lookup search for the current filter text.
    TriggerSymbolSearch(String),
}

const TFS: [&str; 6] = ["5m", "15m", "1H", "4H", "1D", "1W"];

/// Dispatch a key event to the app state.
///
/// Routing precedence:
/// 1. Asset selector overlay (if open) consumes everything.
/// 2. Help overlay (if open) consumes everything.
/// 3. Search prompt (if open) consumes everything.
/// 4. Global keys: `q`, `r`, `?`, `a`, `A`, `1..8`, `/`.
/// 5. View-local keys (chart pan/zoom/TF, positions filter, etc.).
pub fn handle_input(app: &mut App, key: KeyEvent) -> Action {
    // ── add-alert form (highest priority — consumes all keys while open) ──
    if app.add_alert_form.is_some() {
        return handle_add_alert_form(app, key);
    }

    // ── detail overlay (Enter-opened position/play/alert card) ────────────
    if app.detail_overlay.is_some() {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                app.detail_scroll = app.detail_scroll.saturating_add(1);
                return Action::Continue;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                app.detail_scroll = app.detail_scroll.saturating_sub(1);
                return Action::Continue;
            }
            KeyCode::PageDown => {
                app.detail_scroll = app.detail_scroll.saturating_add(5);
                return Action::Continue;
            }
            KeyCode::PageUp => {
                app.detail_scroll = app.detail_scroll.saturating_sub(5);
                return Action::Continue;
            }
            // h → dismiss detail overlay (vim-style: h=left=go back).
            KeyCode::Char('h') => {
                app.close_detail();
                return Action::Continue;
            }
            _ => {
                // Any other key dismisses, matches help overlay UX.
                app.close_detail();
                return Action::Continue;
            }
        }
    }

    // ── asset selector overlay ────────────────────────────────────────────
    if app.asset_selector.is_some() {
        return handle_asset_selector(app, key);
    }

    // ── help overlay ───────────────────────────────────────────────────────
    if app.show_help {
        // Scroll inside the help with j/k/PgUp/PgDn/arrows; any other key
        // closes the overlay.
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                app.help_scroll = app.help_scroll.saturating_add(1);
                return Action::Continue;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                app.help_scroll = app.help_scroll.saturating_sub(1);
                return Action::Continue;
            }
            KeyCode::PageDown => {
                app.help_scroll = app.help_scroll.saturating_add(5);
                return Action::Continue;
            }
            KeyCode::PageUp => {
                app.help_scroll = app.help_scroll.saturating_sub(5);
                return Action::Continue;
            }
            _ => {
                app.show_help = false;
                return Action::Continue;
            }
        }
    }

    // ── Tab / Shift+Tab — universal view cycle ───────────────────────────────
    //
    // The search prompt intercept at the next branch is BELOW this block;
    // Tab while the search prompt is open falls through its `_ => {}` arm
    // and returns Continue (no view cycling during active text search).
    if key.code == KeyCode::Tab {
        let next = app.view.cycle_next();
        if next != app.view {
            app.clear_search();
        }
        if next == View::Report {
            app.view = next;
            return Action::ReportVisited;
        } else if next == View::News {
            app.view = next;
            return Action::NewsVisited;
        } else if next == View::Details {
            app.view = next;
            return Action::DetailsVisited;
        }
        app.view = next;
        return Action::Continue;
    }
    if key.code == KeyCode::BackTab {
        let prev = app.view.cycle_prev();
        if prev != app.view {
            app.clear_search();
        }
        if prev == View::Report {
            app.view = prev;
            return Action::ReportVisited;
        } else if prev == View::News {
            app.view = prev;
            return Action::NewsVisited;
        } else if prev == View::Details {
            app.view = prev;
            return Action::DetailsVisited;
        }
        app.view = prev;
        return Action::Continue;
    }

    // ── search prompt (open) — consumes all keys ──────────────────────────
    if app
        .search
        .as_ref()
        .is_some_and(super::super::app::state::SearchState::is_open)
    {
        return handle_search_prompt_keys(app, key);
    }

    // ── global keys ────────────────────────────────────────────────────────
    match key.code {
        KeyCode::Char('q') => {
            app.should_quit = true;
            return Action::Continue;
        }
        KeyCode::Char('r') => {
            return Action::ForceRefresh;
        }
        KeyCode::Char('?') => {
            app.show_help = true;
            app.help_scroll = 0;
            return Action::Continue;
        }
        KeyCode::Char('a') => {
            app.open_asset_selector();
            return Action::Continue;
        }
        // I (capital) — launch the user's own AI agent ("Intelligence") with current context.
        KeyCode::Char('I') => {
            return Action::LaunchAgent {
                agent: app.agent_config.clone(),
            };
        }
        // Cycle favourites (matches web UI convention). Triggers chart re-fetch.
        KeyCode::Char(',') => {
            if app.cycle_favourite(-1) {
                return Action::RefreshChart;
            }
            return Action::Continue;
        }
        KeyCode::Char('.') => {
            if app.cycle_favourite(1) {
                return Action::RefreshChart;
            }
            return Action::Continue;
        }
        KeyCode::Char(c @ '1'..='9') => {
            let digit = c.to_digit(10).unwrap() as usize;
            if let Some(v) = View::from_index(digit) {
                // Clear search when switching views.
                if v != app.view {
                    app.clear_search();
                }
                return switch_to_view(app, v);
            }
            return Action::Continue;
        }
        // Space → cycle tabs.
        KeyCode::Char(' ') => {
            let next = app.view.cycle_next();
            if next != app.view {
                app.clear_search();
            }
            if next == View::Report {
                app.view = next;
                return Action::ReportVisited;
            } else if next == View::News {
                app.view = next;
                return Action::NewsVisited;
            } else if next == View::Details {
                app.view = next;
                return Action::DetailsVisited;
            }
            app.view = next;
            return Action::Continue;
        }
        // `/` opens search prompt in searchable views; no-op elsewhere.
        KeyCode::Char('/') => {
            if view_is_searchable(app.view) {
                app.open_search();
                return Action::Continue;
            }
            return Action::Continue;
        }
        // Esc (no prompt open): clear any active but closed query.
        KeyCode::Esc if app.search.is_some() => {
            app.clear_search();
            return Action::Continue;
        }
        // fall through to view-local handling
        _ => {}
    }

    // ── view-local keys ────────────────────────────────────────────────────
    match app.view {
        View::Chart => handle_chart_keys(app, key),
        View::Positions => {
            // Filter keys first, then fall through to cursor nav.
            match key.code {
                KeyCode::Char('f') => {
                    app.position_filter = PositionFilter::Futures;
                    Action::Continue
                }
                KeyCode::Char('s') => {
                    app.position_filter = PositionFilter::Spot;
                    Action::Continue
                }
                KeyCode::Char('b') => {
                    app.position_filter = PositionFilter::Both;
                    Action::Continue
                }
                _ => handle_list_cursor_keys(app, key),
            }
        }
        View::Plays => handle_list_cursor_keys(app, key),
        View::Alerts => handle_alerts_keys(app, key),
        View::Report => handle_report_keys(app, key),
        View::News => handle_news_keys(app, key),
        View::Details => handle_details_keys(app, key),
    }
}

/// Alerts view local key handler. Handles `A` to open the create-alert form;
/// falls through to shared list-cursor navigation for everything else.
fn handle_alerts_keys(app: &mut App, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Char('A') => {
            app.open_add_alert_form();
            Action::Continue
        }
        _ => handle_list_cursor_keys(app, key),
    }
}

/// Handle keys while the add-alert form is open.
fn handle_add_alert_form(app: &mut App, key: KeyEvent) -> Action {
    // Shift+Tab: backward field cycle.
    if key.code == KeyCode::BackTab
        || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT))
    {
        if let Some(form) = &mut app.add_alert_form {
            form.field = form.field.prev();
        }
        return Action::Continue;
    }

    match key.code {
        KeyCode::Esc => {
            app.close_add_alert_form();
            Action::Continue
        }
        KeyCode::Tab => {
            if let Some(form) = &mut app.add_alert_form {
                form.field = form.field.next();
            }
            Action::Continue
        }
        KeyCode::Left | KeyCode::Char('h') => {
            if let Some(form) = &mut app.add_alert_form
                && form.field == AddAlertField::Condition
            {
                form.condition = form.condition.toggle();
            }
            Action::Continue
        }
        KeyCode::Right | KeyCode::Char('l') => {
            if let Some(form) = &mut app.add_alert_form
                && form.field == AddAlertField::Condition
            {
                form.condition = form.condition.toggle();
            }
            Action::Continue
        }
        KeyCode::Backspace => {
            if let Some(form) = &mut app.add_alert_form {
                form.pop_char();
            }
            Action::Continue
        }
        KeyCode::Enter => {
            // Validate, then emit SubmitAlert or set an error.
            let action = {
                if let Some(form) = &app.add_alert_form {
                    if form.symbol.trim().is_empty() {
                        None // will set error below
                    } else {
                        match form.parse_price() {
                            Some(price) => {
                                let symbol = form.symbol.trim().to_uppercase();
                                let condition = form.condition.label().to_string();
                                let note = form.note.clone();
                                Some(Action::SubmitAlert {
                                    symbol,
                                    condition,
                                    price,
                                    note,
                                })
                            }
                            None => None, // will set error below
                        }
                    }
                } else {
                    return Action::Continue;
                }
            };

            if let Some(a) = action {
                // Mark as submitting.
                if let Some(form) = &mut app.add_alert_form {
                    form.submitting = true;
                    form.error = None;
                }
                a
            } else {
                // Set validation error.
                if let Some(form) = &mut app.add_alert_form {
                    if form.symbol.trim().is_empty() {
                        form.error = Some("Symbol is required".to_string());
                    } else {
                        form.error = Some("Invalid price".to_string());
                    }
                }
                Action::Continue
            }
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(form) = &mut app.add_alert_form
                && !form.submitting
            {
                form.push_char(c);
                form.error = None;
            }
            Action::Continue
        }
        _ => Action::Continue,
    }
}

/// Cursor navigation shared by positions / plays / alerts.
fn handle_list_cursor_keys(app: &mut App, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            app.move_view_cursor(1);
            Action::Continue
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.move_view_cursor(-1);
            Action::Continue
        }
        // Enter or l → open detail overlay for focused row (vim-style: l=right=go deeper).
        KeyCode::Enter | KeyCode::Char('l') => {
            app.open_detail_for_focused();
            Action::Continue
        }
        _ => Action::Continue,
    }
}

fn handle_chart_keys(app: &mut App, key: KeyEvent) -> Action {
    // TF cycle on `t` (next) / `T` (previous). `,` / `.` are reserved
    // globally for favourite-cycle; chart never sees them here.
    match key.code {
        KeyCode::Char('t') => {
            cycle_tf(app, 1);
            return Action::RefreshChart;
        }
        KeyCode::Char('T') => {
            cycle_tf(app, -1);
            return Action::RefreshChart;
        }
        // P — render PNG for current asset at current TF, display via kitten icat.
        KeyCode::Char('P') => {
            return Action::ShowChartPng {
                asset: app.selected_asset.clone(),
                tf: app.chart.tf.clone(),
            };
        }
        _ => {}
    }

    // Everything else: hand to the chart widget's key table (pan/zoom/toggles).
    let action = match chart::handle_key(&mut app.chart, key) {
        ChartAction::Consumed => Action::Continue,
        ChartAction::RetimeframeFetch => Action::RefreshChart,
        ChartAction::Ignored => Action::Continue,
    };

    // `z` toggles the market-session strip, which the server only populates on
    // intraday timeframes (up to 4H). When it's switched on with no session data
    // (daily/weekly), toast so the key isn't a silent no-op.
    if key.code == KeyCode::Char('z')
        && app.chart.show_sessions
        && app
            .chart_data
            .as_ref()
            .is_none_or(|d| d.sessions.is_empty())
    {
        app.toast = Some("market sessions: intraday only (up to 4H)".to_string());
        app.toast_until = Some(std::time::Instant::now() + std::time::Duration::from_secs(3));
        app.toast_kind = crate::app::state::ToastKind::Info;
    }

    action
}

fn cycle_tf(app: &mut App, dir: i32) {
    let cur = TFS.iter().position(|t| *t == app.chart.tf).unwrap_or(1);
    let n = TFS.len() as i32;
    let next = ((cur as i32 + dir).rem_euclid(n)) as usize;
    // set_tf resets pan and applies the TF's default bar count.
    app.chart.set_tf(TFS[next]);
}

fn handle_asset_selector(app: &mut App, key: KeyEvent) -> Action {
    // Use the combined list (favourites + server search results) so the cursor
    // and Enter-selection work uniformly across both sections.
    let combined = app.asset_selector_combined();
    let sel = app.asset_selector.as_mut().expect("guarded by caller");

    match key.code {
        KeyCode::Esc => {
            app.close_asset_selector();
            Action::Continue
        }
        KeyCode::Enter => {
            // If the cursor lands on a known item, select it.
            if let Some(sym) = combined.get(sel.cursor) {
                app.selected_asset = sym.clone();
                app.close_asset_selector();
                return Action::RefreshChart;
            }
            // Nothing under cursor but there is a typed query: fire a server search.
            // This also handles the case where the user typed a symbol not yet in
            // the combined list — the search result will populate on the next frame.
            let query = sel.filter.clone();
            if !query.trim().is_empty() {
                return Action::TriggerSymbolSearch(query);
            }
            app.close_asset_selector();
            Action::Continue
        }
        KeyCode::Up => {
            sel.cursor = sel.cursor.saturating_sub(1);
            Action::Continue
        }
        KeyCode::Down => {
            let max = combined.len().saturating_sub(1);
            sel.cursor = (sel.cursor + 1).min(max);
            Action::Continue
        }
        KeyCode::Backspace => {
            sel.filter.pop();
            sel.cursor = 0;
            Action::Continue
        }
        KeyCode::Char(c) => {
            // Accept letters/digits/space; reset cursor on every keystroke
            // since the filtered list rebuilds.
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                sel.filter.push(c);
                sel.cursor = 0;
            }
            Action::Continue
        }
        _ => Action::Continue,
    }
}

/// Handle keys when the search prompt is open.
///
/// Switch `app.view` to `v` and return the `Action` that signals any lazy-load
/// the destination view needs (Report / News / Details fetch on first visit).
/// Shared by the digit-key view-switch ladders in the main and search-prompt
/// key handlers so the mapping lives in exactly one place.
fn switch_to_view(app: &mut App, v: View) -> Action {
    app.view = v;
    match v {
        View::Report => Action::ReportVisited,
        View::News => Action::NewsVisited,
        View::Details => Action::DetailsVisited,
        _ => Action::Continue,
    }
}

/// The prompt is a 1-line text input that accumulates a query. On Enter the
/// prompt closes but the query stays active. On Esc the query is also cleared.
/// View-switch keys (1-7) close and clear the search, then switch the view.
fn handle_search_prompt_keys(app: &mut App, key: KeyEvent) -> Action {
    match key.code {
        // Esc → close prompt AND clear query.
        KeyCode::Esc => {
            app.clear_search();
        }
        // Enter → close prompt but keep query active.
        KeyCode::Enter => {
            if let Some(s) = &mut app.search {
                s.close_prompt();
            }
        }
        // Backspace → pop last char, recompute.
        KeyCode::Backspace => {
            if let Some(s) = &mut app.search {
                s.query.pop();
            }
            recompute_active_matches(app);
            clamp_search_cursors(app);
        }
        // Number keys 1-9: close+clear search and switch view.
        KeyCode::Char(c @ '1'..='9') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            let digit = c.to_digit(10).unwrap() as usize;
            if let Some(v) = View::from_index(digit) {
                app.clear_search();
                return switch_to_view(app, v);
            }
        }
        // Printable chars → append to query.
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(s) = &mut app.search {
                s.query.push(c);
            }
            recompute_active_matches(app);
            clamp_search_cursors(app);
        }
        _ => {}
    }
    Action::Continue
}

/// Recompute Find-mode match indices using the currently active report content.
fn recompute_active_matches(app: &mut App) {
    use crate::app::state::BriefReportChoice;
    let is_find = app
        .search
        .as_ref()
        .is_some_and(|s| s.mode == SearchMode::Find);
    if !is_find {
        return;
    }
    let content = match app.brief_report_choice {
        BriefReportChoice::Morning => app.report_morning.as_ref().and_then(|r| r.content.clone()),
        BriefReportChoice::Afternoon => app
            .report_afternoon
            .as_ref()
            .and_then(|r| r.content.clone()),
    };
    let lines = content
        .map(|c| collect_report_lines(&c))
        .unwrap_or_default();
    if let Some(s) = &mut app.search {
        s.recompute_find_matches(&lines);
    }
}

/// Clamp list-view cursors to the current filtered length so the cursor stays
/// in-bounds after a query change.
fn clamp_search_cursors(app: &mut App) {
    let pos_len = app.filtered_positions().len();
    let plays_len = app.filtered_plays().len();
    let alerts_len = app.filtered_alerts().len();
    clamp_cursor_to(&mut app.positions_cursor, pos_len);
    clamp_cursor_to(&mut app.plays_cursor, plays_len);
    clamp_cursor_to(&mut app.alerts_cursor, alerts_len);
}

fn clamp_cursor_to(state: &mut ratatui::widgets::ListState, len: usize) {
    if len == 0 {
        state.select(None);
    } else {
        let cur = state.selected().unwrap_or(0).min(len - 1);
        state.select(Some(cur));
    }
}

/// Collect the plain-text content of the active report as a Vec<String> for
/// Find-mode match indexing.
pub fn collect_report_lines(content: &str) -> Vec<String> {
    content
        .lines()
        .map(std::string::ToString::to_string)
        .collect()
}

/// News view local key handler.
fn handle_news_keys(app: &mut App, key: KeyEvent) -> Action {
    let item_count = app.news.as_ref().map_or(0, |n| n.items.len());
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            if item_count > 0 {
                let cur = app.news_cursor.selected().unwrap_or(0);
                let next = (cur + 1).min(item_count - 1);
                app.news_cursor.select(Some(next));
            }
            Action::Continue
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if item_count > 0 {
                let cur = app.news_cursor.selected().unwrap_or(0);
                let next = cur.saturating_sub(1);
                app.news_cursor.select(Some(next));
            }
            Action::Continue
        }
        KeyCode::PageDown => {
            if item_count > 0 {
                let cur = app.news_cursor.selected().unwrap_or(0);
                let next = (cur + 5).min(item_count - 1);
                app.news_cursor.select(Some(next));
            }
            Action::Continue
        }
        KeyCode::PageUp => {
            if item_count > 0 {
                let cur = app.news_cursor.selected().unwrap_or(0);
                let next = cur.saturating_sub(5);
                app.news_cursor.select(Some(next));
            }
            Action::Continue
        }
        KeyCode::Char('g') => {
            if item_count > 0 {
                app.news_cursor.select(Some(0));
            }
            Action::Continue
        }
        KeyCode::Char('G') => {
            if item_count > 0 {
                app.news_cursor.select(Some(item_count - 1));
            }
            Action::Continue
        }
        KeyCode::Enter | KeyCode::Char('l') => {
            if let Some(resp) = &app.news {
                let idx = app.news_cursor.selected().unwrap_or(0);
                if let Some(item) = resp.items.get(idx) {
                    open_link_in_browser(&item.link);
                }
            }
            Action::Continue
        }
        KeyCode::Char('[') => {
            app.news_feed_preset = app.news_feed_preset.prev();
            Action::RefreshNews
        }
        KeyCode::Char(']') => {
            app.news_feed_preset = app.news_feed_preset.next();
            Action::RefreshNews
        }
        _ => Action::Continue,
    }
}

/// Details view local key handler.
pub fn handle_details_keys(app: &mut App, key: KeyEvent) -> Action {
    let row_count = app
        .watchlist_symbols
        .len()
        .min(crate::tui::widgets::view_details::BATCH_CAP);

    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            app.move_details_cursor(1, row_count);
            Action::Continue
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.move_details_cursor(-1, row_count);
            Action::Continue
        }
        KeyCode::PageDown => {
            app.move_details_cursor(5, row_count);
            Action::Continue
        }
        KeyCode::PageUp => {
            app.move_details_cursor(-5, row_count);
            Action::Continue
        }
        KeyCode::Char('g') => {
            if row_count > 0 {
                app.details_cursor.select(Some(0));
            }
            Action::Continue
        }
        KeyCode::Char('G') => {
            if row_count > 0 {
                app.details_cursor.select(Some(row_count - 1));
            }
            Action::Continue
        }
        KeyCode::Enter | KeyCode::Char('l') => {
            let idx = app.details_cursor.selected().unwrap_or(0);
            if let Some(sym) = app.watchlist_symbols.get(idx).cloned() {
                Action::JumpToChart { symbol: sym }
            } else {
                Action::Continue
            }
        }
        // P — render PNG for the cursor-row symbol at 1D TF, display via kitten icat.
        KeyCode::Char('P') => {
            let idx = app.details_cursor.selected().unwrap_or(0);
            if let Some(sym) = app.watchlist_symbols.get(idx).cloned() {
                Action::ShowChartPng {
                    asset: sym,
                    tf: "1D".to_string(),
                }
            } else {
                Action::Continue
            }
        }
        _ => Action::Continue,
    }
}

/// Open `url` with the OS default handler (Linux `$BROWSER` then `xdg-open`,
/// macOS `open`, Windows `cmd /c start`). Failures are silent.
fn open_link_in_browser(url: &str) {
    crate::helpers::open::open_external(url);
}

/// Report view local key handler.
///
/// `[`/`]` toggle morning/afternoon report.
/// `j`/`k`/`Down`/`Up` scroll by 1 line.
/// `PgDn`/`PgUp` scroll by 5 lines.
/// `g` jump to top.
/// `G` jump to bottom (increments scroll by a large value; the renderer clamps).
/// `n`/`N` jump to next/previous search match (when a Find-mode query is active).
/// Filename for a saved report, e.g. `carrac-morning-2025-01-15.md`. Uses the
/// report's `created_at` date when available, else a stable `report` fallback.
pub(crate) fn report_filename(choice: BriefReportChoice, created_at: Option<&str>) -> String {
    let label = match choice {
        BriefReportChoice::Morning => "morning",
        BriefReportChoice::Afternoon => "afternoon",
    };
    let date = created_at
        .and_then(|ts| ts.get(0..10)) // "2025-01-15T…" → "2025-01-15"
        .filter(|d| d.as_bytes().get(4) == Some(&b'-'))
        .unwrap_or("report");
    format!("carrac-{label}-{date}.md")
}

/// Write `content` to `dir/filename`, returning the full path on success.
fn save_report(
    content: &str,
    dir: &std::path::Path,
    filename: &str,
) -> std::io::Result<std::path::PathBuf> {
    let path = dir.join(filename);
    std::fs::write(&path, content)?;
    Ok(path)
}

/// Set a brief auto-dismissing status toast.
fn report_toast(app: &mut App, msg: &str) {
    app.toast = Some(msg.to_string());
    app.toast_until = Some(std::time::Instant::now() + std::time::Duration::from_secs(3));
}

/// Save the currently-selected report (morning/afternoon) as a `.md` file in the
/// working directory and toast the result. No-op (with a toast) if empty.
fn save_current_report(app: &mut App) {
    let choice = app.brief_report_choice;
    let report = match choice {
        BriefReportChoice::Morning => app.report_morning.as_ref(),
        BriefReportChoice::Afternoon => app.report_afternoon.as_ref(),
    };
    let content = report.and_then(|r| r.content.clone());
    let created = report.and_then(|r| r.created_at.clone());

    let Some(content) = content else {
        report_toast(app, "no report to save yet");
        return;
    };
    let filename = report_filename(choice, created.as_deref());
    let dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    match save_report(&content, &dir, &filename) {
        Ok(path) => report_toast(app, &format!("saved → {}", path.display())),
        Err(e) => report_toast(app, &format!("save failed: {e}")),
    }
}

/// Convert a Find-match *source-line* index into the wrapped *display-row*
/// scroll offset that `Paragraph::scroll` actually consumes. The two differ
/// whenever lines above the match wrap at the current body width — using the
/// raw source index landed the viewport short of the match. Leaves 2 rows of
/// context above the match. Falls back to the raw index before first render
/// (body width unknown).
fn report_match_scroll(app: &App, line_idx: usize) -> u16 {
    let content = match app.brief_report_choice {
        BriefReportChoice::Morning => app.report_morning.as_ref(),
        BriefReportChoice::Afternoon => app.report_afternoon.as_ref(),
    }
    .and_then(|r| r.content.as_deref())
    .unwrap_or("");
    if app.report_body_width == 0 || content.is_empty() {
        return line_idx.min(u16::MAX as usize) as u16;
    }
    crate::tui::widgets::view_report::report_scroll_target(content, line_idx, app.report_body_width)
        .saturating_sub(2)
}

fn handle_report_keys(app: &mut App, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Char('[') => {
            app.brief_report_choice = match app.brief_report_choice {
                BriefReportChoice::Morning => BriefReportChoice::Afternoon,
                BriefReportChoice::Afternoon => BriefReportChoice::Morning,
            };
            // Jump to the top — keeping the old scroll offset on the other
            // report made the toggle look like a no-op mid-document.
            app.report_scroll = 0;
            // Recompute matches for the newly selected report.
            recompute_report_matches(app);
            Action::Continue
        }
        KeyCode::Char(']') => {
            app.brief_report_choice = match app.brief_report_choice {
                BriefReportChoice::Morning => BriefReportChoice::Afternoon,
                BriefReportChoice::Afternoon => BriefReportChoice::Morning,
            };
            app.report_scroll = 0;
            recompute_report_matches(app);
            Action::Continue
        }
        // w — write the current report's raw markdown to a .md file in the
        // working directory, so it can be read/kept in any external viewer.
        KeyCode::Char('w') => {
            save_current_report(app);
            Action::Continue
        }
        // n / N — navigate search matches (only when a Find query is active).
        KeyCode::Char('n') => {
            if let Some(line_idx) = app
                .search
                .as_mut()
                .and_then(super::super::app::state::SearchState::next_match)
            {
                app.report_scroll = report_match_scroll(app, line_idx);
            }
            Action::Continue
        }
        KeyCode::Char('N') => {
            if let Some(line_idx) = app
                .search
                .as_mut()
                .and_then(super::super::app::state::SearchState::prev_match)
            {
                app.report_scroll = report_match_scroll(app, line_idx);
            }
            Action::Continue
        }
        KeyCode::Char('j') | KeyCode::Down => {
            app.report_scroll = app.report_scroll.saturating_add(1);
            Action::Continue
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.report_scroll = app.report_scroll.saturating_sub(1);
            Action::Continue
        }
        KeyCode::PageDown => {
            app.report_scroll = app.report_scroll.saturating_add(5);
            Action::Continue
        }
        KeyCode::PageUp => {
            app.report_scroll = app.report_scroll.saturating_sub(5);
            Action::Continue
        }
        KeyCode::Char('g') => {
            app.report_scroll = 0;
            Action::Continue
        }
        KeyCode::Char('G') => {
            // Jump to bottom — use a large value; renderer will clamp.
            app.report_scroll = u16::MAX;
            Action::Continue
        }
        _ => Action::Continue,
    }
}

/// Re-run Find-mode match computation after the report choice toggles.
fn recompute_report_matches(app: &mut App) {
    use crate::app::state::BriefReportChoice;
    let content = match app.brief_report_choice {
        BriefReportChoice::Morning => app.report_morning.as_ref().and_then(|r| r.content.clone()),
        BriefReportChoice::Afternoon => app
            .report_afternoon
            .as_ref()
            .and_then(|r| r.content.clone()),
    };
    if let Some(s) = &mut app.search {
        let lines = content
            .map(|c| collect_report_lines(&c))
            .unwrap_or_default();
        s.recompute_find_matches(&lines);
    }
}

/// Mouse handler.
///
/// In Chart view, left-click inside the chart canvas pins the crosshair to
/// that column. Right-click clears it. Outside the chart area, mouse events
/// are ignored.
pub fn handle_mouse(app: &mut App, ev: MouseEvent) -> Action {
    // Overlays swallow mouse events (no actions inside the help / asset popup).
    if app.show_help || app.asset_selector.is_some() {
        return Action::Continue;
    }
    if app.view != View::Chart {
        return Action::Continue;
    }
    let Some(rect) = app.last_chart_rect else {
        return Action::Continue;
    };
    let x = ev.column;
    let y = ev.row;
    if x < rect.x || x >= rect.x + rect.width || y < rect.y || y >= rect.y + rect.height {
        return Action::Continue;
    }
    match ev.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            app.chart.crosshair_col = Some(x);
        }
        MouseEventKind::Down(MouseButton::Right) => {
            app.chart.crosshair_col = None;
        }
        _ => {}
    }
    Action::Continue
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn report_filename_uses_choice_and_date() {
        assert_eq!(
            report_filename(BriefReportChoice::Morning, Some("2025-01-15T10:30:00.000Z")),
            "carrac-morning-2025-01-15.md"
        );
        assert_eq!(
            report_filename(BriefReportChoice::Afternoon, Some("2025-01-15T14:00:00Z")),
            "carrac-afternoon-2025-01-15.md"
        );
        // No timestamp → stable fallback, still a valid filename.
        assert_eq!(
            report_filename(BriefReportChoice::Morning, None),
            "carrac-morning-report.md"
        );
    }

    #[test]
    fn save_report_writes_content_to_dir() {
        let dir = std::env::temp_dir().join(format!("carrac-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = save_report("# Hi\n\nbody", &dir, "carrac-morning-2025-01-15.md").unwrap();
        let read = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(read, "# Hi\n\nbody");
        assert!(path.ends_with("carrac-morning-2025-01-15.md"));
    }

    #[test]
    fn w_key_with_no_report_shows_toast_not_panic() {
        let mut app = App::new();
        app.view = View::Report;
        app.brief_report_choice = BriefReportChoice::Morning;
        assert!(app.report_morning.is_none());
        let _ = handle_input(&mut app, press(KeyCode::Char('w')));
        assert!(app.toast.is_some(), "saving with no report still toasts");
    }

    #[test]
    fn numbers_switch_views() {
        let mut app = App::new();
        let _ = handle_input(&mut app, press(KeyCode::Char('2')));
        assert_eq!(app.view, View::Positions);
        let _ = handle_input(&mut app, press(KeyCode::Char('1')));
        assert_eq!(app.view, View::Chart);
        let _ = handle_input(&mut app, press(KeyCode::Char('5')));
        assert_eq!(app.view, View::Report);
    }

    #[test]
    fn alt_jk_pans_y_in_chart_view_through_full_dispatch() {
        // Regression: Alt+j/k must survive the whole handle_input dispatch
        // (not just the widget key table) without being eaten by the plain
        // j/k zoom arms.
        let mut app = App::new();
        app.view = View::Chart;
        let alt = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT);
        let _ = handle_input(&mut app, alt('k'));
        assert!(
            app.chart.y_offset > 0.0,
            "Alt+k must pan up, got {}",
            app.chart.y_offset
        );
        assert_eq!(app.chart.y_zoom, 1.0, "Alt+k must not zoom");
        let _ = handle_input(&mut app, alt('j'));
        let _ = handle_input(&mut app, alt('j'));
        assert!(app.chart.y_offset < 0.0, "Alt+j must pan down");
    }

    #[test]
    fn tf_cycle_forward_backward() {
        let mut app = App::new();
        app.view = View::Chart;
        app.chart.tf = "4H".to_string();
        let _ = handle_input(&mut app, press(KeyCode::Char('t')));
        assert_eq!(app.chart.tf, "1D");
        let _ = handle_input(&mut app, press(KeyCode::Char('T')));
        assert_eq!(app.chart.tf, "4H");
    }

    #[test]
    fn tf_cycle_wraps() {
        let mut app = App::new();
        app.view = View::Chart;
        app.chart.tf = "1W".to_string();
        let _ = handle_input(&mut app, press(KeyCode::Char('t')));
        // TFS = [5m, 15m, 1H, 4H, 1D, 1W] → wraps from 1W to 5m.
        assert_eq!(app.chart.tf, "5m");
    }

    #[test]
    fn favourite_cycle_globally() {
        let mut app = App::new();
        app.watchlist_symbols = vec!["BTC".into(), "ETH".into(), "SOL".into()];
        app.selected_asset = "BTC".into();
        let action = handle_input(&mut app, press(KeyCode::Char('.')));
        assert_eq!(app.selected_asset, "ETH");
        assert!(matches!(action, Action::RefreshChart));
        // Prev with comma
        let _ = handle_input(&mut app, press(KeyCode::Char(',')));
        assert_eq!(app.selected_asset, "BTC");
        // Wrap back to last
        let _ = handle_input(&mut app, press(KeyCode::Char(',')));
        assert_eq!(app.selected_asset, "SOL");
    }

    #[test]
    fn favourite_cycle_outside_watchlist_starts_at_end() {
        let mut app = App::new();
        app.watchlist_symbols = vec!["BTC".into(), "ETH".into()];
        app.selected_asset = "DOGE".into(); // not in list
        let _ = handle_input(&mut app, press(KeyCode::Char('.')));
        assert_eq!(app.selected_asset, "BTC"); // dir +1 starts at index 0
    }

    #[test]
    fn asset_selector_opens_and_closes() {
        let mut app = App::new();
        let _ = handle_input(&mut app, press(KeyCode::Char('a')));
        assert!(app.asset_selector.is_some());
        let _ = handle_input(&mut app, press(KeyCode::Esc));
        assert!(app.asset_selector.is_none());
    }

    #[test]
    fn asset_selector_filter_typing() {
        let mut app = App::new();
        app.watchlist_symbols = vec!["BTC".into(), "ETH".into(), "SOL".into()];
        app.open_asset_selector();
        let _ = handle_input(&mut app, press(KeyCode::Char('e')));
        assert_eq!(app.asset_selector.as_ref().unwrap().filter, "e");
        let matches = app.asset_selector_matches();
        assert_eq!(matches, vec!["ETH"]);
    }

    #[test]
    fn asset_selector_enter_selects_and_refreshes() {
        let mut app = App::new();
        app.watchlist_symbols = vec!["BTC".into(), "ETH".into()];
        app.open_asset_selector();
        // cursor=0 → BTC
        let action = handle_input(&mut app, press(KeyCode::Enter));
        assert_eq!(app.selected_asset, "BTC");
        assert!(app.asset_selector.is_none());
        assert!(matches!(action, Action::RefreshChart));
    }

    #[test]
    fn help_toggles_and_any_key_closes() {
        let mut app = App::new();
        let _ = handle_input(&mut app, press(KeyCode::Char('?')));
        assert!(app.show_help);
        let _ = handle_input(&mut app, press(KeyCode::Char('q')));
        // q dismissed help, did not quit
        assert!(!app.show_help);
        assert!(!app.should_quit);
    }

    #[test]
    fn positions_filter_keys() {
        let mut app = App::new();
        app.view = View::Positions;
        let _ = handle_input(&mut app, press(KeyCode::Char('f')));
        assert_eq!(app.position_filter, PositionFilter::Futures);
        let _ = handle_input(&mut app, press(KeyCode::Char('s')));
        assert_eq!(app.position_filter, PositionFilter::Spot);
        let _ = handle_input(&mut app, press(KeyCode::Char('b')));
        assert_eq!(app.position_filter, PositionFilter::Both);
    }

    #[test]
    fn left_click_sets_crosshair_when_in_chart_rect() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut app = App::new();
        app.view = View::Chart;
        app.last_chart_rect = Some(ratatui::layout::Rect {
            x: 0,
            y: 2,
            width: 80,
            height: 20,
        });
        let ev = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 42,
            row: 10,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        let _ = handle_mouse(&mut app, ev);
        assert_eq!(app.chart.crosshair_col, Some(42));
    }

    #[test]
    fn click_outside_chart_rect_is_ignored() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut app = App::new();
        app.view = View::Chart;
        app.last_chart_rect = Some(ratatui::layout::Rect {
            x: 0,
            y: 2,
            width: 80,
            height: 20,
        });
        // Click in the menubar (row 0) — outside chart rect.
        let ev = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 42,
            row: 0,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        let _ = handle_mouse(&mut app, ev);
        assert!(app.chart.crosshair_col.is_none());
    }

    #[test]
    fn right_click_clears_crosshair() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut app = App::new();
        app.view = View::Chart;
        app.chart.crosshair_col = Some(50);
        app.last_chart_rect = Some(ratatui::layout::Rect {
            x: 0,
            y: 2,
            width: 80,
            height: 20,
        });
        let ev = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: 50,
            row: 10,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        let _ = handle_mouse(&mut app, ev);
        assert!(app.chart.crosshair_col.is_none());
    }

    #[test]
    fn help_scroll_keys_dont_close_overlay() {
        let mut app = App::new();
        app.show_help = true;
        let _ = handle_input(&mut app, press(KeyCode::Char('j')));
        assert!(app.show_help, "j should scroll, not close");
        assert_eq!(app.help_scroll, 1);
        let _ = handle_input(&mut app, press(KeyCode::Char('k')));
        assert_eq!(app.help_scroll, 0);
        // Other key dismisses.
        let _ = handle_input(&mut app, press(KeyCode::Char('q')));
        assert!(!app.show_help);
    }

    #[test]
    fn jk_moves_cursor_in_positions_view() {
        use crate::types::positions::{Direction, Position, PositionsResponse};
        let mut app = App::new();
        app.view = View::Positions;
        let mk = |id: u64, asset: &str| Position {
            id,
            asset: asset.into(),
            direction: Direction::Long,
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
        };
        app.apply(crate::app::state::Update::Positions(PositionsResponse {
            positions: vec![mk(1, "BTC"), mk(2, "ETH"), mk(3, "SOL")],
            portfolio_heat: None,
        }));
        // After data arrives, cursor lands at 0.
        assert_eq!(app.positions_cursor.selected(), Some(0));
        let _ = handle_input(&mut app, press(KeyCode::Char('j')));
        assert_eq!(app.positions_cursor.selected(), Some(1));
        let _ = handle_input(&mut app, press(KeyCode::Char('j')));
        assert_eq!(app.positions_cursor.selected(), Some(2));
        // j at the bottom clamps.
        let _ = handle_input(&mut app, press(KeyCode::Char('j')));
        assert_eq!(app.positions_cursor.selected(), Some(2));
        let _ = handle_input(&mut app, press(KeyCode::Char('k')));
        assert_eq!(app.positions_cursor.selected(), Some(1));
    }

    #[test]
    fn enter_opens_detail_overlay_in_positions_view() {
        use crate::app::state::{DetailOverlay, Update};
        use crate::types::positions::{Direction, Position, PositionsResponse};
        let mut app = App::new();
        app.view = View::Positions;
        app.apply(Update::Positions(PositionsResponse {
            positions: vec![Position {
                id: 42,
                asset: "BTC".into(),
                direction: Direction::Long,
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
            }],
            portfolio_heat: None,
        }));
        let _ = handle_input(&mut app, press(KeyCode::Enter));
        assert!(matches!(
            app.detail_overlay,
            Some(DetailOverlay::Position(_))
        ));
        // Any other key dismisses.
        let _ = handle_input(&mut app, press(KeyCode::Char('q')));
        assert!(app.detail_overlay.is_none());
    }

    #[test]
    fn q_quits_when_no_overlay() {
        let mut app = App::new();
        let _ = handle_input(&mut app, press(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    // ── Brief view key tests ───────────────────────────────────────────────

    #[test]
    fn key_1_does_not_emit_visited_actions() {
        let mut app = App::new();
        let action = handle_input(&mut app, press(KeyCode::Char('1')));
        assert_eq!(app.view, View::Chart);
        assert!(matches!(action, Action::Continue));
    }

    // ── Report view key tests ─────────────────────────────────────────────

    #[test]
    fn key_5_switches_to_report_view_and_emits_visited() {
        // report lives on key 5.
        let mut app = App::new();
        let action = handle_input(&mut app, press(KeyCode::Char('5')));
        assert_eq!(app.view, View::Report);
        assert!(matches!(action, Action::ReportVisited));
    }

    #[test]
    fn bracket_keys_toggle_report_choice_in_report_view() {
        let mut app = App::new();
        app.view = View::Report;
        let initial = app.brief_report_choice;
        let _ = handle_input(&mut app, press(KeyCode::Char(']')));
        assert_ne!(app.brief_report_choice, initial, "] should toggle");
        let after_first = app.brief_report_choice;
        let _ = handle_input(&mut app, press(KeyCode::Char(']')));
        assert_eq!(
            app.brief_report_choice, initial,
            "second ] should toggle back"
        );
        let _ = handle_input(&mut app, press(KeyCode::Char('[')));
        assert_eq!(app.brief_report_choice, after_first, "[ should also toggle");
    }

    #[test]
    fn report_toggle_resets_scroll() {
        let mut app = App::new();
        app.view = View::Report;
        app.report_scroll = 42;
        let _ = handle_input(&mut app, press(KeyCode::Char(']')));
        assert_eq!(
            app.report_scroll, 0,
            "] must jump to top of the other report"
        );
        app.report_scroll = 17;
        let _ = handle_input(&mut app, press(KeyCode::Char('[')));
        assert_eq!(
            app.report_scroll, 0,
            "[ must jump to top of the other report"
        );
    }

    #[test]
    fn report_match_scroll_accounts_for_wrapped_lines() {
        let mut app = App::new();
        // Source line 0 is 100 chars → wraps to ~5 display rows at width 20.
        // A match on source line 1 must scroll past the wrapped rows; the old
        // code scrolled by the source index (1) and landed short.
        let content = format!("{}\ntarget", "x".repeat(100));
        app.report_morning = Some(crate::types::report::ReportResponse {
            content: Some(content),
            created_at: None,
        });
        app.brief_report_choice = BriefReportChoice::Morning;
        app.report_body_width = 20;
        let target = report_match_scroll(&app, 1);
        assert!(
            target >= 3,
            "wrapped line 0 must advance display rows, got {target}"
        );
        // Before first render (width unknown) it falls back to the raw index.
        app.report_body_width = 0;
        assert_eq!(report_match_scroll(&app, 1), 1);
    }

    #[test]
    fn bracket_keys_are_noop_outside_report_view() {
        let mut app = App::new();
        app.view = View::Chart;
        let initial_choice = app.brief_report_choice;
        let _ = handle_input(&mut app, press(KeyCode::Char('[')));
        assert_eq!(
            app.brief_report_choice, initial_choice,
            "[ in chart view must not affect brief_report_choice"
        );
        let _ = handle_input(&mut app, press(KeyCode::Char(']')));
        assert_eq!(
            app.brief_report_choice, initial_choice,
            "] in chart view must not affect brief_report_choice"
        );
    }

    #[test]
    fn report_view_j_scrolls_down() {
        let mut app = App::new();
        app.view = View::Report;
        let _ = handle_input(&mut app, press(KeyCode::Char('j')));
        assert_eq!(app.report_scroll, 1);
        let _ = handle_input(&mut app, press(KeyCode::Down));
        assert_eq!(app.report_scroll, 2);
    }

    #[test]
    fn report_view_k_scrolls_up() {
        let mut app = App::new();
        app.view = View::Report;
        app.report_scroll = 5;
        let _ = handle_input(&mut app, press(KeyCode::Char('k')));
        assert_eq!(app.report_scroll, 4);
        let _ = handle_input(&mut app, press(KeyCode::Up));
        assert_eq!(app.report_scroll, 3);
    }

    #[test]
    fn report_view_g_resets_scroll() {
        let mut app = App::new();
        app.view = View::Report;
        app.report_scroll = 42;
        let _ = handle_input(&mut app, press(KeyCode::Char('g')));
        assert_eq!(app.report_scroll, 0);
    }

    #[test]
    fn report_view_capital_g_jumps_to_bottom() {
        let mut app = App::new();
        app.view = View::Report;
        app.report_scroll = 0;
        let _ = handle_input(&mut app, press(KeyCode::Char('G')));
        assert!(app.report_scroll > 0, "G should set a large scroll offset");
    }

    // ── Agent launch key (I) ──────────────────────────────────────────────────

    #[test]
    fn capital_i_emits_launch_agent() {
        let mut app = App::new();
        app.view = View::Chart;
        let action = handle_input(&mut app, press(KeyCode::Char('I')));
        assert!(
            matches!(action, Action::LaunchAgent { .. }),
            "capital I should emit LaunchAgent, not {:?}",
            std::mem::discriminant(&action)
        );
    }

    #[test]
    fn capital_i_works_from_any_view() {
        for view in [
            View::Chart,
            View::Positions,
            View::Plays,
            View::Alerts,
            View::Report,
            View::News,
            View::Details,
        ] {
            let mut app = App::new();
            app.view = view;
            let action = handle_input(&mut app, press(KeyCode::Char('I')));
            assert!(
                matches!(action, Action::LaunchAgent { .. }),
                "capital I should emit LaunchAgent from {view:?}"
            );
        }
    }

    #[test]
    fn six_key_switches_to_news_view() {
        // Brief tab retired: key 6 = News.
        let mut app = App::new();
        let action = handle_input(&mut app, press(KeyCode::Char('6')));
        assert_eq!(app.view, View::News);
        assert!(matches!(action, Action::NewsVisited));
    }

    #[test]
    fn seven_key_switches_to_details_view() {
        let mut app = App::new();
        let action = handle_input(&mut app, press(KeyCode::Char('7')));
        assert_eq!(app.view, View::Details);
        assert!(matches!(action, Action::DetailsVisited));
    }

    // ── Search key tests ──────────────────────────────────────────────────────

    #[test]
    fn slash_opens_search_in_positions_view() {
        let mut app = App::new();
        app.view = View::Positions;
        let _ = handle_input(&mut app, press(KeyCode::Char('/')));
        assert!(app.search.is_some(), "search should be Some after /");
        assert!(
            app.search.as_ref().unwrap().is_open(),
            "prompt should be open"
        );
    }

    #[test]
    fn slash_opens_search_in_plays_view() {
        let mut app = App::new();
        app.view = View::Plays;
        let _ = handle_input(&mut app, press(KeyCode::Char('/')));
        assert!(app.search.is_some());
    }

    #[test]
    fn slash_opens_search_in_alerts_view() {
        let mut app = App::new();
        app.view = View::Alerts;
        let _ = handle_input(&mut app, press(KeyCode::Char('/')));
        assert!(app.search.is_some());
    }

    #[test]
    fn slash_opens_search_in_report_view() {
        let mut app = App::new();
        app.view = View::Report;
        let _ = handle_input(&mut app, press(KeyCode::Char('/')));
        assert!(app.search.is_some());
        use crate::app::state::SearchMode;
        assert_eq!(app.search.as_ref().unwrap().mode, SearchMode::Find);
    }

    #[test]
    fn slash_noop_in_chart_view() {
        let mut app = App::new();
        app.view = View::Chart;
        let _ = handle_input(&mut app, press(KeyCode::Char('/')));
        assert!(
            app.search.is_none(),
            "/ should not open search in chart view"
        );
    }

    #[test]
    fn slash_noop_in_news_view() {
        let mut app = App::new();
        app.view = View::News;
        let _ = handle_input(&mut app, press(KeyCode::Char('/')));
        assert!(app.search.is_none());
    }

    #[test]
    fn search_prompt_typing_builds_query() {
        let mut app = App::new();
        app.view = View::Positions;
        let _ = handle_input(&mut app, press(KeyCode::Char('/')));
        let _ = handle_input(&mut app, press(KeyCode::Char('b')));
        let _ = handle_input(&mut app, press(KeyCode::Char('t')));
        let _ = handle_input(&mut app, press(KeyCode::Char('c')));
        assert_eq!(app.search.as_ref().unwrap().query, "btc");
    }

    #[test]
    fn search_prompt_backspace_removes_char() {
        let mut app = App::new();
        app.view = View::Positions;
        let _ = handle_input(&mut app, press(KeyCode::Char('/')));
        let _ = handle_input(&mut app, press(KeyCode::Char('b')));
        let _ = handle_input(&mut app, press(KeyCode::Char('t')));
        let _ = handle_input(&mut app, press(KeyCode::Backspace));
        assert_eq!(app.search.as_ref().unwrap().query, "b");
    }

    #[test]
    fn search_prompt_enter_closes_prompt_keeps_query() {
        let mut app = App::new();
        app.view = View::Positions;
        let _ = handle_input(&mut app, press(KeyCode::Char('/')));
        let _ = handle_input(&mut app, press(KeyCode::Char('b')));
        let _ = handle_input(&mut app, press(KeyCode::Enter));
        let s = app.search.as_ref().unwrap();
        assert!(!s.is_open(), "prompt should be closed after Enter");
        assert!(s.is_active(), "query should still be active");
        assert_eq!(s.query, "b");
    }

    #[test]
    fn search_prompt_esc_clears_query_and_closes() {
        let mut app = App::new();
        app.view = View::Positions;
        let _ = handle_input(&mut app, press(KeyCode::Char('/')));
        let _ = handle_input(&mut app, press(KeyCode::Char('b')));
        let _ = handle_input(&mut app, press(KeyCode::Esc));
        assert!(
            app.search.is_none(),
            "search should be cleared on Esc in prompt"
        );
    }

    #[test]
    fn esc_when_no_prompt_clears_active_query() {
        let mut app = App::new();
        app.view = View::Positions;
        // Open search, type, close prompt with Enter.
        let _ = handle_input(&mut app, press(KeyCode::Char('/')));
        let _ = handle_input(&mut app, press(KeyCode::Char('b')));
        let _ = handle_input(&mut app, press(KeyCode::Enter));
        assert!(app.search.is_some(), "query active after Enter");
        // Esc with no prompt open → clear query.
        let _ = handle_input(&mut app, press(KeyCode::Esc));
        assert!(
            app.search.is_none(),
            "Esc with prompt closed should clear query"
        );
    }

    #[test]
    fn view_switch_clears_search() {
        let mut app = App::new();
        app.view = View::Positions;
        let _ = handle_input(&mut app, press(KeyCode::Char('/')));
        assert!(app.search.is_some());
        // Press '3' while prompt is open → search prompt handler handles it:
        // clears search and switches to plays view.
        let _ = handle_input(&mut app, press(KeyCode::Char('3')));
        assert_eq!(app.view, View::Plays);
        assert!(
            app.search.is_none(),
            "search should be cleared on view switch"
        );
    }

    #[test]
    fn filter_narrows_positions_list() {
        use crate::types::positions::{Direction, PositionsResponse};
        let mut app = App::new();
        app.view = View::Positions;
        app.apply(crate::app::state::Update::Positions(PositionsResponse {
            positions: vec![
                crate::types::positions::Position {
                    id: 1,
                    asset: "BTC".into(),
                    direction: Direction::Long,
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
                },
                crate::types::positions::Position {
                    id: 2,
                    asset: "ETH".into(),
                    direction: Direction::Short,
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
                },
            ],
            portfolio_heat: None,
        }));
        // Open search and type "eth".
        let _ = handle_input(&mut app, press(KeyCode::Char('/')));
        let _ = handle_input(&mut app, press(KeyCode::Char('e')));
        let _ = handle_input(&mut app, press(KeyCode::Char('t')));
        let _ = handle_input(&mut app, press(KeyCode::Char('h')));
        // Only ETH (index 1) should appear.
        let filtered = app.filtered_positions();
        assert_eq!(filtered, vec![1], "only ETH should match 'eth'");
    }

    #[test]
    fn n_key_navigates_report_find_matches() {
        use crate::app::state::BriefReportChoice;
        use crate::types::report::ReportResponse;
        let mut app = App::new();
        app.view = View::Report;
        // Pin to morning so the test is clock-independent.
        app.brief_report_choice = BriefReportChoice::Morning;
        app.report_morning = Some(ReportResponse {
            content: Some("BTC is strong\nETH is mixed\nBTC volume up".to_string()),
            created_at: None,
        });
        // Open search, type "btc", close prompt.
        let _ = handle_input(&mut app, press(KeyCode::Char('/')));
        let _ = handle_input(&mut app, press(KeyCode::Char('b')));
        let _ = handle_input(&mut app, press(KeyCode::Char('t')));
        let _ = handle_input(&mut app, press(KeyCode::Char('c')));
        let _ = handle_input(&mut app, press(KeyCode::Enter));
        // Should have 2 matches (line 0 and line 2).
        let matches = &app.search.as_ref().unwrap().matches;
        assert_eq!(matches, &vec![0, 2]);
        // n jumps to next match.
        let _ = handle_input(&mut app, press(KeyCode::Char('n')));
        assert_eq!(app.search.as_ref().unwrap().current_match, 1);
        // N goes back.
        let _ = handle_input(&mut app, press(KeyCode::Char('N')));
        assert_eq!(app.search.as_ref().unwrap().current_match, 0);
    }

    #[test]
    fn search_clears_when_switching_views() {
        let mut app = App::new();
        app.view = View::Report;
        // Open search in report.
        let _ = handle_input(&mut app, press(KeyCode::Char('/')));
        assert!(app.search.is_some());
        // Switch to chart via number key (goes through search prompt handler
        // since prompt is open, which closes search and switches view).
        let _ = handle_input(&mut app, press(KeyCode::Char('1')));
        assert_eq!(app.view, View::Chart);
        assert!(app.search.is_none());
    }

    // ── News view key tests ───────────────────────────────────────────────────

    fn make_news_item(id: &str, title: &str) -> crate::types::news::NewsItem {
        crate::types::news::NewsItem {
            id: id.into(),
            title: title.into(),
            summary: "summary".into(),
            link: format!("https://example.com/{id}"),
            published: "2026-05-23T12:00:00+00:00".into(),
            source: "Test".into(),
            source_id: "test".into(),
        }
    }

    fn load_news(app: &mut App, count: usize) {
        use crate::types::news::NewsResponse;
        let items = (0..count)
            .map(|i| make_news_item(&i.to_string(), &format!("Title {i}")))
            .collect();
        app.apply(crate::app::state::Update::News(Ok(NewsResponse {
            items,
            feeds: std::collections::HashMap::new(),
            timestamp: "2026-05-23T12:00:00+00:00".into(),
        })));
    }

    #[test]
    fn key_6_switches_to_news_view_and_emits_news_visited() {
        // Brief tab retired: key 6 = News.
        let mut app = App::new();
        let action = handle_input(&mut app, press(KeyCode::Char('6')));
        assert_eq!(app.view, View::News);
        assert!(matches!(action, Action::NewsVisited));
    }

    #[test]
    fn key_7_switches_to_details_view_and_emits_details_visited() {
        // Brief tab retired: key 7 = Details.
        let mut app = App::new();
        let action = handle_input(&mut app, press(KeyCode::Char('7')));
        assert_eq!(app.view, View::Details);
        assert!(matches!(action, Action::DetailsVisited));
    }

    #[test]
    fn news_jk_moves_cursor() {
        let mut app = App::new();
        app.view = View::News;
        load_news(&mut app, 5);
        assert_eq!(app.news_cursor.selected(), Some(0));
        let _ = handle_input(&mut app, press(KeyCode::Char('j')));
        assert_eq!(app.news_cursor.selected(), Some(1));
        let _ = handle_input(&mut app, press(KeyCode::Char('j')));
        assert_eq!(app.news_cursor.selected(), Some(2));
        let _ = handle_input(&mut app, press(KeyCode::Char('k')));
        assert_eq!(app.news_cursor.selected(), Some(1));
    }

    #[test]
    fn news_cursor_clamps_at_bounds() {
        let mut app = App::new();
        app.view = View::News;
        load_news(&mut app, 3);
        // j at bottom clamps.
        app.news_cursor.select(Some(2));
        let _ = handle_input(&mut app, press(KeyCode::Char('j')));
        assert_eq!(app.news_cursor.selected(), Some(2));
        // k at top clamps.
        app.news_cursor.select(Some(0));
        let _ = handle_input(&mut app, press(KeyCode::Char('k')));
        assert_eq!(app.news_cursor.selected(), Some(0));
    }

    #[test]
    fn news_g_jumps_to_top() {
        let mut app = App::new();
        app.view = View::News;
        load_news(&mut app, 5);
        app.news_cursor.select(Some(4));
        let _ = handle_input(&mut app, press(KeyCode::Char('g')));
        assert_eq!(app.news_cursor.selected(), Some(0));
    }

    #[test]
    fn news_capital_g_jumps_to_bottom() {
        let mut app = App::new();
        app.view = View::News;
        load_news(&mut app, 5);
        app.news_cursor.select(Some(0));
        let _ = handle_input(&mut app, press(KeyCode::Char('G')));
        assert_eq!(app.news_cursor.selected(), Some(4));
    }

    #[test]
    fn news_bracket_keys_cycle_preset() {
        use crate::app::state::NewsFeedPreset;
        let mut app = App::new();
        app.view = View::News;
        assert_eq!(app.news_feed_preset, NewsFeedPreset::Default);
        let action = handle_input(&mut app, press(KeyCode::Char(']')));
        assert_eq!(app.news_feed_preset, NewsFeedPreset::AllCrypto);
        assert!(matches!(action, Action::RefreshNews));
        let action2 = handle_input(&mut app, press(KeyCode::Char('[')));
        assert_eq!(app.news_feed_preset, NewsFeedPreset::Default);
        assert!(matches!(action2, Action::RefreshNews));
    }

    #[test]
    fn news_open_link_command_spec_does_not_panic() {
        // Verify open_link_in_browser does not panic for a valid-looking URL.
        // We cannot manipulate $BROWSER safely in multi-threaded test binaries
        // (set_var/remove_var are unsafe in Rust 2024), so we only verify that
        // the call completes without panicking when the env is left as-is.
        // xdg-open / $BROWSER may or may not be present; failures are silent.
        open_link_in_browser("https://example.com/news/test-article-dry-run");
    }

    #[test]
    fn empty_query_shows_all_results_in_filter_mode() {
        use crate::types::plays::PlaysResponse;
        let mut app = App::new();
        app.view = View::Plays;
        app.apply(crate::app::state::Update::Plays(PlaysResponse {
            plays: vec![
                crate::types::plays::Play {
                    id: 1,
                    title: "BTC breakout".into(),
                    primary_asset: Some("BTC".into()),
                    direction: Some("long".into()),
                    status: Some("active".into()),
                    timeframe: None,
                    thesis: None,
                    action_plan: None,
                    expiry: None,
                    created_at: None,
                    updated_at: None,
                    invalidation: None,
                    conditions: None,
                },
                crate::types::plays::Play {
                    id: 2,
                    title: "ETH pullback".into(),
                    primary_asset: Some("ETH".into()),
                    direction: Some("short".into()),
                    status: Some("active".into()),
                    timeframe: None,
                    thesis: None,
                    action_plan: None,
                    expiry: None,
                    created_at: None,
                    updated_at: None,
                    invalidation: None,
                    conditions: None,
                },
            ],
            count: 2,
        }));
        // Open search but keep query empty (just press /).
        let _ = handle_input(&mut app, press(KeyCode::Char('/')));
        // Empty query → all rows.
        assert_eq!(app.filtered_plays().len(), 2);
    }

    #[test]
    fn report_view_no_data_search_opens_without_crash() {
        let mut app = App::new();
        app.view = View::Report;
        // No report data loaded — search should still open.
        let _ = handle_input(&mut app, press(KeyCode::Char('/')));
        assert!(app.search.is_some());
        // Typing and entering should not panic.
        let _ = handle_input(&mut app, press(KeyCode::Char('b')));
        let _ = handle_input(&mut app, press(KeyCode::Enter));
        let matches = &app.search.as_ref().unwrap().matches;
        assert!(matches.is_empty(), "no data → no matches");
    }

    // ── New UX goals (feat/ux-polish) ────────────────────────────────────────

    // Goal 5: Space cycles tabs
    #[test]
    fn space_cycles_tabs_forward() {
        let mut app = App::new();
        // Start at Chart (default).
        assert_eq!(app.view, View::Chart);
        let _ = handle_input(&mut app, press(KeyCode::Char(' ')));
        assert_eq!(app.view, View::Positions);
        let _ = handle_input(&mut app, press(KeyCode::Char(' ')));
        assert_eq!(app.view, View::Plays);
        let _ = handle_input(&mut app, press(KeyCode::Char(' ')));
        assert_eq!(app.view, View::Alerts);
        let _ = handle_input(&mut app, press(KeyCode::Char(' ')));
        assert_eq!(app.view, View::Report);
    }

    #[test]
    fn space_cycles_back_to_chart_after_last_view() {
        // Details is the last view; cycle_next wraps to Chart.
        assert_eq!(
            View::Details.cycle_next(),
            View::Chart,
            "Details.cycle_next() should wrap to Chart"
        );
        assert_eq!(View::News.cycle_next(), View::Details);
    }

    // ── Tab / Shift+Tab universal cycle ─────────────────────────────────────

    #[test]
    fn tab_cycles_view_forward_from_chart() {
        let mut app = App::new();
        assert_eq!(app.view, View::Chart);
        let _ = handle_input(&mut app, press(KeyCode::Tab));
        assert_eq!(app.view, View::Positions);
    }

    #[test]
    fn tab_cycles_view_forward_from_report() {
        // Report is index 5; Tab should go to News (index 6).
        let mut app = App::new();
        app.view = View::Report;
        let action = handle_input(&mut app, press(KeyCode::Tab));
        assert_eq!(app.view, View::News, "Tab from Report must cycle to News");
        assert!(matches!(action, Action::NewsVisited));
    }

    #[test]
    fn shift_tab_cycles_backward_from_chart() {
        // Chart is index 0; Shift+Tab should wrap to Details (last).
        let mut app = App::new();
        assert_eq!(app.view, View::Chart);
        let _ = handle_input(
            &mut app,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE),
        );
        assert_eq!(
            app.view,
            View::Details,
            "BackTab from Chart must wrap to Details"
        );
    }

    #[test]
    fn tab_in_add_alert_form_cycles_fields_not_views() {
        let mut app = App::new();
        app.open_add_alert_form();
        let view_before = app.view;
        let _ = handle_input(&mut app, press(KeyCode::Tab));
        // View must not change — form handler wins.
        assert_eq!(
            app.view, view_before,
            "Tab with open form must not change the active view"
        );
        // Field should have advanced (from Price default → Note).
        assert_eq!(
            app.add_alert_form.as_ref().unwrap().field,
            crate::app::state::AddAlertField::Note
        );
    }

    #[test]
    fn tab_emits_reportvisited_when_landing_on_report() {
        let mut app = App::new();
        // Report follows Alerts in the cycle (Brief tab retired).
        app.view = View::Alerts;
        let action = handle_input(&mut app, press(KeyCode::Tab));
        assert_eq!(app.view, View::Report);
        assert!(matches!(action, Action::ReportVisited));
    }

    #[test]
    fn view_cycle_prev_wraps_chart_to_details() {
        // Unit test directly on View::cycle_prev.
        assert_eq!(
            View::Chart.cycle_prev(),
            View::Details,
            "cycle_prev from Chart should wrap to Details"
        );
        assert_eq!(View::Details.cycle_prev(), View::News);
        assert_eq!(View::News.cycle_prev(), View::Report);
        assert_eq!(View::Positions.cycle_prev(), View::Chart);
    }

    // Goal 4: l opens detail overlay, h closes it
    #[test]
    fn l_in_positions_opens_detail_overlay() {
        use crate::app::state::{DetailOverlay, Update};
        use crate::types::positions::{Direction, Position, PositionsResponse};
        let mut app = App::new();
        app.view = View::Positions;
        app.apply(Update::Positions(PositionsResponse {
            positions: vec![Position {
                id: 99,
                asset: "BTC".into(),
                direction: Direction::Long,
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
            }],
            portfolio_heat: None,
        }));
        let _ = handle_input(&mut app, press(KeyCode::Char('l')));
        assert!(
            matches!(app.detail_overlay, Some(DetailOverlay::Position(_))),
            "l should open the detail overlay"
        );
    }

    #[test]
    fn h_in_positions_with_overlay_open_closes_it() {
        use crate::app::state::Update;
        use crate::types::positions::{Direction, Position, PositionsResponse};
        let mut app = App::new();
        app.view = View::Positions;
        app.apply(Update::Positions(PositionsResponse {
            positions: vec![Position {
                id: 99,
                asset: "BTC".into(),
                direction: Direction::Long,
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
            }],
            portfolio_heat: None,
        }));
        // Open overlay first.
        let _ = handle_input(&mut app, press(KeyCode::Char('l')));
        assert!(app.detail_overlay.is_some(), "overlay must be open first");
        // h should close it.
        let _ = handle_input(&mut app, press(KeyCode::Char('h')));
        assert!(
            app.detail_overlay.is_none(),
            "h should close the detail overlay"
        );
    }

    // ── Details view key tests ────────────────────────────────────────────────

    #[test]
    fn details_view_jk_moves_cursor() {
        let mut app = App::new();
        app.view = View::Details;
        app.watchlist_symbols = vec!["BTC".into(), "ETH".into(), "SOL".into()];
        let _ = handle_input(&mut app, press(KeyCode::Char('j')));
        assert_eq!(app.details_cursor.selected(), Some(1));
        let _ = handle_input(&mut app, press(KeyCode::Char('k')));
        assert_eq!(app.details_cursor.selected(), Some(0));
    }

    #[test]
    fn details_view_g_jumps_to_top() {
        let mut app = App::new();
        app.view = View::Details;
        app.watchlist_symbols = vec!["BTC".into(), "ETH".into(), "SOL".into()];
        app.details_cursor.select(Some(2));
        let _ = handle_input(&mut app, press(KeyCode::Char('g')));
        assert_eq!(app.details_cursor.selected(), Some(0));
    }

    #[test]
    fn details_view_capital_g_jumps_to_bottom() {
        let mut app = App::new();
        app.view = View::Details;
        app.watchlist_symbols = vec!["BTC".into(), "ETH".into(), "SOL".into()];
        let _ = handle_input(&mut app, press(KeyCode::Char('G')));
        assert_eq!(app.details_cursor.selected(), Some(2));
    }

    #[test]
    fn details_view_enter_emits_jump_to_chart_with_symbol() {
        let mut app = App::new();
        app.view = View::Details;
        app.watchlist_symbols = vec!["BTC".into(), "ETH".into(), "SOL".into()];
        app.details_cursor.select(Some(1));
        let action = handle_input(&mut app, press(KeyCode::Enter));
        if let Action::JumpToChart { symbol } = action {
            assert_eq!(symbol, "ETH");
        } else {
            panic!("expected JumpToChart action");
        }
    }

    #[test]
    fn details_view_l_also_emits_jump_to_chart() {
        let mut app = App::new();
        app.view = View::Details;
        app.watchlist_symbols = vec!["BTC".into(), "ETH".into()];
        app.details_cursor.select(Some(0));
        let action = handle_input(&mut app, press(KeyCode::Char('l')));
        if let Action::JumpToChart { symbol } = action {
            assert_eq!(symbol, "BTC");
        } else {
            panic!("expected JumpToChart action from l key");
        }
    }

    #[test]
    fn details_view_watchlist_cap_applies_to_cursor_nav() {
        let mut app = App::new();
        app.view = View::Details;
        app.watchlist_symbols = (0..30).map(|i| format!("SYM{i}")).collect();
        let _ = handle_input(&mut app, press(KeyCode::Char('G')));
        assert_eq!(app.details_cursor.selected(), Some(24));
    }

    // ── Add-alert form key tests ────────────────────────────────────────────────

    #[test]
    fn capital_i_in_alerts_view_launches_agent_not_form() {
        // Capital I is the global agent-launch key; in alerts view it emits
        // LaunchAgent, NOT the add-alert form (capital A opens the form there).
        let mut app = App::new();
        app.view = View::Alerts;
        app.selected_asset = "BTC".into();
        let action = handle_input(&mut app, press(KeyCode::Char('I')));
        assert!(
            matches!(action, Action::LaunchAgent { .. }),
            "capital I must emit LaunchAgent (agent key), not open the alert form"
        );
        assert!(
            app.add_alert_form.is_none(),
            "form must NOT open on capital I"
        );
    }

    #[test]
    fn capital_a_in_alerts_view_opens_form() {
        // A is the create-alert key (restored — agent launch moved to I).
        let mut app = App::new();
        app.view = View::Alerts;
        app.selected_asset = "BTC".into();
        let _ = handle_input(&mut app, press(KeyCode::Char('A')));
        assert!(
            app.add_alert_form.is_some(),
            "capital A should open the add-alert form"
        );
        let form = app.add_alert_form.as_ref().unwrap();
        assert_eq!(form.symbol, "BTC");
    }

    #[test]
    fn lowercase_a_in_alerts_view_opens_asset_selector_not_form() {
        let mut app = App::new();
        app.view = View::Alerts;
        let _ = handle_input(&mut app, press(KeyCode::Char('a')));
        assert!(
            app.asset_selector.is_some(),
            "lowercase a should open asset selector"
        );
        assert!(
            app.add_alert_form.is_none(),
            "lowercase a must NOT open the add-alert form"
        );
    }

    #[test]
    fn esc_closes_form() {
        let mut app = App::new();
        app.open_add_alert_form();
        let _ = handle_input(&mut app, press(KeyCode::Esc));
        assert!(app.add_alert_form.is_none(), "Esc should close the form");
    }

    #[test]
    fn tab_cycles_fields_forward() {
        let mut app = App::new();
        app.open_add_alert_form();
        // Default field is Price.
        assert_eq!(
            app.add_alert_form.as_ref().unwrap().field,
            crate::app::state::AddAlertField::Price
        );
        let _ = handle_input(&mut app, press(KeyCode::Tab));
        assert_eq!(
            app.add_alert_form.as_ref().unwrap().field,
            crate::app::state::AddAlertField::Note
        );
        let _ = handle_input(&mut app, press(KeyCode::Tab));
        assert_eq!(
            app.add_alert_form.as_ref().unwrap().field,
            crate::app::state::AddAlertField::Symbol
        );
        let _ = handle_input(&mut app, press(KeyCode::Tab));
        assert_eq!(
            app.add_alert_form.as_ref().unwrap().field,
            crate::app::state::AddAlertField::Condition
        );
    }

    #[test]
    fn shift_tab_cycles_fields_backward() {
        let mut app = App::new();
        app.open_add_alert_form();
        // Default field is Price; Shift+Tab (BackTab) goes to Condition.
        let _ = handle_input(
            &mut app,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE),
        );
        assert_eq!(
            app.add_alert_form.as_ref().unwrap().field,
            crate::app::state::AddAlertField::Condition
        );
        let _ = handle_input(
            &mut app,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE),
        );
        assert_eq!(
            app.add_alert_form.as_ref().unwrap().field,
            crate::app::state::AddAlertField::Symbol
        );
    }

    #[test]
    fn left_right_toggle_condition_only_in_condition_field() {
        use crate::app::state::AlertCondition;
        let mut app = App::new();
        app.open_add_alert_form();
        // Focus the Condition field.
        app.add_alert_form.as_mut().unwrap().field = crate::app::state::AddAlertField::Condition;
        assert_eq!(
            app.add_alert_form.as_ref().unwrap().condition,
            AlertCondition::Above
        );
        let _ = handle_input(&mut app, press(KeyCode::Right));
        assert_eq!(
            app.add_alert_form.as_ref().unwrap().condition,
            AlertCondition::Below
        );
        let _ = handle_input(&mut app, press(KeyCode::Left));
        assert_eq!(
            app.add_alert_form.as_ref().unwrap().condition,
            AlertCondition::Above
        );
    }

    #[test]
    fn left_right_in_price_field_do_not_toggle_condition() {
        use crate::app::state::AlertCondition;
        let mut app = App::new();
        app.open_add_alert_form();
        // Focus stays on Price (default).
        let _ = handle_input(&mut app, press(KeyCode::Right));
        // condition should not change
        assert_eq!(
            app.add_alert_form.as_ref().unwrap().condition,
            AlertCondition::Above
        );
    }

    #[test]
    fn printable_char_appends_to_focused_text_field() {
        let mut app = App::new();
        app.open_add_alert_form();
        // Focus is Price by default.
        let _ = handle_input(&mut app, press(KeyCode::Char('7')));
        let _ = handle_input(&mut app, press(KeyCode::Char('5')));
        assert_eq!(app.add_alert_form.as_ref().unwrap().price, "75");
    }

    #[test]
    fn backspace_removes_last_char_from_focused_field() {
        let mut app = App::new();
        app.open_add_alert_form();
        app.add_alert_form.as_mut().unwrap().price = "750".to_string();
        let _ = handle_input(&mut app, press(KeyCode::Backspace));
        assert_eq!(app.add_alert_form.as_ref().unwrap().price, "75");
    }

    #[test]
    fn submit_with_empty_symbol_sets_error_keeps_form_open() {
        let mut app = App::new();
        app.open_add_alert_form();
        app.add_alert_form.as_mut().unwrap().symbol.clear();
        app.add_alert_form.as_mut().unwrap().price = "75000".to_string();
        let action = handle_input(&mut app, press(KeyCode::Enter));
        assert!(app.add_alert_form.is_some(), "form should stay open");
        assert!(
            app.add_alert_form.as_ref().unwrap().error.is_some(),
            "error should be set"
        );
        assert!(matches!(action, Action::Continue));
    }

    #[test]
    fn submit_with_invalid_price_sets_error_keeps_form_open() {
        let mut app = App::new();
        app.open_add_alert_form();
        app.add_alert_form.as_mut().unwrap().symbol = "BTC".to_string();
        app.add_alert_form.as_mut().unwrap().price = "abc".to_string();
        let action = handle_input(&mut app, press(KeyCode::Enter));
        assert!(app.add_alert_form.is_some(), "form should stay open");
        let form = app.add_alert_form.as_ref().unwrap();
        assert_eq!(
            form.error.as_deref(),
            Some("Invalid price"),
            "error should say Invalid price"
        );
        assert!(matches!(action, Action::Continue));
    }

    #[test]
    fn submit_with_valid_input_marks_submitting_and_emits_action() {
        let mut app = App::new();
        app.open_add_alert_form();
        app.add_alert_form.as_mut().unwrap().symbol = "BTC".to_string();
        app.add_alert_form.as_mut().unwrap().price = "75000".to_string();
        let action = handle_input(&mut app, press(KeyCode::Enter));
        // form should still be present (submitting) and submitting=true
        assert!(app.add_alert_form.is_some());
        assert!(app.add_alert_form.as_ref().unwrap().submitting);
        assert!(matches!(action, Action::SubmitAlert { .. }));
        if let Action::SubmitAlert {
            symbol,
            condition,
            price,
            ..
        } = action
        {
            assert_eq!(symbol, "BTC");
            assert_eq!(condition, "above");
            assert!((price - 75000.0).abs() < 1e-9);
        }
    }

    // ── PNG popup key tests ───────────────────────────────────────────────────

    #[test]
    fn p_in_chart_view_emits_show_chart_png_with_current_asset_and_tf() {
        let mut app = App::new();
        app.view = View::Chart;
        app.selected_asset = "BTC".into();
        app.chart.tf = "4H".into();
        let action = handle_input(&mut app, press(KeyCode::Char('P')));
        if let Action::ShowChartPng { asset, tf } = action {
            assert_eq!(asset, "BTC");
            assert_eq!(tf, "4H");
        } else {
            panic!("expected ShowChartPng from P in chart view");
        }
    }

    #[test]
    fn p_in_chart_view_uses_current_tf_not_hardcoded() {
        // Verify the emitted TF tracks the live chart.tf, not a hardcoded value.
        let mut app = App::new();
        app.view = View::Chart;
        app.selected_asset = "ETH".into();
        app.chart.tf = "1D".into();
        let action = handle_input(&mut app, press(KeyCode::Char('P')));
        if let Action::ShowChartPng { asset, tf } = action {
            assert_eq!(asset, "ETH");
            assert_eq!(tf, "1D");
        } else {
            panic!("expected ShowChartPng with 1D tf");
        }
    }

    #[test]
    fn p_in_details_view_emits_show_chart_png_with_cursored_symbol_and_1d_tf() {
        let mut app = App::new();
        app.view = View::Details;
        app.watchlist_symbols = vec!["BTC".into(), "ETH".into(), "SOL".into()];
        app.details_cursor.select(Some(1)); // ETH
        let action = handle_input(&mut app, press(KeyCode::Char('P')));
        if let Action::ShowChartPng { asset, tf } = action {
            assert_eq!(asset, "ETH");
            assert_eq!(tf, "1D", "details view P always uses 1D tf");
        } else {
            panic!("expected ShowChartPng from P in details view");
        }
    }

    #[test]
    fn p_in_details_view_with_empty_watchlist_is_noop() {
        let mut app = App::new();
        app.view = View::Details;
        app.watchlist_symbols = vec![];
        let action = handle_input(&mut app, press(KeyCode::Char('P')));
        assert!(
            matches!(action, Action::Continue),
            "P with empty watchlist should be Continue"
        );
    }

    #[test]
    fn lowercase_p_in_chart_view_does_not_emit_png_action() {
        // Lowercase p toggles position overlay — must NOT trigger PNG.
        let mut app = App::new();
        app.view = View::Chart;
        let action = handle_input(&mut app, press(KeyCode::Char('p')));
        assert!(
            !matches!(action, Action::ShowChartPng { .. }),
            "lowercase p must not emit ShowChartPng"
        );
    }
}
