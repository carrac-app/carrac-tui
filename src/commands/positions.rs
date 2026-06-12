use anyhow::Result;
use chrono::Utc;
use std::collections::HashMap;
use std::io::{Write, stdout};

use crate::client::Client;
use crate::config::Config;
use crate::format as f;
use crate::types::positions::{Direction, PortfolioHeat, Position, PositionsResponse, RiskStatus};

// ─── subcommand enum ─────────────────────────────────────────────────────────

#[derive(clap::Subcommand)]
pub enum PositionsCmd {
    /// Aggregated / detail table view (default)
    List {
        /// Filter to a single asset
        #[arg(long, short)]
        asset: Option<String>,
        /// Show per-position rows instead of asset-aggregated view
        #[arg(long, short)]
        detail: bool,
        /// Poll every N seconds and redraw in place (Ctrl-C to exit)
        #[arg(long, short)]
        watch: Option<u64>,
    },
    /// Card-style render with SL/Entry/TP visual bar
    Show {
        /// Position ID (omit to show all open positions)
        id: Option<u64>,
        /// Filter to a single asset (used when no id given)
        #[arg(long, short)]
        asset: Option<String>,
    },
}

// ─── entry point ─────────────────────────────────────────────────────────────

pub async fn run(
    cmd: Option<PositionsCmd>,
    asset: Option<String>,
    detail: bool,
    watch: Option<u64>,
) -> Result<()> {
    let cfg = Config::load()?;
    if crate::config::gate_cli_if_demo(&cfg, "positions") {
        return Ok(());
    }
    let client = Client::new(&cfg)?;

    match cmd {
        // No subcommand — legacy flags still work
        None => match watch {
            None => render_once(&client, asset.as_deref(), detail).await,
            Some(secs) => watch_loop(&client, asset.as_deref(), detail, secs).await,
        },
        Some(PositionsCmd::List {
            asset,
            detail,
            watch,
        }) => match watch {
            None => render_once(&client, asset.as_deref(), detail).await,
            Some(secs) => watch_loop(&client, asset.as_deref(), detail, secs).await,
        },
        Some(PositionsCmd::Show { id, asset }) => show_cards(&client, id, asset.as_deref()).await,
    }
}

pub(crate) async fn fetch(client: &Client, asset: Option<&str>) -> Result<PositionsResponse> {
    let path = match asset {
        Some(a) => format!("/api/positions/live?asset={a}"),
        None => "/api/positions/live".to_string(),
    };
    client.get::<PositionsResponse>(&path).await
}

async fn render_once(client: &Client, asset: Option<&str>, detail: bool) -> Result<()> {
    let resp = fetch(client, asset).await?;
    print!("{}", render(&resp, detail));
    Ok(())
}

async fn watch_loop(client: &Client, asset: Option<&str>, detail: bool, secs: u64) -> Result<()> {
    use crossterm::{ExecutableCommand, cursor, terminal};
    let interval = std::time::Duration::from_secs(secs.max(2));

    let mut out = stdout();
    out.execute(cursor::Hide).ok();
    out.execute(terminal::Clear(terminal::ClearType::All)).ok();
    out.execute(cursor::MoveTo(0, 0)).ok();

    // Restore cursor on Ctrl-C; default SIGINT handler exits cleanly.
    let _guard = CursorGuard;

    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    loop {
        let frame = match fetch(client, asset).await {
            Ok(r) => render(&r, detail),
            Err(e) => format!("error: {e:#}\n"),
        };

        // Move home and overwrite frame.
        out.execute(cursor::MoveTo(0, 0)).ok();
        out.write_all(frame.as_bytes())?;
        out.execute(terminal::Clear(terminal::ClearType::FromCursorDown))
            .ok();
        out.flush()?;

        tokio::select! {
            _ = &mut ctrl_c => break,
            () = tokio::time::sleep(interval) => {}
        }
    }
    Ok(())
}

struct CursorGuard;
impl Drop for CursorGuard {
    fn drop(&mut self) {
        use crossterm::{ExecutableCommand, cursor};
        let _ = stdout().execute(cursor::Show);
    }
}

