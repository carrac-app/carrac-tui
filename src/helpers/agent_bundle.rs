//! Agent context bundle writer.
//!
//! When the user presses `I`, the TUI serializes whatever data is currently
//! loaded in the focused view into a workspace directory so the spawned agent
//! can immediately read context without hitting the network.
//!
//! Workspace dir: `<cache_dir>/agent/` (same `directories` base as tui-state).
//! Files written depend on the active view (orientation = CLAUDE.md + AGENTS.md,
//! identical content, auto-loaded by most agents):
//!   - Chart     → chart.png + chart-context.json + orientation
//!   - Positions → positions.json + orientation
//!   - Plays     → plays.json     + orientation
//!   - Alerts    → alerts.json    + orientation
//!   - (other)   → orientation only (generic)
//!
//! The caller is responsible for actually rendering chart.png (it must be done
//! async before calling the bundle writer). This module handles the JSON
//! serialization and orientation-file generation.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::app::state::{App, View};

/// Return the workspace directory used for agent bundles:
/// `~/.cache/carrac/agent/`
pub fn workspace_dir() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("cc", "carrac", "carrac")?;
    Some(dirs.cache_dir().join("agent"))
}

/// Ensure the workspace dir exists and is empty of previous bundle files.
///
/// We clear only the specific files we might write (CONTEXT.md, *.json, *.png)
/// so a user who has put something else there won't lose it.
pub fn prepare_workspace(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    for name in &[
        "CONTEXT.md",
        "CLAUDE.md",
        "AGENTS.md",
        "chart-context.json",
        "drawings.json",
        "positions.json",
        "plays.json",
        "alerts.json",
        "brief.json",
    ] {
        let p = dir.join(name);
        if p.exists() {
            let _ = std::fs::remove_file(&p);
        }
    }
    // Also remove any existing chart.png so stale images don't confuse the agent.
    let chart_png = dir.join("chart.png");
    if chart_png.exists() {
        let _ = std::fs::remove_file(&chart_png);
    }
    Ok(())
}

/// Write the orientation text under the filenames coding agents auto-load:
/// `CLAUDE.md` (Claude Code reads it as project context automatically) and
/// `AGENTS.md` (gemini / aider / opencode convention). This is why the agent
/// knows the asset / timeframe / which files exist without being asked.
fn write_orientation(dir: &Path, content: &str) -> Result<()> {
    // Prepend a session preamble so the agent (which auto-loads CLAUDE.md) opens
    // with a short welcome covering: launched from the Carrac TUI, what's being
    // viewed, the chart.png, MCP + web access, and Ctrl-D to return.
    //
    // The chart.png clause is included ONLY when a PNG was actually rendered into
    // the workspace (the caller writes it before this runs). Otherwise the agent
    // is told to read a chart image that isn't there and reports it "not found"
    // — which is exactly what happened on non-chart views (e.g. Details).
    let png_clause = if dir.join("chart.png").exists() {
        "that `chart.png` is the rendered chart and you can read it; "
    } else {
        ""
    };
    let preamble = format!(
        "# Carrac TUI — Agent Session\n\n\
         You were launched from the Carrac trading terminal (a TUI). On your FIRST reply, \
         open with a brief 3-5 line welcome stating: you're invoked from the Carrac TUI; \
         what the user is currently viewing (see below); {png_clause}that you can also use your \
         MCP tools and the web to dig deeper; and that pressing Ctrl-D (or `/exit`) returns them \
         to the TUI. Then help with their question.\n\n\
         ---\n\n"
    );
    let full = format!("{preamble}{content}");
    std::fs::write(dir.join("CLAUDE.md"), &full)?;
    std::fs::write(dir.join("AGENTS.md"), &full)?;
    Ok(())
}

// ─── view-specific bundle writers ─────────────────────────────────────────────

/// Write context bundle for the Chart view.
///
/// `png_src`: path to the freshly-rendered PNG (will be copied to
/// `<workspace>/chart.png` if provided and exists).
pub fn write_chart_bundle(dir: &Path, app: &App, png_src: Option<&Path>) -> Result<()> {
    // Copy PNG into the workspace if a separate source was rendered.
    // Guard against a SELF-COPY: launch_agent renders straight into the
    // workspace (src == <dir>/chart.png), and std::fs::copy(x, x) truncates the
    // file to 0 bytes. When src is already the destination, leave it in place.
    if let Some(src) = png_src {
        let dest = dir.join("chart.png");
        if src.exists() && src != dest.as_path() {
            std::fs::copy(src, &dest)?;
        }
    }

    // Serialize chart data as JSON (OHLCV + indicators + drawings).
    if let Some(ref data) = app.chart_data {
        let json = serde_json::to_string_pretty(data)?;
        std::fs::write(dir.join("chart-context.json"), json)?;
    }

    let has_png = dir.join("chart.png").exists();
    let has_json = dir.join("chart-context.json").exists();
    let mut ctx = build_chart_context(app, has_png, has_json);
    if dir.join("drawings.json").exists() {
        ctx.push_str(
            "\n- `drawings.json` — the exact server-side drawings on this chart \
             (horizontal lines, trendlines, zones, scenarios) as machine-readable \
             levels. Prefer these precise values over reading them off the image.\n",
        );
    }
    write_orientation(dir, &ctx)?;
    Ok(())
}

