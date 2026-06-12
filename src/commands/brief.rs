use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::client::Client;
use crate::config::Config;
use crate::format as f;
use crate::helpers::wrap::{truncate, wrap_text};
use crate::types::alerts::AlertsResponse;
use crate::types::plays::PlaysResponse;
use crate::types::positions::{Position, PositionsResponse, RiskStatus};
use crate::types::report::ReportResponse;
use crate::types::scanner::{ScannerResponse, ScannerResult};
use crate::types::sentiment::SentimentResponse;

const LABEL_WIDTH: usize = 13;

// `BriefSnapshot` lives here (not in src/types/) because it is a command-level
// orchestration struct: it bundles the six concurrent Results that `fetch()`
// fires in parallel and hands them as a unit to `render()`.  It has no
// domain-type semantics of its own — it is purely an async coordination artefact.
pub struct BriefSnapshot {
    pub sentiment: Result<SentimentResponse>,
    pub positions: Result<PositionsResponse>,
    pub scanner: Result<ScannerResponse>,
    pub plays: Result<PlaysResponse>,
    pub alerts: Result<AlertsResponse>,
    pub report: Result<ReportResponse>,
}

/// Fire all six API calls in parallel and return raw Results.
/// Per-section failures are preserved so `run()` / TUI can render partial data.
pub(crate) async fn fetch(client: &Client) -> BriefSnapshot {
    let (sentiment, positions, scanner, plays, alerts, report) = tokio::join!(
        client.get::<SentimentResponse>("/api/market-sentiment"),
        client.get::<PositionsResponse>("/api/positions/live"),
        client.get::<ScannerResponse>("/api/scanner?direction=long"),
        client.get::<PlaysResponse>("/api/plays/active"),
        client.get::<AlertsResponse>("/api/alerts/triggered"),
        client.get::<ReportResponse>("/api/report/morning/latest?section=summary"),
    );
    BriefSnapshot {
        sentiment,
        positions,
        scanner,
        plays,
        alerts,
        report,
    }
}

pub async fn run() -> Result<()> {
    let cfg = Config::load()?;
    if crate::config::gate_cli_if_demo(&cfg, "brief") {
        return Ok(());
    }
    let client = Client::new(&cfg)?;

    let snap = fetch(&client).await;

    let now = Utc::now().format("%Y-%m-%d %H:%M UTC");
    println!(
        "{} {}  {}",
        f::bold("Carrac"),
        f::dim("·"),
        f::dim(&now.to_string())
    );
    println!();

    macro_section(snap.sentiment.as_ref(), snap.scanner.as_ref());
    positions_section(snap.positions.as_ref());
    scanner_section(snap.scanner.as_ref());
    plays_section(snap.plays.as_ref());
    alerts_section(snap.alerts.as_ref());
    report_section(snap.report.as_ref());

    let _ = write_state_file();
    Ok(())
}

// ─── helpers ─────────────────────────────────────────────────────────────────

fn label(name: &str) -> String {
    let mut s = String::with_capacity(LABEL_WIDTH);
    s.push_str(name);
    while f::strip_ansi_width(&s) < LABEL_WIDTH {
        s.push(' ');
    }
    f::bold(&s)
}

fn print_lines(name: &str, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    for (i, line) in lines.iter().enumerate() {
        if i == 0 {
            println!("{}{}", label(name), line);
        } else {
            println!("{:width$}{}", "", line, width = LABEL_WIDTH);
        }
    }
    println!();
}

fn err_line<E: std::fmt::Display>(name: &str, e: E) {
    println!("{}{}", label(name), f::dim(&format!("unavailable: {e}")));
    println!();
}

// ─── MACRO ──────────────────────────────────────────────────────────────────