pub(crate) fn render(resp: &PositionsResponse, detail: bool) -> String {
    let heat = resp
        .portfolio_heat
        .clone()
        .unwrap_or_else(|| derive_heat(&resp.positions));
    let mut out = String::new();
    out.push_str(&header_line(&resp.positions, &heat));
    out.push('\n');
    if resp.positions.is_empty() {
        out.push('\n');
        out.push_str(&f::dim("(no open positions)\n"));
        return out;
    }
    out.push('\n');
    if detail {
        render_detail(&mut out, &resp.positions);
    } else {
        render_aggregated(&mut out, &resp.positions);
    }
    out
}

fn derive_heat(positions: &[Position]) -> PortfolioHeat {
    let mut h = PortfolioHeat {
        total_exposure: positions.len() as u32,
        ..Default::default()
    };
    for p in positions {
        match p.direction {
            Direction::Long => h.long_count += 1,
            Direction::Short => h.short_count += 1,
        }
    }
    h
}

fn header_line(positions: &[Position], heat: &PortfolioHeat) -> String {
    let at_risk = positions
        .iter()
        .filter(|p| p.risk_status == Some(RiskStatus::AtRisk))
        .count();
    let stopped = positions
        .iter()
        .filter(|p| p.risk_status == Some(RiskStatus::StoppedOut))
        .count();
    let now = Utc::now().format("%Y-%m-%d %H:%M UTC");
    let bar = format!(
        "{} open  {}  long {}  {}  short {}  {}  at-risk {}  {}  stopped {}",
        heat.total_exposure,
        f::dim("·"),
        heat.long_count,
        f::dim("·"),
        heat.short_count,
        f::dim("·"),
        if at_risk > 0 {
            f::caution(&at_risk.to_string())
        } else {
            at_risk.to_string()
        },
        f::dim("·"),
        if stopped > 0 {
            f::alert(&stopped.to_string())
        } else {
            stopped.to_string()
        },
    );
    format!("{}    {}", bar, f::dim(&now.to_string()))
}

// ─── aggregated view ─────────────────────────────────────────────────────────

#[derive(Default, Debug)]
struct Agg {
    count: usize,
    sum_entry: f64,
    n_entry: usize,
    last: Option<f64>,
    sum_pnl: f64,
    n_pnl: usize,
    max_rr: Option<f64>,
    scanner: Option<String>,
    worst_risk: u8, // 0=neutral, 1=at_risk, 2=stopped_out
}

fn risk_rank(s: Option<&RiskStatus>) -> u8 {
    match s {
        Some(RiskStatus::StoppedOut) => 2,
        Some(RiskStatus::AtRisk) => 1,
        _ => 0,
    }
}

