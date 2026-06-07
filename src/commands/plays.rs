use anyhow::Result;
use chrono::Utc;
use serde_json::json;

use crate::client::Client;
use crate::config::Config;
use crate::format as f;
use crate::helpers::wrap::{truncate, wrap_text};
use crate::types::plays::{EvalResponse, Play, PlayDetailResponse, PlaysResponse};

const THESIS_PREVIEW_CHARS: usize = 400;
const WRAP_WIDTH: usize = 72;
const RULE_WRAP_WIDTH: usize = 68; // indented 2 spaces inside rule block

// ─── clap arg structs ────────────────────────────────────────────────────────

#[derive(clap::Args)]
pub struct PlaysListArgs {
    /// Filter by status: watching | active | completed | invalidated | all
    /// [default: watching]
    #[arg(long, default_value = "watching")]
    pub status: String,

    /// Filter by primary asset (e.g. BTC, ETH)
    #[arg(long)]
    pub asset: Option<String>,

    /// Max rows to display
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
}

#[derive(clap::Args)]
pub struct PlaysShowArgs {
    /// Play ID
    pub play_id: u32,

    /// Show only condition rules + met/unmet state
    #[arg(long)]
    pub conditions: bool,

    /// Show only the action_plan block
    #[arg(long)]
    pub action: bool,
}

#[derive(clap::Subcommand)]
pub enum PlaysCmd {
    /// List plays (default: watching status)
    List(PlaysListArgs),
    /// Full play detail: thesis, conditions, action_plan
    Show(PlaysShowArgs),
    /// Force re-evaluate conditions via POST /api/plays/{id}/evaluate
    Eval {
        /// Play ID
        play_id: u32,
    },
}

// ─── shared fetch ────────────────────────────────────────────────────────────

/// Fetch active (watching-status) plays. Used by both CLI and TUI fetchers.
pub(crate) async fn fetch_active(client: &Client) -> Result<crate::types::plays::PlaysResponse> {
    client
        .get::<crate::types::plays::PlaysResponse>("/api/plays/active")
        .await
}

// ─── entry point ─────────────────────────────────────────────────────────────

pub async fn run(cmd: PlaysCmd) -> Result<()> {
    let cfg = Config::load()?;
    if crate::config::gate_cli_if_demo(&cfg, "plays") {
        return Ok(());
    }
    let client = Client::new(&cfg)?;
    match cmd {
        PlaysCmd::List(args) => list(&client, args).await,
        PlaysCmd::Show(args) => show(&client, args).await,
        PlaysCmd::Eval { play_id } => eval(&client, play_id).await,
    }
}

// ─── list ────────────────────────────────────────────────────────────────────

async fn list(client: &Client, args: PlaysListArgs) -> Result<()> {
    let mut path = format!("/api/plays?limit={}", args.limit);

    if args.status == "all" {
        // No status filter — returns all statuses
    } else {
        path.push_str(&format!("&status={}", args.status));
    }

    if let Some(ref asset) = args.asset {
        path.push_str(&format!("&asset={}", asset));
    }

    let resp = client.get::<PlaysResponse>(&path).await?;

    let now = Utc::now().format("%Y-%m-%d %H:%M UTC");
    let filter_label = if args.status == "all" {
        "all".to_string()
    } else {
        args.status.clone()
    };
    let asset_label = args
        .asset
        .as_deref()
        .map(|a| format!(" · {a}"))
        .unwrap_or_default();
    println!(
        "{}  {}  {}   ({}{}: {})",
        f::bold("Carrac"),
        f::dim("·"),
        f::dim(&now.to_string()),
        filter_label,
        asset_label,
        resp.plays.len(),
    );
    println!();

    if resp.plays.is_empty() {
        println!("{}", f::dim("(no plays)"));
        return Ok(());
    }

    let headers = ["ID", "Asset", "Dir", "TF", "Status", "Conditions", "Title"];
    let aligns = ['r', 'l', 'l', 'l', 'l', 'l', 'l'];

    let mut cells: Vec<Vec<String>> = Vec::with_capacity(resp.plays.len() + 1);
    cells.push(headers.iter().map(|s| s.to_string()).collect());

    for play in &resp.plays {
        let id = play.id.to_string();
        let asset = play.primary_asset.clone().unwrap_or_else(|| "—".into());
        let dir = play.direction.clone().unwrap_or_else(|| "—".into());
        let tf = play.timeframe.clone().unwrap_or_else(|| "—".into());
        let status = play.status.clone().unwrap_or_else(|| "—".into());
        let (met, total) = play.condition_counts();
        let cond_str = if total == 0 {
            "—".to_string()
        } else if met == total {
            format!("{met}/{total} ✓")
        } else {
            format!("{met}/{total}")
        };
        let title = truncate(&play.title, 60);
        cells.push(vec![id, asset, dir, tf, status, cond_str, title]);
    }

    print!("{}", f::render_table(&cells, &aligns, true));
    Ok(())
}