/// Write context bundle for the Positions view.
pub fn write_positions_bundle(dir: &Path, app: &App) -> Result<()> {
    if let Some(ref pos) = app.positions {
        let json = serde_json::to_string_pretty(pos)?;
        std::fs::write(dir.join("positions.json"), json)?;
    }
    let ctx = build_positions_context(app);
    write_orientation(dir, &ctx)?;
    Ok(())
}

/// Write context bundle for the Plays view.
pub fn write_plays_bundle(dir: &Path, app: &App) -> Result<()> {
    if let Some(ref plays) = app.plays {
        let json = serde_json::to_string_pretty(plays)?;
        std::fs::write(dir.join("plays.json"), json)?;
    }
    let ctx = build_plays_context(app);
    write_orientation(dir, &ctx)?;
    Ok(())
}

/// Write context bundle for the Alerts view.
pub fn write_alerts_bundle(dir: &Path, app: &App) -> Result<()> {
    if let Some(ref alerts) = app.alerts {
        let json = serde_json::to_string_pretty(alerts)?;
        std::fs::write(dir.join("alerts.json"), json)?;
    }
    let ctx = build_alerts_context(app);
    write_orientation(dir, &ctx)?;
    Ok(())
}

/// Write the context bundle for the `carrac brief` command.
#[allow(dead_code)]
pub fn write_brief_bundle(dir: &Path, app: &App) -> Result<()> {
    // Serialize sentiment + scanner as a combined JSON.
    let mut obj = serde_json::json!({});
    if let Some(ref s) = app.sentiment {
        obj["sentiment"] = serde_json::to_value(s).unwrap_or_default();
    }
    if let Some(ref s) = app.scanner {
        obj["scanner"] = serde_json::to_value(s).unwrap_or_default();
    }
    let json = serde_json::to_string_pretty(&obj)?;
    std::fs::write(dir.join("brief.json"), json)?;
    let ctx = build_brief_context(app);
    write_orientation(dir, &ctx)?;
    Ok(())
}

/// Write a generic CONTEXT.md for any view not specifically handled.
pub fn write_generic_bundle(dir: &Path, app: &App) -> Result<()> {
    let ctx = build_generic_context(app);
    write_orientation(dir, &ctx)?;
    Ok(())
}

/// Top-level dispatch: picks the right writer based on `app.view`.
///
/// For Chart view the caller must supply `png_src` (may be None if rendering
/// failed); for all other views `png_src` is ignored.
pub fn write_bundle(dir: &Path, app: &App, png_src: Option<&Path>) -> Result<()> {
    match app.view {
        View::Chart => write_chart_bundle(dir, app, png_src),
        View::Positions => write_positions_bundle(dir, app),
        View::Plays => write_plays_bundle(dir, app),
        View::Alerts => write_alerts_bundle(dir, app),
        _ => write_generic_bundle(dir, app),
    }
}

// ─── CONTEXT.md builders ───────────────────────────────────────────────────────

fn build_chart_context(app: &App, has_png: bool, has_json: bool) -> String {
    let mut lines = vec![
        "# Carrac TUI — Agent Context".to_string(),
        String::new(),
        format!(
            "You're helping analyze **{}** ({} timeframe) in the Carrac trading terminal.",
            app.selected_asset, app.chart.tf
        ),
        String::new(),
    ];
    if has_png {
        lines.push(
            "- **chart.png** — full chart rendering with overlays and indicators.".to_string(),
        );
    }
    if has_json {
        lines.push(
            "- **chart-context.json** — raw OHLCV candles + CPR / EMA / drawing data.".to_string(),
        );
    }
    lines.push(String::new());
    lines.push(
        "The user has the chart open and may ask follow-up questions about the price action,"
            .to_string(),
    );
    lines.push(
        "indicator readings, or trade setup. Reference the chart data / PNG in your answers."
            .to_string(),
    );
    lines.join("\n")
}