fn render_aggregated(out: &mut String, positions: &[Position]) {
    let mut groups: HashMap<(String, Direction), Agg> = HashMap::new();
    for p in positions {
        let key = (p.asset.clone(), p.direction);
        let g = groups.entry(key).or_default();
        g.count += 1;
        if let Some(e) = p.entry_price {
            g.sum_entry += e;
            g.n_entry += 1;
        }
        if g.last.is_none() {
            g.last = p.current_price;
        }
        if let Some(v) = p.unrealized_pnl_pct {
            g.sum_pnl += v;
            g.n_pnl += 1;
        }
        if let Some(rr) = p.current_rr {
            g.max_rr = Some(g.max_rr.map_or(rr, |cur| cur.max(rr)));
        }
        if g.scanner.is_none() {
            g.scanner = p.scanner_verdict.clone();
        }
        g.worst_risk = g.worst_risk.max(risk_rank(p.risk_status.as_ref()));
    }

    // Sort: long before short, then by |sum_pnl| desc.
    let mut rows: Vec<((String, Direction), Agg)> = groups.into_iter().collect();
    rows.sort_by(|a, b| {
        let dir_a = i32::from(a.0.1 != Direction::Long);
        let dir_b = i32::from(b.0.1 != Direction::Long);
        dir_a.cmp(&dir_b).then_with(|| b.0.0.cmp(&a.0.0).reverse())
    });

    let headers = [
        "Asset",
        "Dir",
        "Count",
        "Avg Entry",
        "Last",
        "ΣPnL%",
        "Max RR",
        "Scanner",
    ];
    let aligns = ['l', 'l', 'r', 'r', 'r', 'r', 'r', 'l'];

    let mut cells: Vec<Vec<String>> = Vec::with_capacity(rows.len() + 1);
    cells.push(
        headers
            .iter()
            .map(std::string::ToString::to_string)
            .collect(),
    );

    for ((asset, dir), g) in &rows {
        let avg_entry = if g.n_entry > 0 {
            format_price(g.sum_entry / g.n_entry as f64)
        } else {
            "—".into()
        };
        let last = match g.last {
            Some(v) => format_price(v),
            None => "—".into(),
        };
        let pnl_raw = if g.n_pnl > 0 {
            format!("{:+.2}", g.sum_pnl)
        } else {
            "—".into()
        };
        let pnl_cell = if g.n_pnl > 0 {
            f::pnl(&pnl_raw, g.sum_pnl)
        } else {
            pnl_raw
        };
        let rr = g.max_rr.map_or_else(|| "—".into(), |v| format!("{v:.2}"));
        let scanner = g.scanner.clone().unwrap_or_else(|| "—".into());

        let asset_cell = match g.worst_risk {
            2 => f::alert_dim(asset),
            1 => f::caution(asset),
            _ => asset.clone(),
        };

        cells.push(vec![
            asset_cell,
            dir.to_string(),
            g.count.to_string(),
            avg_entry,
            last,
            pnl_cell,
            rr,
            scanner,
        ]);
    }

    out.push_str(&f::render_table(&cells, &aligns, true));
}

// ─── detail view ─────────────────────────────────────────────────────────────

fn render_detail(out: &mut String, positions: &[Position]) {
    let headers = [
        "ID", "Asset", "Dir", "Entry", "Last", "PnL%", "RR", "Risk", "Scanner",
    ];
    let aligns = ['r', 'l', 'l', 'r', 'r', 'r', 'r', 'l', 'l'];

    let mut sorted: Vec<&Position> = positions.iter().collect();
    sorted.sort_by(|a, b| a.asset.cmp(&b.asset).then(b.id.cmp(&a.id)));

    let mut cells: Vec<Vec<String>> = Vec::with_capacity(sorted.len() + 1);
    cells.push(
        headers
            .iter()
            .map(std::string::ToString::to_string)
            .collect(),
    );

    for p in &sorted {
        let entry = p.entry_price.map_or_else(|| "—".into(), format_price);
        let last = p.current_price.map_or_else(|| "—".into(), format_price);
        let (pnl_raw, pnl_v) = match p.unrealized_pnl_pct {
            Some(v) => (format!("{v:+.2}"), Some(v)),
            None => ("—".into(), None),
        };
        let pnl_cell = match pnl_v {
            Some(v) => f::pnl(&pnl_raw, v),
            None => pnl_raw,
        };
        let rr = p
            .current_rr
            .map_or_else(|| "—".into(), |v| format!("{v:.2}"));
        let risk_cell = match &p.risk_status {
            Some(rs) => f::risk(&rs.to_string(), rs),
            None => "—".into(),
        };
        let scanner = p.scanner_verdict.clone().unwrap_or_else(|| "—".into());

        cells.push(vec![
            p.id.to_string(),
            p.asset.clone(),
            p.direction.to_string(),
            entry,
            last,
            pnl_cell,
            rr,
            risk_cell,
            scanner,
        ]);
    }

    out.push_str(&f::render_table(&cells, &aligns, true));
}

fn format_price(v: f64) -> String {
    let abs = v.abs();
    if abs >= 1000.0 {
        // thin spaces for thousands separators
        let s = format!("{v:.2}");
        thousands_sep(&s)
    } else if abs >= 1.0 {
        format!("{v:.4}")
    } else if abs >= 0.01 {
        format!("{v:.5}")
    } else {
        format!("{v:.6}")
    }
}

fn thousands_sep(s: &str) -> String {
    let (int_part, rest) = match s.find('.') {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    };
    let (sign, digits) = if let Some(stripped) = int_part.strip_prefix('-') {
        ("-", stripped)
    } else {
        ("", int_part)
    };
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + sign.len());
    out.push_str(sign);
    let chars: Vec<char> = digits.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if i > 0 && (chars.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(*c);
    }
    out.push_str(rest);
    out
}

