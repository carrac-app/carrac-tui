use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─── per-asset scanner block ─────────────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize, Default)]
pub struct ScannerSide {
    #[serde(default)]
    pub score: i32,
    #[serde(default)]
    pub verdict: String,
}

#[derive(Debug, Deserialize, Serialize, Default)]
pub struct BatchScannerBlock {
    #[serde(default)]
    pub long: Option<ScannerSide>,
    #[serde(default)]
    pub short: Option<ScannerSide>,
    #[serde(default)]
    pub best_verdict: Option<String>,
}

// ─── per-asset market block ───────────────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize, Default)]
pub struct BatchMarketBlock {
    #[serde(default)]
    pub symbol: String,
    /// May be null if the data feed for this symbol is unavailable.
    pub price: Option<f64>,
    pub change_24h: Option<f64>,
}

// ─── per-asset play (summary, not full Play) ─────────────────────────────────

#[derive(Debug, Deserialize, Serialize)]
pub struct BatchPlay {
    pub id: u32,
    pub title: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub direction: Option<String>,
}

// ─── per-asset bundle ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize, Default)]
pub struct AssetBatch {
    #[serde(default)]
    pub market: Option<BatchMarketBlock>,
    #[serde(default)]
    pub scanner: Option<BatchScannerBlock>,
    #[serde(default)]
    pub plays: Vec<BatchPlay>,
}

// ─── availability block ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
pub struct BatchAvailability {
    #[serde(default)]
    pub scanner: Option<String>,
    #[serde(default)]
    pub prices: Option<String>,
    #[serde(default)]
    pub plays: Option<String>,
    #[serde(default)]
    pub htf: Option<String>,
}

// ─── top-level response ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
pub struct BatchResponse {
    #[serde(default)]
    pub assets: HashMap<String, AssetBatch>,
    #[serde(default)]
    pub availability: BatchAvailability,
}
