use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "carrac",
    version,
    about = "Carrac terminal client",
    long_about = "Terminal market cockpit for Carrac.cc — candlestick charts, positions, plays, alerts and AI workflows in your terminal."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Morning briefing: macro snapshot + position heat + scanner top + active plays + new alerts
    #[command(hide = true)]
    Brief,

    /// Open positions snapshot (or polled live with --watch)
    Positions {
        #[command(subcommand)]
        cmd: Option<commands::positions::PositionsCmd>,

        /// Filter to a single asset (e.g. BTC, ETH) [list mode]
        #[arg(long, short, global = false)]
        asset: Option<String>,

        /// Show per-position rows instead of asset-aggregated view [list mode]
        #[arg(long, short, global = false)]
        detail: bool,

        /// Poll every N seconds and redraw in place (Ctrl-C to exit) [list mode]
        #[arg(long, short, global = false)]
        watch: Option<u64>,
    },

    /// Inspect and evaluate trading plays
    Plays {
        #[command(subcommand)]
        cmd: commands::plays::PlaysCmd,
    },

    /// Interactive candlestick chart — works in any terminal
    Chart(commands::chart::ChartArgs),

    /// Multi-asset scanner: batch analysis with optional AI ranking
    #[command(hide = true)]
    Scan(commands::scan::ScanArgs),

    /// Alert triage: list, toggle, or delete price/indicator alerts
    Alerts {
        #[command(subcommand)]
        cmd: commands::alerts::AlertsCmd,
    },

    /// One-shot AI question with live market context injection
    #[command(hide = true)]
    Ask(commands::ask::AskArgs),

    /// Full-screen TUI: chart · positions · plays · alerts · report · news · details (number keys swap views; `a` asset selector; `?` keys)
    Tui,

    /// (internal) render a chart payload from stdin — used by the server
    #[command(hide = true)]
    Render(commands::render_stdin::RenderArgs),
}

mod app;
mod chart;
mod client;
mod commands;
mod config;
mod format;
mod helpers;
mod tui;
mod types;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // The `render` subcommand is a pure stdin→stdout pipeline with no config
    // and no network.  Dispatch it before init_color() so that force_enable_color()
    // in render_stdin::run() can win the OnceLock race (set() fails if already
    // initialised by init_color()).
    if let Commands::Render(args) = cli.command {
        std::process::exit(commands::render_stdin::run(args));
    }

    // Only initialise color for interactive commands — the Render path above
    // calls force_enable_color() itself, and gating here ensures no future
    // reordering can poison the OnceLock before that call.
    if !matches!(cli.command, Commands::Render(_)) {
        format::init_color();
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        match cli.command {
            Commands::Brief => commands::brief::run().await,
            Commands::Positions {
                cmd,
                asset,
                detail,
                watch,
            } => commands::positions::run(cmd, asset, detail, watch).await,
            Commands::Chart(args) => commands::chart::run(args).await,
            Commands::Plays { cmd } => commands::plays::run(cmd).await,
            Commands::Scan(args) => commands::scan::run(args).await,
            Commands::Alerts { cmd } => commands::alerts::run(cmd).await,
            Commands::Ask(args) => commands::ask::run(args).await,
            Commands::Tui => commands::tui::run().await,
            Commands::Render(_) => unreachable!("render is dispatched before the runtime starts"),
        }
    })
}