// ─── card show ───────────────────────────────────────────────────────────────

async fn show_cards(client: &Client, id: Option<u64>, asset_filter: Option<&str>) -> Result<()> {
    let resp = fetch(client, asset_filter).await?;

    if resp.positions.is_empty() {
        println!("{}", f::dim("(no open positions)"));
        return Ok(());
    }

    if let Some(target_id) = id {
        if let Some(pos) = resp.positions.iter().find(|p| p.id == target_id) {
            print!("{}", render_card(pos));
        } else {
            eprintln!("position {target_id} not found");
            std::process::exit(1);
        }
    } else {
        // Sort: most recent first (highest id)
        let mut sorted: Vec<&Position> = resp.positions.iter().collect();
        sorted.sort_by_key(|p| std::cmp::Reverse(p.id));
        for pos in sorted {
            print!("{}", render_card(pos));
            println!();
        }
    }
    Ok(())
}

// ─── card render — vertical price ladder ─────────────────────────────────────

/// Card inner width (between box borders).
const CARD_WIDTH: usize = 72;
/// Ladder height in rows (between top and bottom markers, exclusive).
/// Proportional spacing is computed within this budget.
const LADDER_ROWS: usize = 12;
/// Left column: price label + spacer.  "  99 999.99 " = 12 chars.
const PRICE_COL: usize = 12;

