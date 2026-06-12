use serde::{Deserialize, Serialize};
use std::fmt;

// ─── Direction ────────────────────────────────────────────────────────────────

/// Position direction as received from the server ("long" | "short").
/// Not non-exhaustive: long/short is a binary contract; an unexpected value IS
/// a server schema break and we want a hard deserialization error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Long,
    Short,
}

impl Direction {
    /// Parse a server-supplied direction label ("long"/"short", any case).
    /// Returns `None` for unrecognised values so callers can fall back rather
    /// than each widget hand-rolling its own string match. This keeps the enum
    /// the single source of truth for what a valid direction string is.
    #[must_use]
    pub fn from_label(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "long" => Some(Direction::Long),
            "short" => Some(Direction::Short),
            _ => None,
        }
    }
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Direction::Long => write!(f, "long"),
            Direction::Short => write!(f, "short"),
        }
    }
}

// ─── RiskStatus ───────────────────────────────────────────────────────────────

/// Risk status string from the server.
/// `Unknown` absorbs any future server-side additions so deserialization never
/// crashes; it emits a one-shot stderr warning via `warn_unknown_risk`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskStatus {
    Healthy,
    AtRisk,
    StoppedOut,
    /// Catch-all for values the server sends that we don't recognise yet.
    #[serde(other)]
    Unknown,
}

impl fmt::Display for RiskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RiskStatus::Healthy => write!(f, "healthy"),
            RiskStatus::AtRisk => write!(f, "at_risk"),
            RiskStatus::StoppedOut => write!(f, "stopped_out"),
            RiskStatus::Unknown => write!(f, "unknown"),
        }
    }
}

// ─── Structs ──────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct PositionsResponse {
    #[serde(default)]
    pub positions: Vec<Position>,
    #[serde(default)]
    pub portfolio_heat: Option<PortfolioHeat>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct PortfolioHeat {
    #[serde(default)]
    pub total_exposure: u32,
    #[serde(default)]
    pub long_count: u32,
    #[serde(default)]
    pub short_count: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Position {
    pub id: u64,
    pub asset: String,
    pub direction: Direction,

    #[serde(default)]
    pub entry_price: Option<f64>,
    #[serde(default)]
    pub current_price: Option<f64>,
    #[serde(default)]
    pub stop_price: Option<f64>,
    #[serde(default)]
    pub target_price: Option<f64>,

    #[serde(default)]
    pub unrealized_pnl_pct: Option<f64>,
    #[serde(default)]
    pub current_rr: Option<f64>,

    #[serde(default)]
    pub risk_status: Option<RiskStatus>,
    #[serde(default)]
    pub scanner_verdict: Option<String>,

    #[serde(default)]
    pub linked_play: Option<LinkedPlay>,

    #[serde(default)]
    pub time_in_trade: Option<String>,
    #[serde(default)]
    pub setup_type: Option<String>,
    #[serde(default)]
    pub opened_at: Option<String>,
    #[serde(default)]
    pub planned_rr: Option<f64>,
    #[serde(default)]
    pub unrealized_pnl_usd: Option<f64>,
    #[serde(default)]
    pub timeframe: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LinkedPlay {
    pub id: u64,
    pub title: String,
    #[serde(default)]
    pub conditions_met: Option<String>,
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Direction ──

    #[test]
    fn direction_deserializes_long() {
        let d: Direction = serde_json::from_str("\"long\"").unwrap();
        assert_eq!(d, Direction::Long);
    }

    #[test]
    fn direction_deserializes_short() {
        let d: Direction = serde_json::from_str("\"short\"").unwrap();
        assert_eq!(d, Direction::Short);
    }

    #[test]
    fn direction_rejects_buy() {
        // "buy" is not a known direction — must be a hard error (no serde(other))
        let result: Result<Direction, _> = serde_json::from_str("\"buy\"");
        assert!(result.is_err(), "Direction::buy should fail to deserialize");
    }

    #[test]
    fn direction_display() {
        assert_eq!(Direction::Long.to_string(), "long");
        assert_eq!(Direction::Short.to_string(), "short");
    }

    #[test]
    fn direction_from_label() {
        assert_eq!(Direction::from_label("long"), Some(Direction::Long));
        assert_eq!(Direction::from_label("SHORT"), Some(Direction::Short));
        assert_eq!(Direction::from_label(" Long "), Some(Direction::Long));
        assert_eq!(Direction::from_label("—"), None);
        assert_eq!(Direction::from_label("buy"), None);
    }

    // ── RiskStatus ──

    #[test]
    fn risk_status_deserializes_at_risk() {
        let r: RiskStatus = serde_json::from_str("\"at_risk\"").unwrap();
        assert_eq!(r, RiskStatus::AtRisk);
    }

    #[test]
    fn risk_status_deserializes_stopped_out() {
        let r: RiskStatus = serde_json::from_str("\"stopped_out\"").unwrap();
        assert_eq!(r, RiskStatus::StoppedOut);
    }

    #[test]
    fn risk_status_deserializes_healthy() {
        let r: RiskStatus = serde_json::from_str("\"healthy\"").unwrap();
        assert_eq!(r, RiskStatus::Healthy);
    }

    #[test]
    fn risk_status_unknown_round_trips() {
        // A completely new server-side value must not crash — it maps to Unknown.
        let r: RiskStatus = serde_json::from_str("\"completely_new_status_value\"").unwrap();
        assert_eq!(r, RiskStatus::Unknown);
    }
}
