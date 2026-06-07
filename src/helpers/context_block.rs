//! Context block assembler for `carrac ask`.
//!
//! Fetches `/api/ai/context-blocks` metadata (optional) and assembles the
//! market context that ships with an AI chat request.  Designed to be
//! transport-agnostic: callers receive a `ContextPayload` struct and decide
//! how to display the `display_lines` or post the `ai_body`.

use serde_json::{Value, json};

use crate::commands::chart::fmt_price;
use crate::types::batch::BatchResponse;

// ─── Public surface ───────────────────────────────────────────────────────────

/// Assembled context for a single-asset AI question.
/// `display_lines` are ANSI-coloured strings for terminal output.
/// `ai_body` is the JSON value ready to POST to `/api/ai/chat`.
pub struct ContextPayload {
    #[allow(dead_code)] // assembled context metadata, not yet surfaced
    pub asset: String,
    #[allow(dead_code)] // assembled context metadata, not yet surfaced
    pub tf: Option<String>,
    pub display_lines: Vec<String>,
    pub ai_body: Value,
}

/// Build a `ContextPayload` from a pre-fetched `BatchResponse`.
///
/// This is the primary entry point.  `carrac ask` calls `fetch_batch` first, then
/// passes the result here.  The TUI can do the same inside its AI popup handler.
pub fn from_batch(
    asset: &str,
    tf: Option<&str>,
    page: &str,
    question: &str,
    batch: &BatchResponse,
) -> ContextPayload {
    let asset_upper = asset.to_uppercase();
    let asset_data = batch.assets.get(&asset_upper);

    let mut display_lines: Vec<String> = Vec::new();
    let mut context_parts: Vec<String> = Vec::new();

    // ── Price / market ────────────────────────────────────────────────────────
    if let Some(data) = asset_data {
        if let Some(market) = &data.market {
            let price = market.price.unwrap_or(0.0);
            let chg = market.change_24h.unwrap_or(0.0);
            let tf_label = tf.unwrap_or("—");
            let price_str = if price == 0.0 {
                "—".to_string()
            } else {
                fmt_price(price)
            };
            let chg_str = format!("{:+.2}%", chg);

            display_lines.push(format!(
                "  context  {} {}  ·  price {}  ·  chg 24h {}",
                asset_upper, tf_label, price_str, chg_str
            ));

            context_parts.push(format!(
                "{asset_upper} {tf_label} — price: {price_str}, 24h change: {chg_str}"
            ));
        }

        // ── Scanner ───────────────────────────────────────────────────────────
        if let Some(scanner) = &data.scanner {
            let long_score = scanner.long.as_ref().map(|s| s.score).unwrap_or(0);
            let short_score = scanner.short.as_ref().map(|s| s.score).unwrap_or(0);
            let long_verdict = scanner
                .long
                .as_ref()
                .map(|s| s.verdict.as_str())
                .unwrap_or("—");
            let short_verdict = scanner
                .short
                .as_ref()
                .map(|s| s.verdict.as_str())
                .unwrap_or("—");

            display_lines.push(format!(
                "           scanner  long {} ({})  ·  short {} ({})",
                long_verdict, long_score, short_verdict, short_score
            ));

            context_parts.push(format!(
                "Scanner: long {long_verdict} score {long_score}, short {short_verdict} score {short_score}"
            ));
        }
    } else {
        display_lines.push(format!("  context  {asset_upper}  ·  (no market data)"));
        context_parts.push(format!("{asset_upper} — no market data available"));
    }

    // ── Assemble AI body ──────────────────────────────────────────────────────
    let context_text = context_parts.join("\n");
    let message =
        format!("Context:\n{context_text}\n\nUser question: {question}\n\nReply concisely.",);

    let ai_body = json!({
        "message": message,
        "asset": asset_upper,
        "timeframe": tf.unwrap_or(""),
        "page_context": page,
    });

    ContextPayload {
        asset: asset_upper,
        tf: tf.map(str::to_string),
        display_lines,
        ai_body,
    }
}

/// Build a minimal `ContextPayload` with no batch data (--no-context mode).
pub fn bare(asset: Option<&str>, tf: Option<&str>, page: &str, question: &str) -> ContextPayload {
    let asset_str = asset.unwrap_or("(no asset)").to_uppercase();

    let ai_body = json!({
        "message": question,
        "asset": asset_str,
        "timeframe": tf.unwrap_or(""),
        "page_context": page,
    });

    ContextPayload {
        asset: asset_str,
        tf: tf.map(str::to_string),
        display_lines: vec![],
        ai_body,
    }
}