/// Render a direction-aware vertical price-ladder card for a single position.
pub fn render_card(pos: &Position) -> String {
    let is_short = pos.direction == Direction::Short;
    let pnl = pos.unrealized_pnl_pct.unwrap_or(0.0);

    let color_on = std::env::var_os("NO_COLOR").is_none()
        && std::io::IsTerminal::is_terminal(&std::io::stdout());

    // ── box top ──
    let dir_badge = if is_short { "[SHORT]" } else { "[LONG]" };
    let dir_colored = if color_on {
        if is_short {
            format!("\x1b[31m{dir_badge}\x1b[0m")
        } else {
            format!("\x1b[32m{dir_badge}\x1b[0m")
        }
    } else {
        dir_badge.to_string()
    };

    // Build plain-text version of header to measure width, then substitute ANSI
    // Format: "─ #ID ─ DIR · ASSET ──────────────"
    // Total inner width must equal CARD_WIDTH.
    let id_part = format!("─ #{} ─ ", pos.id);
    let dir_asset_plain = format!("{} · {}", dir_badge, pos.asset);
    let plain_used = id_part.chars().count() + dir_asset_plain.chars().count();
    let dash_fill = CARD_WIDTH.saturating_sub(plain_used);
    let asset_bold = if color_on {
        f::bold(&pos.asset)
    } else {
        pos.asset.clone()
    };
    let dir_asset_colored = format!("{dir_colored} · {asset_bold}");
    let trailing_dashes = if color_on {
        f::dim(&"─".repeat(dash_fill))
    } else {
        "─".repeat(dash_fill)
    };
    let top = format!("╭{id_part}{dir_asset_colored}{trailing_dashes}╮");

    let mut lines: Vec<String> = Vec::new();
    lines.push(top);

    // ── sub-header: setup · timeframe · opened · age ──
    let setup = pos.setup_type.as_deref().unwrap_or("—");
    let tf = pos.timeframe.as_deref().unwrap_or("—");
    let opened = pos
        .opened_at
        .as_deref()
        .and_then(|s| s.get(0..10))
        .unwrap_or("—");
    let time_in = pos.time_in_trade.as_deref().unwrap_or("");
    let sub = format!(
        " {} · {} · opened {} · {}",
        f::dim(setup),
        f::dim(tf),
        f::dim(opened),
        f::dim(time_in)
    );
    lines.push(card_row(&sub, CARD_WIDTH));
    lines.push(card_blank(CARD_WIDTH));

    // ── ladder ──
    let sl_v = pos.stop_price;
    let tp_v = pos.target_price;
    let entry_v = pos.entry_price;
    let cur_v = pos.current_price;

    let ladder = build_ladder(sl_v, entry_v, tp_v, cur_v, is_short, pnl, color_on);
    for row in &ladder {
        lines.push(card_row(row, CARD_WIDTH));
    }

    lines.push(card_blank(CARD_WIDTH));

    // ── footer: PnL · RR · Risk · Scanner ──
    let pnl_str = match pos.unrealized_pnl_pct {
        Some(v) => {
            let raw = format!("{v:+.2}%");
            if color_on { f::pnl(&raw, v) } else { raw }
        }
        None => "—".to_string(),
    };
    let usd_str = pos
        .unrealized_pnl_usd
        .map(|v| format!(" ({}{:.0})", if v >= 0.0 { "+" } else { "" }, v))
        .unwrap_or_default();
    let cur_rr = pos.current_rr.map_or("—".into(), |v| format!("{v:+.2}"));
    let plan_rr = pos.planned_rr.map_or("—".into(), |v| format!("{v:.2}"));
    let risk_label = match pos.risk_status.as_ref() {
        Some(RiskStatus::AtRisk) => {
            let s = "AT RISK";
            if color_on { f::caution(s) } else { s.into() }
        }
        Some(RiskStatus::StoppedOut) => {
            let s = "STOPPED OUT";
            if color_on { f::alert(s) } else { s.into() }
        }
        _ => {
            let s = "SAFE";
            if color_on { f::ok(s) } else { s.into() }
        }
    };
    let scanner_v = pos.scanner_verdict.as_deref().unwrap_or("—");
    let scanner_colored = if color_on {
        let sl_lower = scanner_v.to_lowercase();
        if sl_lower.contains("good") || sl_lower.contains("strong") {
            f::ok(scanner_v)
        } else if sl_lower.contains("weak") || sl_lower.contains("poor") {
            f::alert(scanner_v)
        } else {
            f::dim(scanner_v)
        }
    } else {
        scanner_v.to_string()
    };

    let footer = format!(
        " PnL {}{}    RR {} / {}    Risk: {}    Scanner: {}",
        pnl_str,
        f::dim(&usd_str),
        if color_on {
            f::pnl(&cur_rr, pos.current_rr.unwrap_or(0.0))
        } else {
            cur_rr
        },
        f::dim(&plan_rr),
        risk_label,
        scanner_colored,
    );
    lines.push(card_row(&footer, CARD_WIDTH));

    // ── linked play ──
    if let Some(ref lp) = pos.linked_play {
        let play_row = if color_on {
            format!(
                " Play #{}: {}  {}",
                lp.id,
                f::dim(&lp.title),
                lp.conditions_met
                    .as_deref()
                    .map(|c| format!("[{c}]"))
                    .unwrap_or_default()
            )
        } else {
            format!(" Play #{}: {}", lp.id, lp.title)
        };
        lines.push(card_row(&play_row, CARD_WIDTH));
    }

    // ── box bottom ──
    lines.push(format!("╰{}╯", "─".repeat(CARD_WIDTH)));

    let mut out = lines.join("\n");
    out.push('\n');
    out
}

// ─── vertical ladder builder ─────────────────────────────────────────────────

/// A marker placed on the ladder at a specific price.
#[derive(Clone)]
struct Marker {
    price: f64,
    label: &'static str, // "SL" | "entry" | "live" | "TP"
}