// ─── show ────────────────────────────────────────────────────────────────────

async fn show(client: &Client, args: PlaysShowArgs) -> Result<()> {
    let path = format!("/api/plays/{}", args.play_id);
    let resp = match client.get::<PlayDetailResponse>(&path).await {
        Ok(r) => r,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("404") {
                eprintln!("play {} not found", args.play_id);
                std::process::exit(1);
            }
            return Err(e);
        }
    };
    let play = resp.play;

    // --conditions only
    if args.conditions {
        render_conditions_only(&play);
        return Ok(());
    }

    // --action only
    if args.action {
        render_action_only(&play);
        return Ok(());
    }

    render_full(&play);
    Ok(())
}

fn separator() -> String {
    "─".repeat(65)
}

fn render_full(play: &Play) {
    let sep = separator();
    let asset = play.primary_asset.as_deref().unwrap_or("—");
    let dir = play.direction.as_deref().unwrap_or("—");
    let tf = play.timeframe.as_deref().unwrap_or("—");
    let status = play.status.as_deref().unwrap_or("—");

    println!("{}", f::dim(&sep));
    println!(
        "{}  {}  {}  {}  {}  {}",
        f::bold(&format!("Play {}", play.id)),
        f::dim("·"),
        f::bold(asset),
        f::dim("·"),
        dir,
        f::dim(&format!("· {tf} · {status}"))
    );
    println!("{}", f::dim(&sep));
    println!();

    // Thesis
    if let Some(thesis) = &play.thesis {
        let preview = truncate(thesis, THESIS_PREVIEW_CHARS);
        let total_chars = thesis.chars().count();
        println!("{}", f::bold("THESIS"));
        for line in wrap_text(&preview, WRAP_WIDTH) {
            println!("  {line}");
        }
        if total_chars > THESIS_PREVIEW_CHARS {
            println!(
                "  {}",
                f::dim(&format!("[truncated — {total_chars} chars total]"))
            );
        }
        println!();
    }

    // Conditions
    if let Some(cond) = &play.conditions {
        let total = cond.rules.len();
        let met_count = cond.rules.iter().filter(|r| r.met).count();
        let logic = cond.logic.as_deref().unwrap_or("all");
        println!(
            "{}",
            f::bold(&format!(
                "CONDITIONS  (logic: {} — {met_count}/{total} met)",
                logic
            ))
        );
        // Show first rule in full, summarise rest
        for (i, rule) in cond.rules.iter().enumerate() {
            let icon = if rule.met {
                f::ok("✓")
            } else {
                f::dim("✗")
            };
            let desc = rule.description.as_deref().unwrap_or("(no description)");
            let desc_wrapped = wrap_text(desc, RULE_WRAP_WIDTH);
            println!();
            if let Some(first) = desc_wrapped.first() {
                println!("  {}  {}", icon, first);
            }
            for line in desc_wrapped.iter().skip(1) {
                println!("     {line}");
            }
            if let Some(ev) = &rule.evidence {
                println!("     {}", f::dim(&format!("Evidence: {ev}")));
            }
            if i == 0 && total > 1 {
                let remaining = total - 1;
                println!();
                println!(
                    "  {}",
                    f::dim(&format!(
                        "({remaining} more rule{} — use --conditions to expand all)",
                        if remaining == 1 { "" } else { "s" }
                    ))
                );
                break;
            }
        }
        println!();
    }

    // Invalidation
    if let Some(inv) = &play.invalidation {
        let icon = if inv.met {
            f::alert("✗ MET")
        } else {
            f::dim("✗ not met")
        };
        println!("{}", f::bold("INVALIDATION"));
        if let Some(desc) = &inv.description {
            for line in wrap_text(desc, WRAP_WIDTH) {
                println!("  {line}");
            }
        }
        if let Some(ev) = &inv.evidence {
            println!("  {} {}", icon, f::dim(ev));
        }
        println!();
    }

    // Expiry / timestamps
    let expiry = play.expiry.as_deref().unwrap_or("none");
    let updated = play.updated_at.as_deref().unwrap_or("—");
    println!("{}  {}", f::bold("EXPIRES"), expiry);
    println!("{}  {}", f::bold("UPDATED"), updated);
    println!();
    println!("{}", f::dim(&sep));
}

