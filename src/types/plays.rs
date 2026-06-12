use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct PlaysResponse {
    #[serde(default)]
    pub plays: Vec<Play>,
    #[serde(default)]
    pub count: u32,
}

/// Single play returned by GET /api/plays/{id}  (envelope: {"play": {...}})
#[derive(Debug, Deserialize)]
pub struct PlayDetailResponse {
    pub play: Play,
}

/// Evaluate response returned by POST /api/plays/{id}/evaluate
#[derive(Debug, Deserialize)]
pub struct EvalResponse {
    pub evaluation: EvalResult,
    #[allow(dead_code)] // reserved for future display
    pub play: Play,
}

#[derive(Debug, Deserialize)]
pub struct EvalResult {
    pub all_met: bool,
    pub met_count: u32,
    pub total_count: u32,
    #[serde(default)]
    pub expired: bool,
    #[serde(default)]
    pub invalidation_triggered: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Play {
    pub id: u64,
    pub title: String,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub primary_asset: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub timeframe: Option<String>,
    #[serde(default)]
    pub thesis: Option<String>,
    #[serde(default)]
    pub action_plan: Option<String>,
    #[serde(default)]
    pub expiry: Option<String>,
    #[serde(default)]
    #[allow(dead_code)] // reserved for future display
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub invalidation: Option<Invalidation>,
    #[serde(default)]
    pub conditions: Option<Conditions>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Invalidation {
    #[serde(default)]
    pub met: bool,
    /// Human text of the invalidation rule. `/api/plays/active` ships it as
    /// `note`; older shapes used `description` — accept both.
    #[serde(default, alias = "note")]
    pub description: Option<String>,
    #[serde(default)]
    pub evidence: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Conditions {
    #[serde(default)]
    pub logic: Option<String>,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Rule {
    #[serde(default)]
    pub met: bool,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub evidence: Option<String>,
}

impl Play {
    /// "n/m" string showing how many rules are currently met.
    #[must_use]
    pub fn progress(&self) -> String {
        if let Some(c) = &self.conditions {
            let total = c.rules.len();
            let met = c.rules.iter().filter(|r| r.met).count();
            format!("{met}/{total}")
        } else {
            "—".to_string()
        }
    }

    /// Met count as (met, total) tuple.
    #[must_use]
    pub fn condition_counts(&self) -> (usize, usize) {
        if let Some(c) = &self.conditions {
            let total = c.rules.len();
            let met = c.rules.iter().filter(|r| r.met).count();
            (met, total)
        } else {
            (0, 0)
        }
    }
}
