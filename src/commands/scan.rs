use anyhow::Result;
use chrono::Utc;

use crate::client::Client;
use crate::commands::chart::fmt_price;
use crate::config::Config;
use crate::format as f;
use crate::helpers::batch_fetch::fetch_batch;
use crate::helpers::sse::collect_sse_tokens;
use crate::helpers::watchlist;
use crate::types::batch::{AssetBatch, BatchResponse};

// ─── clap arg structs ────────────────────────────────────────────────────────

#[derive(clap::ValueEnum, Clone, Debug, Default, PartialEq)]
pub enum Direction {
    #[default]
    Both,
    Long,
    Short,
}

#[derive(clap::ValueEnum, Clone, Debug, Default)]
pub enum SortKey {
    #[default]
    Score,
    Symbol,
    Change,
}

#[derive(clap::Args)]
pub struct ScanArgs {
    /// Symbols to scan (comma-separated positional args; default: watchlist)
    #[arg(value_delimiter = ',')]
    pub assets: Vec<String>,

    /// Ad-hoc asset list (comma-separated; overrides watchlist and positional args)
    #[arg(long = "asset", value_delimiter = ',')]
    pub asset_flag: Vec<String>,

    /// Show only STRONG verdicts
    #[arg(long)]
    pub strong: bool,

    /// Filter direction: both | long | short
    #[arg(long, value_enum, default_value = "both")]
    pub direction: Direction,

    /// After fetching, pipe context to /api/ai/chat and print the AI response
    #[arg(long = "batch-ai")]
    pub batch_ai: Option<String>,

    /// Sort column: score | symbol | change
    #[arg(long, value_enum, default_value = "score")]
    pub sort: SortKey,

    /// Max rows to display
    #[arg(long, default_value_t = 20)]
    pub limit: usize,

    /// Emit raw JSON (for piping to jq)
    #[arg(long)]
    pub json: bool,
}

// ─── entry point ─────────────────────────────────────────────────────────────