fn macro_section(
    sentiment: Result<&SentimentResponse, &anyhow::Error>,
    scanner: Result<&ScannerResponse, &anyhow::Error>,
) {
    let sentiment = match sentiment {
        Ok(s) => s,
        Err(e) => {
            err_line("MACRO", e);
            return;
        }
    };

    let mut lines = Vec::new();
    let regime_str = sentiment.sentiment.regime.as_ref().map_or_else(
        || "Regime —".to_string(),
        |r| format!("Regime {} (score {:+})", regime_color(&r.regime), r.score),
    );
    let fg_str = sentiment.sentiment.fear_greed.as_ref().map_or_else(
        || "F&G —".to_string(),
        |fg| format!("F&G {} ({})", fg.value, fg.classification),
    );
    let breadth_str = sentiment
        .sentiment
        .regime
        .as_ref()
        .and_then(|r| r.breadth().map(|b| format!("Breadth {b}")))
        .unwrap_or_else(|| "Breadth —".to_string());
    lines.push(format!(
        "{}  {}  {}  {}  {}",
        regime_str,
        f::dim("·"),
        fg_str,
        f::dim("·"),
        breadth_str
    ));

    if let Ok(s) = scanner
        && let Some(top) = s
            .results
            .iter()
            .find(|r| r.best_verdict.as_deref() == Some("STRONG"))
    {
        lines.push(format!(
            "Top STRONG long: {}  {}",
            top.symbol,
            f::dim(&verdict_detail(top))
        ));
    }

    print_lines("MACRO", &lines);
}

fn regime_color(regime: &str) -> String {
    match regime {
        "Risk-on" | "Bullish" => f::ok(regime),
        "Cautious" | "Neutral" => f::caution(regime),
        "Risk-off" | "Bearish" => f::alert(regime),
        _ => regime.to_string(),
    }
}

fn verdict_detail(r: &ScannerResult) -> String {
    match &r.long {
        Some(side) => match side.total {
            Some(total) => format!("(long score {total:.0})"),
            None => String::new(),
        },
        None => String::new(),
    }
}

// ─── POSITIONS ──────────────────────────────────────────────────────────────

fn positions_section(positions: Result<&PositionsResponse, &anyhow::Error>) {
    let positions = match positions {
        Ok(p) => p,
        Err(e) => {
            err_line("POSITIONS", e);
            return;
        }
    };

    let heat = positions.portfolio_heat.clone().unwrap_or_default();
    let total = if heat.total_exposure > 0 {
        heat.total_exposure as usize
    } else {
        positions.positions.len()
    };
    let long = heat.long_count;
    let short = heat.short_count;

    let mut lines = vec![format!(
        "{} open  {}  long {}  {}  short {}",
        total,
        f::dim("·"),
        long,
        f::dim("·"),
        short,
    )];

    if let Some((side, p)) = top_extreme(&positions.positions, true) {
        let pnl_v = p.unrealized_pnl_pct.unwrap_or(0.0);
        lines.push(format!(
            "Top {}: {} {} @{} {} {}",
            side,
            p.asset,
            p.direction,
            p.entry_price.map_or_else(|| "—".into(), short_price),
            f::dim("→"),
            f::pnl(&format!("{pnl_v:+.2}%"), pnl_v),
        ));
    }

    let at_risk: Vec<&Position> = positions
        .positions
        .iter()
        .filter(|p| p.risk_status == Some(RiskStatus::AtRisk))
        .collect();
    if !at_risk.is_empty() {
        let names: Vec<String> = at_risk
            .iter()
            .take(3)
            .map(|p| {
                format!(
                    "{} {} {:+.2}%",
                    p.asset,
                    p.direction,
                    p.unrealized_pnl_pct.unwrap_or(0.0)
                )
            })
            .collect();
        lines.push(format!(
            "{}: {} ({})",
            f::caution("At-risk"),
            at_risk.len(),
            names.join(", ")
        ));
    }

    let stopped: Vec<&Position> = positions
        .positions
        .iter()
        .filter(|p| p.risk_status == Some(RiskStatus::StoppedOut))
        .collect();
    if !stopped.is_empty() {
        let names: Vec<String> = stopped
            .iter()
            .take(3)
            .map(|p| format!("{} {}", p.asset, p.direction))
            .collect();
        lines.push(format!(
            "{}: {} ({})",
            f::alert("Stopped out"),
            stopped.len(),
            names.join(", ")
        ));
    }

    print_lines("POSITIONS", &lines);
}

