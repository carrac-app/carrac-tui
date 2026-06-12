use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct ScannerResponse {
    #[serde(default)]
    pub results: Vec<ScannerResult>,
    #[serde(default)]
    pub computed_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ScannerResult {
    pub symbol: String,
    #[serde(default)]
    pub price: Option<f64>,
    #[serde(default)]
    pub best_verdict: Option<String>,
    #[serde(default)]
    pub long: Option<ScannerSide>,
    #[serde(default)]
    pub short: Option<ScannerSide>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ScannerSide {
    #[serde(default)]
    pub verdict: Option<String>,
    #[serde(default)]
    pub total: Option<f64>,
}
