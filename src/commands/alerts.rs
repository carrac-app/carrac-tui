use anyhow::Result;
use chrono::Utc;

use crate::client::Client;
use crate::config::Config;
use crate::format as f;
use crate::helpers::wrap::truncate;
use crate::types::alerts::{
    AlertActionResponse, AlertFull, AlertListResponse, TriggeredAlertsResponse,
};

// ─── clap arg structs ─────────────────────────────────────────────────────────

#[derive(clap::Args)]
pub struct AlertsListArgs {
    /// Show only active (non-triggered) alerts
    #[arg(long)]
    pub active: bool,

    /// Show only triggered alerts (fetches /api/alerts/triggered)
    #[arg(long)]
    pub triggered: bool,

    /// Filter by symbol (e.g. BTC, ETH)
    #[arg(long)]
    pub asset: Option<String>,

    /// Max rows to display
    #[arg(long, default_value_t = 30)]
    pub limit: usize,
}

#[derive(clap::Subcommand)]
pub enum AlertsCmd {
    /// List alerts (active, triggered, or all)
    List(AlertsListArgs),

    /// Toggle an alert's active state (mark as inactive/active)
    Ack {
        /// Alert ID
        alert_id: String,
    },

    /// Delete an alert permanently
    Clear {
        /// Alert ID
        alert_id: String,
    },
}

// ─── entry point ─────────────────────────────────────────────────────────────

pub async fn run(cmd: AlertsCmd) -> Result<()> {
    let cfg = Config::load()?;
    if crate::config::gate_cli_if_demo(&cfg, "alerts") {
        return Ok(());
    }
    let client = Client::new(&cfg)?;
    match cmd {
        AlertsCmd::List(args) => list(&client, args).await,
        AlertsCmd::Ack { alert_id } => ack(&client, &alert_id).await,
        AlertsCmd::Clear { alert_id } => clear(&client, &alert_id).await,
    }
}

// ─── list ─────────────────────────────────────────────────────────────────────

async fn list(client: &Client, args: AlertsListArgs) -> Result<()> {
    let now = Utc::now().format("%Y-%m-%d %H:%M UTC");

    if args.triggered {
        return list_triggered(client, &args, &now.to_string()).await;
    }

    // Build query for GET /api/alerts
    let mut path = String::from("/api/alerts?");

    // Default: all alerts. --active filters to active only.
    if args.active {
        path.push_str("active=true");
    } else {
        path.push_str("active=false"); // returns all (active + triggered)
    }

    if let Some(ref sym) = args.asset {
        path.push_str(&format!("&asset={}", sym.to_uppercase()));
    }

    let resp = match client.get::<AlertListResponse>(&path).await {
        Ok(r) => r,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("redirect") || msg.contains("302") {
                eprintln!(
                    "{}",
                    f::dim("alerts unavailable (auth — try again or check API key scope)")
                );
                return Ok(());
            }
            return Err(e);
        }
    };

    let alerts = resp.alerts;
    let asset_label = args
        .asset
        .as_deref()
        .map(|a| format!(" · {}", a.to_uppercase()))
        .unwrap_or_default();
    let filter_label = if args.active { "active" } else { "all" };

    print_header(
        "alerts",
        &now.to_string(),
        &format!("{filter_label}{asset_label}"),
        alerts.len(),
    );

    if alerts.is_empty() {
        println!("{}", f::dim("(no alerts)"));
        return Ok(());
    }

    let limited: Vec<&AlertFull> = alerts.iter().take(args.limit).collect();
    render_alerts_table(&limited);

    if alerts.len() > args.limit {
        println!(
            "{}",
            f::dim(&format!(
                "({} total — showing first {})",
                alerts.len(),
                args.limit
            ))
        );
    }

    Ok(())
}

async fn list_triggered(client: &Client, args: &AlertsListArgs, now: &str) -> Result<()> {
    let resp = match client
        .get::<TriggeredAlertsResponse>("/api/alerts/triggered")
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("redirect") || msg.contains("302") {
                // Degrade gracefully: fall back to triggered_since=24h-ago query
                eprintln!(
                    "{}",
                    f::dim(
                        "⚠ /api/alerts/triggered unavailable — falling back to triggered_since filter"
                    )
                );
                let since = (Utc::now() - chrono::Duration::hours(24))
                    .format("%Y-%m-%dT%H:%M:%S")
                    .to_string();
                let path = format!("/api/alerts?triggered_since={since}");
                let fb = client.get::<AlertListResponse>(&path).await?;
                let asset_label = args
                    .asset
                    .as_deref()
                    .map(|a| format!(" · {}", a.to_uppercase()))
                    .unwrap_or_default();
                print_header(
                    "alerts triggered",
                    now,
                    &format!("24h{asset_label}"),
                    fb.alerts.len(),
                );
                let limited: Vec<&AlertFull> = fb.alerts.iter().take(args.limit).collect();
                render_alerts_table(&limited);
                return Ok(());
            }
            return Err(e);
        }
    };

    let mut alerts: Vec<&AlertFull> = if let Some(ref sym) = args.asset {
        let sym_up = sym.to_uppercase();
        resp.triggered
            .iter()
            .filter(|a| a.symbol.as_deref().map(str::to_uppercase).as_deref() == Some(&sym_up))
            .collect()
    } else {
        resp.triggered.iter().collect()
    };

    alerts.truncate(args.limit);

    let asset_label = args
        .asset
        .as_deref()
        .map(|a| format!(" · {}", a.to_uppercase()))
        .unwrap_or_default();
    print_header(
        "alerts triggered",
        now,
        asset_label.trim_start_matches(' '),
        alerts.len(),
    );

    if alerts.is_empty() {
        println!("{}", f::dim("(no triggered alerts)"));
        return Ok(());
    }

    render_alerts_table(&alerts);
    Ok(())
}

