use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct SentimentResponse {
    #[serde(default)]
    pub sentiment: Sentiment,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Sentiment {
    #[serde(default)]
    pub fear_greed: Option<FearGreed>,
    #[serde(default)]
    pub regime: Option<Regime>,
    /// Alt Season Index (0-100 + "alt season"/"neutral"/"btc season").
    #[serde(default)]
    pub alt_season: Option<AltSeason>,
    /// MVRV Z-Score (+ zone label like "neutral"/"overheated").
    #[serde(default)]
    pub mvrv_zscore: Option<MvrvZ>,
    /// BTC perp funding rate.
    #[serde(default)]
    pub funding_rate: Option<FundingRate>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct AltSeason {
    #[serde(default)]
    pub index: Option<f64>,
    #[serde(default)]
    pub season: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct MvrvZ {
    #[serde(default)]
    pub zscore: Option<f64>,
    #[serde(default)]
    pub zone: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct FundingRate {
    #[serde(default)]
    pub rate_pct: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FearGreed {
    pub value: u32,
    pub classification: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Regime {
    pub regime: String,
    pub score: i32,
    #[serde(default)]
    pub components: Vec<RegimeComponent>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RegimeComponent {
    pub name: String,
    pub detail: String,
    #[serde(default)]
    pub score: i32,
}

impl Regime {
    pub fn breadth(&self) -> Option<&str> {
        self.components
            .iter()
            .find(|c| c.name == "Breadth")
            .map(|c| c.detail.as_str())
    }
}
