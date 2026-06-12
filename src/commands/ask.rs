use anyhow::Result;
use std::io::Write;

use crate::client::Client;
use crate::config::Config;
use crate::format as f;
use crate::helpers::batch_fetch::fetch_batch;
use crate::helpers::context_block::{self, ContextPayload};
use crate::helpers::sse::SseStreamParser;
use crate::helpers::watchlist;

// ─── clap arg struct ──────────────────────────────────────────────────────────

#[derive(clap::Args)]
pub struct AskArgs {
    /// Question to ask the AI copilot
    pub question: String,

    /// Asset symbol for market context (e.g. BTC, ETH). Defaults to first watchlist symbol.
    #[arg(long)]
    pub asset: Option<String>,

    /// Timeframe hint included in context (e.g. 4H, 1D)
    #[arg(long)]
    pub tf: Option<String>,

    /// Page hint for the AI: chart | scanner | journal | report [default: chart]
    #[arg(long, default_value = "chart")]
    pub page: String,

    /// Skip context prefetch; send question with no market data attached
    #[arg(long)]
    pub no_context: bool,
}

// ─── entry point ─────────────────────────────────────────────────────────────

pub async fn run(args: AskArgs) -> Result<()> {
    let cfg = Config::load()?;
    if crate::config::gate_cli_if_demo(&cfg, "ask") {
        return Ok(());
    }
    let client = Client::new(&cfg)?;

    if args.no_context {
        run_bare(&client, &args).await
    } else {
        run_with_context(&client, &args).await
    }
}

// ─── with-context path ────────────────────────────────────────────────────────

async fn run_with_context(client: &Client, args: &AskArgs) -> Result<()> {
    // Resolve asset: --asset flag > first watchlist entry > fallback "BTC"
    let asset = args.asset.clone().unwrap_or_else(|| {
        watchlist::load()
            .into_iter()
            .next()
            .unwrap_or_else(|| "BTC".to_string())
    });

    eprint!("{}", f::dim("fetching context…"));

    let payload = match fetch_batch(
        client,
        std::slice::from_ref(&asset),
        &["scanner", "market", "plays"],
    )
    .await
    {
        Ok(batch) => {
            eprint!("\r                 \r");
            context_block::from_batch(
                &asset,
                args.tf.as_deref(),
                &args.page,
                &args.question,
                &batch,
            )
        }
        Err(e) => {
            eprint!("\r                 \r");
            eprintln!(
                "{}",
                f::dim(&format!(
                    "context fetch failed: {e} — continuing without market data"
                ))
            );
            context_block::bare(Some(&asset), args.tf.as_deref(), &args.page, &args.question)
        }
    };

    run_ask(client, payload).await
}

// ─── bare (no-context) path ───────────────────────────────────────────────────

async fn run_bare(client: &Client, args: &AskArgs) -> Result<()> {
    let payload = context_block::bare(
        args.asset.as_deref(),
        args.tf.as_deref(),
        &args.page,
        &args.question,
    );
    run_ask(client, payload).await
}

// ─── shared ask handler ───────────────────────────────────────────────────────

async fn run_ask(client: &Client, payload: ContextPayload) -> Result<()> {
    // Print context summary header
    for line in &payload.display_lines {
        println!("{}", f::dim(line));
    }
    if !payload.display_lines.is_empty() {
        println!();
    }

    eprint!("{}", f::dim("▴ thinking…"));

    let mut resp = match client.post_stream("/api/ai/chat", &payload.ai_body).await {
        Ok(r) => {
            // Clear the "thinking…" spinner as the first chunk is about to arrive.
            eprint!("\r            \r");
            r
        }
        Err(e) => {
            eprint!("\r            \r");
            let msg = e.to_string();
            if msg.contains("authentication required") || msg.contains("302") {
                println!(
                    "{}",
                    f::alert_dim(
                        "[AI unavailable: endpoint returned 302 — API key may lack AI scope]"
                    )
                );
                println!();
                print_context_dump(&payload);
                return Err(anyhow::anyhow!(
                    "AI endpoint returned 302 — API key may lack AI scope"
                ));
            } else if msg.contains("503") || msg.contains("AI service unavailable") {
                println!(
                    "{}",
                    f::alert_dim("[AI service temporarily unavailable, try again shortly.]")
                );
                println!();
                print_context_dump(&payload);
                return Err(anyhow::anyhow!(
                    "AI service temporarily unavailable, try again shortly."
                ));
            }
            println!("{}", f::dim(&format!("AI request failed: {msg}")));
            return Err(anyhow::anyhow!("AI request failed: {msg}"));
        }
    };

    let mut parser = SseStreamParser::new();
    let mut got_any_output = false;
    let mut final_sse_err: Option<String> = None;

    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                let chunk_str = match std::str::from_utf8(&chunk) {
                    Ok(s) => s,
                    Err(e) => {
                        println!();
                        return Err(anyhow::anyhow!("invalid UTF-8 in stream: {e}"));
                    }
                };
                let (tokens, sse_err) = parser.feed(chunk_str);
                for token in &tokens {
                    print!("{token}");
                    std::io::stdout().flush()?;
                    got_any_output = true;
                }
                if let Some(err) = sse_err {
                    final_sse_err = Some(err);
                }
            }
            Ok(None) => break, // stream ended cleanly
            Err(e) => {
                println!();
                return Err(anyhow::anyhow!("stream read error: {e:#}"));
            }
        }
    }

    if got_any_output {
        println!(); // final newline after streamed tokens
        if let Some(err) = final_sse_err {
            println!();
            println!(
                "{}",
                f::dim(&format!("(AI tool call failed mid-response: {err})"))
            );
        }
    } else if let Some(ref err) = final_sse_err {
        println!("{}", f::alert_dim(&format!("[AI unavailable: {err}]")));
    } else {
        println!("{}", f::dim("(AI returned an empty response)"));
    }

    Ok(())
}

// ─── context dump fallback ────────────────────────────────────────────────────

fn print_context_dump(payload: &ContextPayload) {
    println!("{}", f::bold("Context that would have been sent:"));
    let body = &payload.ai_body;
    if let Some(msg) = body.get("message").and_then(|m| m.as_str()) {
        for line in msg.lines() {
            println!("  {line}");
        }
    }
}