fn build_positions_context(app: &App) -> String {
    let count = app.positions.as_ref().map_or(0, |p| p.positions.len());
    format!(
        "# Carrac TUI — Agent Context\n\
         \n\
         You're helping review **open positions** in the Carrac trading terminal.\n\
         \n\
         - **positions.json** — {count} open position(s) with entry, stop, target, P&L and risk status.\n\
         \n\
         The user may ask about risk management, position sizing, or how to handle specific trades.\n\
         Reference positions.json for the data.\n"
    )
}

fn build_plays_context(app: &App) -> String {
    let count = app.plays.as_ref().map_or(0, |p| p.plays.len());
    format!(
        "# Carrac TUI — Agent Context\n\
         \n\
         You're helping review **active plays** (trade theses / setups) in the Carrac trading terminal.\n\
         \n\
         - **plays.json** — {count} active play(s) with title, direction, conditions and thesis.\n\
         \n\
         The user may ask about evaluating setups, checking conditions, or adjusting plays.\n\
         Reference plays.json for the data.\n"
    )
}

fn build_alerts_context(app: &App) -> String {
    let count = app.alerts.as_ref().map_or(0, |a| a.alerts.len());
    format!(
        "# Carrac TUI — Agent Context\n\
         \n\
         You're helping review **triggered alerts** in the Carrac trading terminal.\n\
         \n\
         - **alerts.json** — {count} triggered alert(s) with symbol, condition and trigger price.\n\
         \n\
         The user may ask about what to do following these alerts.\n\
         Reference alerts.json for the data.\n"
    )
}

fn build_brief_context(app: &App) -> String {
    "# Carrac TUI — Agent Context\n\
     \n\
     You're helping review the **market brief** in the Carrac trading terminal.\n\
     \n\
     - **brief.json** — market sentiment + scanner overview (Consensio, EMA, CPR, BB states).\n\
     \n\
     The user may ask about overall market conditions, regime, or which assets look interesting.\n\
     Reference brief.json for the data.\n"
    .to_string()
    // suppress unused-variable warning for app — kept for API consistency
    + &format!("\n<!-- asset: {} -->", app.selected_asset)
}