/// Build the vertical price ladder rows.
/// Prices are always shown high→low (top→bottom), regardless of direction.
/// Returns a Vec<String> of inner content (without box borders — `card_row` adds those).
fn build_ladder(
    sl: Option<f64>,
    entry: Option<f64>,
    tp: Option<f64>,
    cur: Option<f64>,
    is_short: bool,
    pnl: f64,
    color_on: bool,
) -> Vec<String> {
    // Collect all known prices
    let prices: Vec<(f64, &'static str)> = {
        let mut v = Vec::new();
        if let Some(p) = sl {
            v.push((p, "SL"));
        }
        if let Some(p) = entry {
            v.push((p, "entry"));
        }
        if let Some(p) = tp {
            v.push((p, "TP"));
        }
        v
    };

    if prices.is_empty() {
        // No price data at all — show plain entry/cur fallback
        let mut rows = Vec::new();
        if let Some(e) = entry {
            rows.push(format!(
                "  entry: {}  current: {}",
                format_price(e),
                cur.map_or("—".into(), format_price)
            ));
        } else {
            rows.push("  (no price data)".into());
        }
        return rows;
    }

    // Price range for interpolation: max (top) and min (bottom)
    let mut all_prices: Vec<f64> = prices.iter().map(|(p, _)| *p).collect();
    if let Some(c) = cur {
        all_prices.push(c);
    }
    let price_max = all_prices.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let price_min = all_prices.iter().copied().fold(f64::INFINITY, f64::min);
    let range = (price_max - price_min).max(1e-9);

    // Map a price to a row index [0..LADDER_ROWS] where 0 = top (highest price)
    let price_to_row = |p: f64| -> usize {
        let frac = (price_max - p) / range;
        let r = (frac * LADDER_ROWS as f64).round() as isize;
        r.clamp(0, LADDER_ROWS as isize) as usize
    };

    // Determine which row live price falls on; handle out-of-range
    let cur_row_info: Option<(usize, bool)> = cur.map(|c| {
        let raw = (price_max - c) / range * LADDER_ROWS as f64;
        let clamped = raw.round() as isize;
        let row = clamped.clamp(0, LADDER_ROWS as isize) as usize;
        let off_ladder = raw < 0.0 || raw > LADDER_ROWS as f64;
        (row, off_ladder)
    });

    // Build row map: row_index → Vec<Marker>
    let mut row_map: Vec<Vec<Marker>> = vec![vec![]; LADDER_ROWS + 1];
    for (p, lbl) in &prices {
        let r = price_to_row(*p);
        row_map[r].push(Marker {
            price: *p,
            label: lbl,
        });
    }
    // If live is on-ladder, add it
    if let (Some(c), Some((row, false))) = (cur, cur_row_info) {
        row_map[row].push(Marker {
            price: c,
            label: "live",
        });
    }

    // Build text rows
    let mut rows: Vec<String> = Vec::with_capacity(LADDER_ROWS + 4);

    // If live price is above the ladder (off-ladder top)
    if let (Some(c), Some((_, true))) = (cur, cur_row_info) {
        let frac = (price_max - c) / range * LADDER_ROWS as f64;
        if frac < 0.0 {
            let price_str = format_price(c);
            let pnl_raw = format!("{pnl:+.2}%");
            let live_content = if color_on {
                format!(
                    "  ▲ {} live {} ← off-ladder",
                    format_price(c),
                    f::pnl(&pnl_raw, pnl)
                )
            } else {
                format!("  ^ {price_str} live {pnl_raw} [off-ladder above]")
            };
            rows.push(live_content);
        }
    }

    for markers in &row_map {
        if markers.is_empty() {
            // Spacing row — dim vertical line
            let spine = if color_on {
                format!("{}  {}", " ".repeat(PRICE_COL), f::dim("┊"))
            } else {
                format!("{}  |", " ".repeat(PRICE_COL))
            };
            rows.push(spine);
        } else {
            // Sort markers on this row: by label priority (SL/TP boundaries first, then entry, then live)
            let label_priority = |lbl: &str| match lbl {
                "SL" => 0,
                "TP" => 1,
                "entry" => 2,
                "live" => 3,
                _ => 4,
            };
            let mut sorted_markers = markers.clone();
            sorted_markers.sort_by_key(|m| label_priority(m.label));

            for m in &sorted_markers {
                let price_str = format_price(m.price);
                let price_col = format!("{price_str:>10}  "); // right-aligned, 10 chars + 2 spaces

                let pct_from_entry: Option<f64> = entry.map(|e| {
                    if is_short {
                        (e - m.price) / e * 100.0
                    } else {
                        (m.price - e) / e * 100.0
                    }
                });

                let (label_colored, pct_colored, annotation): (String, String, String) =
                    match m.label {
                        "SL" => {
                            let pct = pct_from_entry.map_or("—".into(), |v| format!("{v:+.1}%"));
                            let worst_ann = if color_on {
                                f::dim("← worst")
                            } else {
                                "← worst".into()
                            };
                            let label = if color_on {
                                f::alert("─── SL ──────────")
                            } else {
                                "─── SL ──────────".into()
                            };
                            let pct_s = if color_on { f::alert(&pct) } else { pct };
                            (label, pct_s, worst_ann)
                        }
                        "entry" => {
                            let pct = pct_from_entry.map_or("0.0%".into(), |v| format!("{v:+.1}%"));
                            let label = if color_on {
                                f::dim("─── entry ────────")
                            } else {
                                "─── entry ────────".into()
                            };
                            let pct_s = if color_on { f::dim(&pct) } else { pct };
                            (label, pct_s, String::new())
                        }
                        "live" => {
                            let cur_price = cur.unwrap_or(m.price);
                            let cur_fmt = format_price(cur_price);
                            let pnl_raw = format!("{pnl:+.2}%");
                            let pnl_s = if color_on {
                                f::pnl(&pnl_raw, pnl)
                            } else {
                                pnl_raw
                            };
                            let bullet = if color_on {
                                f::pnl("●", pnl)
                            } else {
                                "●".into()
                            };
                            let label = format!("─── {bullet} live ──");
                            let ann = if color_on {
                                f::dim(&format!("({cur_fmt})"))
                            } else {
                                format!("({cur_fmt})")
                            };
                            (label, pnl_s, ann)
                        }
                        "TP" => {
                            let pct = pct_from_entry.map_or("—".into(), |v| format!("{v:+.1}%"));
                            let best_ann = if color_on {
                                f::dim("← best")
                            } else {
                                "← best".into()
                            };
                            let label = if color_on {
                                f::ok("─── TP ───────────")
                            } else {
                                "─── TP ───────────".into()
                            };
                            let pct_s = if color_on { f::ok(&pct) } else { pct };
                            (label, pct_s, best_ann)
                        }
                        _ => (m.label.into(), String::new(), String::new()),
                    };

                let row_content = if annotation.is_empty() {
                    format!("{price_col}{label_colored}  {pct_colored}")
                } else {
                    format!("{price_col}{label_colored}  {pct_colored}  {annotation}")
                };
                rows.push(row_content);
            }
        }
    }

    // If live price is below the ladder (off-ladder bottom)
    if let (Some(c), Some((_, true))) = (cur, cur_row_info) {
        let frac = (price_max - c) / range * LADDER_ROWS as f64;
        if frac > LADDER_ROWS as f64 {
            let pnl_raw = format!("{pnl:+.2}%");
            let live_content = if color_on {
                format!(
                    "  ▼ {} live {} ← off-ladder",
                    format_price(c),
                    f::pnl(&pnl_raw, pnl)
                )
            } else {
                format!(
                    "  v {} live {} [off-ladder below]",
                    format_price(c),
                    pnl_raw
                )
            };
            rows.push(live_content);
        }
    }

    rows
}

/// Pad (or truncate) a card inner row to exactly `CARD_WIDTH` visible chars, wrap in box borders.
fn card_row(content: &str, width: usize) -> String {
    let visible = f::strip_ansi_width(content);
    if visible <= width {
        let pad = width - visible;
        format!("│{}{}│", content, " ".repeat(pad))
    } else {
        // Truncate: walk chars, counting visible width, cut at `width - 1`, add ellipsis
        let mut out = String::new();
        let mut count = 0usize;
        let mut chars = content.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' && chars.peek() == Some(&'[') {
                out.push(c);
                out.push(chars.next().unwrap());
                // `chars` is shared with the outer loop, so a `for` (which would
                // move the iterator) isn't applicable here.
                #[allow(clippy::while_let_on_iterator)]
                while let Some(nc) = chars.next() {
                    out.push(nc);
                    if nc.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                if count >= width.saturating_sub(1) {
                    out.push('…');
                    break;
                }
                out.push(c);
                count += 1;
            }
        }
        let final_visible = f::strip_ansi_width(&out);
        let pad = width.saturating_sub(final_visible);
        format!("│{}{}│", out, " ".repeat(pad))
    }
}

fn card_blank(width: usize) -> String {
    format!("│{}│", " ".repeat(width))
}