fn render_alerts_table(alerts: &[&AlertFull]) {
    if alerts.is_empty() {
        return;
    }

    let headers = [
        "ID",
        "Symbol",
        "Type",
        "Condition",
        "Price",
        "Active",
        "Note",
    ];
    let aligns = ['r', 'l', 'l', 'l', 'r', 'l', 'l'];

    let mut cells: Vec<Vec<String>> = Vec::with_capacity(alerts.len() + 1);
    cells.push(
        headers
            .iter()
            .map(std::string::ToString::to_string)
            .collect(),
    );

    for alert in alerts {
        let id = alert.id_str();
        let symbol = alert.symbol.as_deref().unwrap_or("—").to_string();
        let alert_type = alert.alert_type.as_deref().unwrap_or("price").to_string();

        // Condition column: for price alerts show condition+indicator, for indicator alerts show target_state
        let condition = if let Some(ts) = &alert.target_state {
            ts.clone()
        } else {
            alert.condition.as_deref().unwrap_or("—").to_string()
        };

        let price = match (alert.price, alert.triggered_price) {
            (_, Some(tp)) => format!("{tp:.2} ⚡"),
            (Some(p), None) => format!("{p:.2}"),
            _ => "—".to_string(),
        };

        let active = match alert.active {
            Some(true) => f::ok("yes"),
            Some(false) => f::dim("no"),
            None => "—".to_string(),
        };

        let note = truncate(alert.note.as_deref().unwrap_or(""), 40);

        cells.push(vec![id, symbol, alert_type, condition, price, active, note]);
    }

    print!("{}", f::render_table(&cells, &aligns, true));
}

// ─── ack ──────────────────────────────────────────────────────────────────────

async fn ack(client: &Client, alert_id: &str) -> Result<()> {
    let path = format!("/api/alerts/{alert_id}/toggle");
    let body = serde_json::json!({});

    match client.post::<AlertActionResponse, _>(&path, &body).await {
        Ok(resp) => {
            // Determine new state from the toggled alert
            let new_active = resp
                .alert
                .as_ref()
                .and_then(|a| a.active)
                .map_or("toggled", |a| if a { "active" } else { "inactive" });

            println!("{}  alert {}  →  {}", f::ok("⚡"), alert_id, new_active);
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("404") {
                eprintln!("alert {alert_id} not found");
                std::process::exit(1);
            }
            if msg.contains("redirect") || msg.contains("302") || msg.contains("401") {
                eprintln!(
                    "{}",
                    f::dim(&format!(
                        "ack failed: toggle alert {alert_id} from the web interface."
                    ))
                );
                std::process::exit(1);
            }
            return Err(e);
        }
    }

    Ok(())
}

// ─── clear ────────────────────────────────────────────────────────────────────

async fn clear(client: &Client, alert_id: &str) -> Result<()> {
    let path = format!("/api/alerts/{alert_id}");

    match client.delete::<AlertActionResponse>(&path).await {
        Ok(_) => {
            println!("{}", f::dim(&format!("alert {alert_id} deleted")));
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("404") {
                eprintln!("alert {alert_id} not found");
                std::process::exit(1);
            }
            if msg.contains("redirect") || msg.contains("302") || msg.contains("401") {
                eprintln!(
                    "{}",
                    f::dim("clear failed: delete this alert from the web interface.")
                );
                std::process::exit(1);
            }
            return Err(e);
        }
    }

    Ok(())
}

// ─── helpers ──────────────────────────────────────────────────────────────────

fn print_header(section: &str, ts: &str, filter: &str, count: usize) {
    println!(
        "{} {}  {}  {}   ({}: {})",
        f::bold("Carrac"),
        f::bold(section),
        f::dim("·"),
        f::dim(ts),
        filter,
        count,
    );
    println!();
}