fn render_conditions_only(play: &Play) {
    let asset = play.primary_asset.as_deref().unwrap_or("—");
    let dir = play.direction.as_deref().unwrap_or("—");
    let Some(cond) = &play.conditions else {
        println!("{}", f::dim("conditions: (none)"));
        return;
    };
    let total = cond.rules.len();
    let met_count = cond.rules.iter().filter(|r| r.met).count();
    let logic = cond.logic.as_deref().unwrap_or("all");
    let asset_dir = format!("{asset} {dir}");
    println!(
        "Conditions  play {}  {}  {}  logic: {}  {}/{} met",
        play.id,
        f::dim("·"),
        asset_dir,
        logic,
        met_count,
        total,
    );
    println!();
    for (i, rule) in cond.rules.iter().enumerate() {
        let icon = if rule.met {
            f::ok("✓")
        } else {
            f::alert("✗")
        };
        let desc = rule.description.as_deref().unwrap_or("(no description)");
        let desc_wrapped = wrap_text(desc, RULE_WRAP_WIDTH);
        print!("{}  [rule {}]  ", icon, i + 1);
        if let Some(first) = desc_wrapped.first() {
            println!("{first}");
        } else {
            println!();
        }
        for line in desc_wrapped.iter().skip(1) {
            println!("   {line}");
        }
        if let Some(ev) = &rule.evidence {
            println!("   {}", f::dim(&format!("Evidence: {ev}")));
        }
        println!();
    }
}

fn render_action_only(play: &Play) {
    let Some(plan) = &play.action_plan else {
        println!("{}", f::dim("action_plan: (none)"));
        return;
    };
    let asset = play.primary_asset.as_deref().unwrap_or("—");
    let dir = play.direction.as_deref().unwrap_or("—");
    println!(
        "{}",
        f::bold(&format!("Action Plan  play {}  · {asset} {dir}", play.id))
    );
    println!();
    // Preserve paragraph breaks; wrap each paragraph individually
    for para in plan.split("\n\n") {
        for line in para.lines() {
            if line.is_empty() {
                continue;
            }
            for wrapped in wrap_text(line, WRAP_WIDTH) {
                println!("  {wrapped}");
            }
        }
        println!();
    }
}

// ─── eval ────────────────────────────────────────────────────────────────────

async fn eval(client: &Client, play_id: u32) -> Result<()> {
    // First fetch play title for the header line
    let title: String = {
        let path = format!("/api/plays/{play_id}");
        match client.get::<PlayDetailResponse>(&path).await {
            Ok(r) => truncate(&r.play.title, 50),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("404") {
                    eprintln!("play {play_id} not found");
                    std::process::exit(1);
                }
                return Err(e);
            }
        }
    };

    println!("Evaluating play {} ({})…", play_id, title);

    let path = format!("/api/plays/{play_id}/evaluate");
    let body = json!({});

    match client.post::<EvalResponse, _>(&path, &body).await {
        Ok(resp) => {
            let ev = &resp.evaluation;
            let all_icon = if ev.all_met {
                f::ok("true")
            } else {
                "false".to_string()
            };
            println!("  all_met:      {}", all_icon);
            println!("  met:          {} / {}", ev.met_count, ev.total_count);
            println!("  expired:      {}", ev.expired);
            println!("  invalidated:  {}", ev.invalidation_triggered);
        }
        Err(e) => {
            let msg = e.to_string();
            // 503 = market data not ready
            if msg.contains("503") {
                println!(
                    "  {}: market data not ready (try during market hours)",
                    f::dim("unavailable")
                );
                return Ok(());
            }
            // 404 = play not found
            if msg.contains("404") {
                eprintln!("play {play_id} not found");
                std::process::exit(1);
            }
            return Err(e);
        }
    }

    Ok(())
}