fn top_extreme(positions: &[Position], top: bool) -> Option<(&'static str, &Position)> {
    let valid: Vec<&Position> = positions
        .iter()
        .filter(|p| p.unrealized_pnl_pct.is_some())
        .collect();
    if valid.is_empty() {
        return None;
    }
    if top {
        let p = valid.iter().max_by(|a, b| {
            a.unrealized_pnl_pct
                .unwrap()
                .partial_cmp(&b.unrealized_pnl_pct.unwrap())
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;
        Some(("gainer", *p))
    } else {
        let p = valid.iter().min_by(|a, b| {
            a.unrealized_pnl_pct
                .unwrap()
                .partial_cmp(&b.unrealized_pnl_pct.unwrap())
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;
        Some(("loser", *p))
    }
}

fn short_price(v: f64) -> String {
    if v.abs() >= 1000.0 {
        format!("{v:.0}")
    } else if v.abs() >= 1.0 {
        format!("{v:.2}")
    } else {
        format!("{v:.4}")
    }
}

// ─── SCANNER ────────────────────────────────────────────────────────────────

fn scanner_section(scanner: Result<&ScannerResponse, &anyhow::Error>) {
    let scanner = match scanner {
        Ok(s) => s,
        Err(e) => {
            err_line("SCANNER", e);
            return;
        }
    };
    if scanner.results.is_empty() {
        println!(
            "{}{}",
            label("SCANNER"),
            f::dim("no results (API key scope?)")
        );
        println!();
        return;
    }
    let mut lines = Vec::new();
    let strong: Vec<&ScannerResult> = scanner
        .results
        .iter()
        .filter(|r| r.best_verdict.as_deref() == Some("STRONG"))
        .collect();
    if !strong.is_empty() {
        let syms: Vec<String> = strong.iter().map(|r| r.symbol.clone()).collect();
        lines.push(format!("{} long: {}", f::ok("STRONG"), syms.join(" · ")));
    }
    let good: Vec<&ScannerResult> = scanner
        .results
        .iter()
        .filter(|r| r.best_verdict.as_deref() == Some("GOOD"))
        .take(8)
        .collect();
    if !good.is_empty() {
        let syms: Vec<String> = good.iter().map(|r| r.symbol.clone()).collect();
        lines.push(format!("{} long: {}", f::ok("GOOD"), syms.join(" · ")));
    }
    if lines.is_empty() {
        lines.push(f::dim("no STRONG/GOOD long verdicts"));
    }
    print_lines("SCANNER", &lines);
}

// ─── PLAYS ──────────────────────────────────────────────────────────────────

fn plays_section(plays: Result<&PlaysResponse, &anyhow::Error>) {
    let plays = match plays {
        Ok(p) => p,
        Err(e) => {
            err_line("PLAYS", e);
            return;
        }
    };
    let watching: Vec<_> = plays
        .plays
        .iter()
        .filter(|p| p.status.as_deref() == Some("watching"))
        .collect();
    if watching.is_empty() && plays.plays.is_empty() {
        println!("{}{}", label("PLAYS"), f::dim("(none active)"));
        println!();
        return;
    }
    let mut lines = Vec::new();
    let shown: Vec<_> = if watching.is_empty() {
        plays.plays.iter().collect()
    } else {
        watching
    };
    for p in shown.iter().take(4) {
        let asset = p.primary_asset.clone().unwrap_or_else(|| "—".into());
        let dir = p.direction.clone().unwrap_or_default();
        let progress = p.progress();
        let status = p.status.clone().unwrap_or_else(|| "—".into());
        let title = truncate(&p.title, 72);
        lines.push(format!(
            "{} {} {} {} {}",
            asset,
            f::dim(&dir),
            f::dim(&format!("[{progress}]")),
            f::dim(&format!("[{status}]")),
            title
        ));
    }
    if shown.len() > 4 {
        lines.push(f::dim(&format!("(+{} more)", shown.len() - 4)));
    }
    print_lines("PLAYS", &lines);
}

// ─── ALERTS ─────────────────────────────────────────────────────────────────

fn alerts_section(alerts: Result<&AlertsResponse, &anyhow::Error>) {
    let alerts = match alerts {
        Ok(a) => a,
        Err(e) => {
            err_line("ALERTS", e);
            return;
        }
    };
    if alerts.alerts.is_empty() {
        println!("{}{}", label("ALERTS"), f::dim("(none triggered)"));
        println!();
        return;
    }
    let last_ts = read_state_file().map(|s| s.ts).unwrap_or_default();
    let recent: Vec<_> = alerts
        .alerts
        .iter()
        .filter(|a| {
            a.triggered_at
                .as_deref()
                .is_some_and(|t| t > last_ts.as_str())
        })
        .collect();
    let shown = if recent.is_empty() {
        alerts.alerts.iter().collect()
    } else {
        recent
    };

    let mut lines = vec![format!(
        "{} triggered{}",
        shown.len(),
        if last_ts.is_empty() {
            ""
        } else {
            " since last brief"
        }
    )];
    for a in shown.iter().take(5) {
        let ts = a
            .triggered_at
            .as_deref()
            .and_then(|t| t.split('T').nth(1))
            .map(|s| s.chars().take(5).collect::<String>())
            .unwrap_or_default();
        let sym = a.symbol.clone().unwrap_or_else(|| "—".into());
        let kind = a.alert_type.clone().unwrap_or_else(|| "alert".into());
        let detail = match kind.as_str() {
            "price" => match (a.condition.as_deref(), a.triggered_price.or(a.price)) {
                (Some(c), Some(p)) => format!("price {} {}", c, short_price(p)),
                _ => "price".into(),
            },
            "indicator" => a.indicator.clone().unwrap_or_else(|| "indicator".into()),
            _ => kind.clone(),
        };
        lines.push(format!(
            "{} {} {} {}",
            f::caution("⚡"),
            f::dim(&ts),
            sym,
            detail
        ));
    }
    print_lines("ALERTS", &lines);
}

// ─── REPORT ─────────────────────────────────────────────────────────────────

fn report_section(report: Result<&ReportResponse, &anyhow::Error>) {
    let report = match report {
        Ok(r) => r,
        Err(e) => {
            err_line("REPORT", e);
            return;
        }
    };
    let Some(content) = &report.content else {
        println!("{}{}", label("REPORT"), f::dim("(no morning report yet)"));
        println!();
        return;
    };

    let body = first_paragraph(content, 380);
    let mut lines = Vec::new();
    for line in wrap_text(&body, 70) {
        lines.push(line);
    }
    lines.push(f::dim("…full report at /report/morning"));
    print_lines("REPORT", &lines);
}

// `first_paragraph` is brief-domain logic (markdown report structure) — stays here.
// `wrap_text` and `truncate` are in `crate::helpers::wrap`.
pub(crate) fn first_paragraph(content: &str, max_chars: usize) -> String {
    // Skip leading "## Heading\n\n" sections, take first paragraph of prose.
    let mut buf = String::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            if buf.is_empty() {
                continue;
            }
            break;
        }
        if line.starts_with('#') {
            continue;
        }
        if !buf.is_empty() {
            buf.push(' ');
        }
        buf.push_str(line);
        if buf.chars().count() >= max_chars {
            break;
        }
    }
    truncate(&buf, max_chars)
}

// ─── state file (~/.cache/carrac/last_brief.json on Linux) ───────────────────

#[derive(Serialize, Deserialize, Default)]
struct StateFile {
    #[serde(default)]
    ts: String,
    #[serde(default)]
    version: String,
}

fn state_path() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("cc", "carrac", "carrac")?;
    Some(dirs.cache_dir().join("last_brief.json"))
}

fn read_state_file() -> Option<StateFile> {
    let path = state_path()?;
    let body = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&body).ok()
}

fn write_state_file() -> Result<()> {
    let Some(path) = state_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let state = StateFile {
        ts: Utc::now().to_rfc3339(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    std::fs::write(&path, serde_json::to_string(&state)?)?;
    Ok(())
}