fn build_generic_context(app: &App) -> String {
    format!(
        "# Carrac TUI — Agent Context\n\
         \n\
         You're helping a Carrac user who is viewing the **{}** tab.\n\
         Current asset: {}  |  Timeframe: {}\n\
         \n\
         The user will ask follow-up questions about what they see in the terminal.\n",
        app.view.label(),
        app.selected_asset,
        app.chart.tf
    )
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::App;
    use crate::types::plays::PlaysResponse;
    use crate::types::positions::{Direction, PositionsResponse};

    /// Create a unique temp directory. Caller must clean up or ignore.
    fn make_tmp_dir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "carrac-agent-bundle-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).expect("create temp dir");
        base
    }

    // Helper that positions serde_json deriving works on PositionsResponse.
    fn sample_positions_response() -> PositionsResponse {
        PositionsResponse {
            positions: vec![crate::types::positions::Position {
                id: 1,
                asset: "BTC".into(),
                direction: Direction::Long,
                entry_price: Some(60000.0),
                current_price: Some(62000.0),
                stop_price: Some(58000.0),
                target_price: Some(70000.0),
                unrealized_pnl_pct: Some(3.3),
                current_rr: Some(1.0),
                risk_status: None,
                scanner_verdict: None,
                linked_play: None,
                time_in_trade: None,
                setup_type: None,
                opened_at: None,
                planned_rr: Some(2.0),
                unrealized_pnl_usd: None,
                timeframe: None,
            }],
            portfolio_heat: None,
        }
    }

    #[test]
    fn workspace_dir_returns_some() {
        assert!(workspace_dir().is_some());
    }

    #[test]
    fn prepare_workspace_creates_dir() {
        let base = make_tmp_dir();
        let ws = base.join("agent");
        prepare_workspace(&ws).unwrap();
        assert!(ws.exists());
    }

    #[test]
    fn prepare_workspace_removes_stale_files() {
        let ws = make_tmp_dir();
        std::fs::write(ws.join("CLAUDE.md"), "old").unwrap();
        std::fs::write(ws.join("positions.json"), "old").unwrap();
        prepare_workspace(&ws).unwrap();
        assert!(!ws.join("CLAUDE.md").exists());
        assert!(!ws.join("positions.json").exists());
    }

    #[test]
    fn write_positions_bundle_creates_json_and_context() {
        let ws = make_tmp_dir();
        let mut app = App::new();
        app.view = View::Positions;
        app.apply(crate::app::state::Update::Positions(
            sample_positions_response(),
        ));

        write_positions_bundle(&ws, &app).unwrap();

        let json_path = ws.join("positions.json");
        let ctx_path = ws.join("CLAUDE.md");
        assert!(json_path.exists(), "positions.json should be written");
        assert!(ctx_path.exists(), "CONTEXT.md should be written");

        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();
        assert_eq!(
            json["positions"][0]["asset"].as_str(),
            Some("BTC"),
            "positions.json should contain BTC position"
        );

        let ctx = std::fs::read_to_string(&ctx_path).unwrap();
        assert!(
            ctx.contains("positions"),
            "CONTEXT.md should mention positions"
        );
        assert!(
            ctx.contains("positions.json"),
            "CONTEXT.md should reference the json file"
        );
    }

    #[test]
    fn write_plays_bundle_creates_json_and_context() {
        let ws = make_tmp_dir();
        let mut app = App::new();
        app.view = View::Plays;
        app.apply(crate::app::state::Update::Plays(PlaysResponse::default()));

        write_plays_bundle(&ws, &app).unwrap();

        assert!(ws.join("plays.json").exists());
        let ctx = std::fs::read_to_string(ws.join("CLAUDE.md")).unwrap();
        assert!(ctx.contains("plays"));
    }

    #[test]
    fn write_generic_bundle_creates_context_with_view_label() {
        let ws = make_tmp_dir();
        let mut app = App::new();
        app.view = View::News;
        app.selected_asset = "ETH".to_string();

        write_generic_bundle(&ws, &app).unwrap();

        let ctx = std::fs::read_to_string(ws.join("CLAUDE.md")).unwrap();
        assert!(ctx.contains("news"), "should mention view label");
        assert!(ctx.contains("ETH"), "should mention selected asset");
    }

    #[test]
    fn write_bundle_dispatches_to_positions_writer() {
        let ws = make_tmp_dir();
        let mut app = App::new();
        app.view = View::Positions;
        app.apply(crate::app::state::Update::Positions(
            sample_positions_response(),
        ));

        write_bundle(&ws, &app, None).unwrap();

        assert!(ws.join("positions.json").exists());
        assert!(ws.join("CLAUDE.md").exists());
    }

    #[test]
    fn write_chart_bundle_without_png_writes_context_only() {
        let ws = make_tmp_dir();
        let mut app = App::new();
        app.view = View::Chart;
        app.selected_asset = "BTC".to_string();
        // No chart_data loaded — bundle writes CONTEXT.md only.
        write_chart_bundle(&ws, &app, None).unwrap();

        assert!(ws.join("CLAUDE.md").exists());
        assert!(
            !ws.join("chart-context.json").exists(),
            "no json without chart_data"
        );
    }

    #[test]
    fn write_chart_bundle_does_not_truncate_in_place_png() {
        let ws = make_tmp_dir();
        let mut app = App::new();
        app.view = View::Chart;
        app.selected_asset = "BTC".to_string();
        // Real launch_agent flow: PNG is rendered straight into the workspace,
        // then that same path is passed as png_src. Must NOT self-copy-truncate.
        let png = ws.join("chart.png");
        std::fs::write(&png, b"\x89PNG fake bytes that must survive").unwrap();
        write_chart_bundle(&ws, &app, Some(png.as_path())).unwrap();
        let bytes = std::fs::read(&png).unwrap();
        assert!(
            !bytes.is_empty(),
            "in-place chart.png must NOT be truncated to 0 bytes"
        );
    }

    #[test]
    fn orientation_omits_chart_png_clause_when_absent() {
        // The bug: agent told to read chart.png on a view that rendered none →
        // "not found". With no chart.png present, the clause must be omitted.
        let ws = make_tmp_dir();
        write_orientation(&ws, "ctx").unwrap();
        let md = std::fs::read_to_string(ws.join("CLAUDE.md")).unwrap();
        assert!(
            !md.contains("is the rendered chart"),
            "must NOT instruct the agent to read chart.png when none was rendered"
        );
    }

    #[test]
    fn orientation_includes_chart_png_clause_when_present() {
        let ws = make_tmp_dir();
        std::fs::write(ws.join("chart.png"), b"\x89PNG").unwrap();
        write_orientation(&ws, "ctx").unwrap();
        let md = std::fs::read_to_string(ws.join("CLAUDE.md")).unwrap();
        assert!(
            md.contains("is the rendered chart"),
            "must tell the agent about chart.png when it exists"
        );
    }
}