pub async fn run(args: ScanArgs) -> Result<()> {
    let cfg = Config::load()?;
    if crate::config::gate_cli_if_demo(&cfg, "scan") {
        return Ok(());
    }
    let client = Client::new(&cfg)?;

    // Resolve asset list: --asset flag > positional args > watchlist
    let (symbols, from_watchlist) = if !args.asset_flag.is_empty() {
        (args.asset_flag.clone(), false)
    } else if !args.assets.is_empty() {
        (args.assets.clone(), false)
    } else {
        (watchlist::load(), true)
    };

    if from_watchlist && symbols.is_empty() {
        eprintln!(
            "carrac: watchlist is empty — add symbols to ~/.config/carrac/carrac/watchlist.toml"
        );
        return Ok(());
    }

    if args.batch_ai.is_some() {
        eprintln!("carrac: [fetching {} assets…]", symbols.len());
    }

    let batch = fetch_batch(&client, &symbols, &["scanner", "market", "plays"]).await?;

    if batch.assets.is_empty() {
        println!(
            "{}",
            f::dim("no scanner data for these assets right now (try again in 30 s)")
        );
        return Ok(());
    }

    if args.json {
        let json = serde_json::to_string_pretty(&batch.assets).unwrap_or_else(|_| "{}".to_string());
        println!("{json}");
        return Ok(());
    }

    // Build rows
    let mut rows = build_rows(&batch, &args);

    // Availability warning
    let scanner_avail = batch.availability.scanner.as_deref().unwrap_or("ok");
    let avail_note = if scanner_avail != "ok" {
        Some(format!("{} scanner data may be stale", f::caution("⚠")))
    } else {
        None
    };

    // Sort
    match args.sort {
        SortKey::Score => rows.sort_by_key(|r| std::cmp::Reverse(r.score)),
        SortKey::Symbol => rows.sort_by(|a, b| a.symbol.cmp(&b.symbol)),
        SortKey::Change => rows.sort_by(|a, b| {
            b.change
                .partial_cmp(&a.change)
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
    }

    // Filter: --strong
    let total_before = rows.len();
    if args.strong {
        rows.retain(|r| r.verdict.to_uppercase() == "STRONG");
    }
    let hidden = total_before - rows.len();

    // --limit
    let shown = rows.len().min(args.limit);
    rows.truncate(shown);

    // Header
    let now = Utc::now().format("%Y-%m-%d %H:%M UTC");
    let dir_label = match args.direction {
        Direction::Both => "both",
        Direction::Long => "long",
        Direction::Short => "short",
    };
    let watchlist_label = if from_watchlist {
        format!("  {}  (watchlist: {} assets)", f::dim("·"), symbols.len())
    } else {
        String::new()
    };
    println!(
        "{}  {}  {}  {}{}",
        f::bold("Carrac"),
        f::dim("·"),
        f::dim("scanner"),
        f::dim(&now.to_string()),
        watchlist_label,
    );
    if dir_label != "both" {
        println!("{} direction: {dir_label}", f::dim("filter:"));
    }
    println!();

    // Table
    let plain_table = render_scan_table(&rows);
    print!("{plain_table}");

    // --strong footer
    if args.strong && hidden > 0 {
        println!();
        println!(
            "{}",
            f::dim(&format!(
                "{} STRONG  ·  {hidden} below threshold (hidden)",
                rows.len()
            ))
        );
    }

    // Missing data note
    let returned = batch.assets.len();
    if returned < symbols.len() {
        let missing = symbols.len() - returned;
        println!(
            "{}",
            f::dim(&format!(
                "{missing} of {} symbols returned no data",
                symbols.len()
            ))
        );
    }

    // Availability note
    if let Some(note) = avail_note {
        println!("{note}");
    }
    println!(
        "{}",
        f::dim(&format!(
            "availability: scanner {} · prices {}",
            scanner_avail,
            batch.availability.prices.as_deref().unwrap_or("ok"),
        ))
    );

    // --batch-ai
    if let Some(prompt) = &args.batch_ai {
        println!();
        run_batch_ai(&client, &plain_table, prompt).await;
    }

    Ok(())
}

// ─── row type ─────────────────────────────────────────────────────────────────

struct ScanRow {
    symbol: String,
    dir: String,
    score: i32,
    verdict: String,
    price: f64,
    change: f64,
}

// ─── build rows ───────────────────────────────────────────────────────────────

fn build_rows(batch: &BatchResponse, args: &ScanArgs) -> Vec<ScanRow> {
    let mut rows = Vec::new();

    for (symbol, asset) in &batch.assets {
        let Some(scanner) = &asset.scanner else {
            // Asset with no scanner data — show placeholder row
            rows.push(ScanRow {
                symbol: symbol.clone(),
                dir: "—".into(),
                score: -1,
                verdict: "—".into(),
                price: asset.market.as_ref().and_then(|m| m.price).unwrap_or(0.0),
                change: asset
                    .market
                    .as_ref()
                    .and_then(|m| m.change_24h)
                    .unwrap_or(0.0),
            });
            continue;
        };

        // Determine which side(s) to show
        match args.direction {
            Direction::Long => {
                if let Some(row) = scanner_row(symbol, "long", scanner.long.as_ref(), asset) {
                    rows.push(row);
                }
            }
            Direction::Short => {
                if let Some(row) = scanner_row(symbol, "short", scanner.short.as_ref(), asset) {
                    rows.push(row);
                }
            }
            Direction::Both => {
                // Show the best side (highest score), or the best_verdict side
                let long_score = scanner.long.as_ref().map(|s| s.score).unwrap_or(0);
                let short_score = scanner.short.as_ref().map(|s| s.score).unwrap_or(0);
                if long_score >= short_score {
                    if let Some(row) = scanner_row(symbol, "long", scanner.long.as_ref(), asset) {
                        rows.push(row);
                    }
                } else if let Some(row) =
                    scanner_row(symbol, "short", scanner.short.as_ref(), asset)
                {
                    rows.push(row);
                }
            }
        }
    }

    rows
}

fn scanner_row(
    symbol: &str,
    dir: &str,
    side: Option<&crate::types::batch::ScannerSide>,
    asset: &AssetBatch,
) -> Option<ScanRow> {
    let (score, verdict) = match side {
        Some(s) => (s.score, s.verdict.clone()),
        None => (0, "—".into()),
    };
    let price = asset.market.as_ref().and_then(|m| m.price).unwrap_or(0.0);
    let change = asset
        .market
        .as_ref()
        .and_then(|m| m.change_24h)
        .unwrap_or(0.0);
    Some(ScanRow {
        symbol: symbol.to_string(),
        dir: dir.to_string(),
        score,
        verdict,
        price,
        change,
    })
}

// ─── table renderer ───────────────────────────────────────────────────────────

fn verdict_styled(v: &str) -> String {
    match v.to_uppercase().as_str() {
        "STRONG" => f::ok(v),
        "GOOD" => f::ok(v),
        "WATCH" => f::caution(v),
        "NEUTRAL" => f::dim(v),
        "WEAK" => f::dim(v),
        "AVOID" => f::alert_dim(v),
        _ => v.to_string(),
    }
}

/// Price for the scan table — dash for an absent (zero) price, else the shared
/// canonical formatter.
fn fmt_price_or_dash(p: f64) -> String {
    if p == 0.0 {
        "—".to_string()
    } else {
        fmt_price(p)
    }
}

fn fmt_change(c: f64) -> String {
    if c == 0.0 {
        return "—".to_string();
    }
    format!("{:+.2}%", c)
}

/// Renders a plain-text (ANSI-coloured) scan table and also returns an ANSI-free
/// copy for feeding to the AI.
fn render_scan_table(rows: &[ScanRow]) -> String {
    if rows.is_empty() {
        return format!("{}\n", f::dim("(no results)"));
    }

    let headers = ["Symbol", "Dir", "Score", "Verdict", "Price", "Chg 24h"];
    let aligns = ['l', 'l', 'r', 'l', 'r', 'r'];

    let mut cells: Vec<Vec<String>> = Vec::with_capacity(rows.len() + 1);
    cells.push(headers.iter().map(|s| s.to_string()).collect());

    for row in rows {
        let score_str = if row.score < 0 {
            "—".to_string()
        } else {
            row.score.to_string()
        };
        let verdict_str = if row.verdict == "—" {
            "—".to_string()
        } else {
            verdict_styled(&row.verdict)
        };
        cells.push(vec![
            row.symbol.clone(),
            row.dir.clone(),
            score_str,
            verdict_str,
            fmt_price_or_dash(row.price),
            fmt_change(row.change),
        ]);
    }

    f::render_table(&cells, &aligns, true)
}

// ─── --batch-ai ───────────────────────────────────────────────────────────────

async fn run_batch_ai(client: &Client, plain_table: &str, prompt: &str) {
    // Strip ANSI from table for the AI context
    let stripped = strip_ansi(plain_table);
    let context = format!(
        "You are reviewing a multi-asset scanner snapshot. Here's the current state:\n\n{stripped}\n\nUser question: {prompt}\n\nReply concisely in markdown.",
    );

    eprint!("▴ thinking…");

    let body = serde_json::json!({
        "message": context,
        "page_context": "scanner"
    });

    // /api/ai/chat returns SSE: `data: {"t": "token"}\n\n` lines
    // We use the raw reqwest response to stream-collect the text tokens.
    match client.post_raw("/api/ai/chat", &body).await {
        Ok(text) => {
            eprint!("\r          \r"); // clear spinner
            let (tokens, sse_err) = collect_sse_tokens(&text);
            if tokens.is_empty() {
                if let Some(err) = sse_err {
                    println!("[AI unavailable: {err}]");
                } else {
                    println!("{}", f::dim("AI returned an empty response."));
                }
            } else {
                println!("{tokens}");
                if let Some(err) = sse_err {
                    println!();
                    println!("_(note: AI tool call failed mid-response: {err})_");
                }
            }
        }
        Err(e) => {
            eprint!("\r          \r");
            println!(
                "{}",
                f::dim(&format!(
                    "AI request failed: {e}. Try again or use `carrac ask` directly."
                ))
            );
        }
    }
}

fn strip_ansi(s: &str) -> String {
    f::strip_ansi(s)
}
