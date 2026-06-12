use serde::{Deserialize, Serialize};

// ─── Existing types (used by brief.rs) ───────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct AlertsResponse {
    /// `/api/alerts/triggered` (the only endpoint parsed into this type)
    /// returns the array under `"triggered"` — accept both keys. Without the
    /// alias the unknown field was silently ignored and the alerts view
    /// always showed an empty list.
    #[serde(default, alias = "triggered")]
    pub alerts: Vec<Alert>,
}

/// Alert ids arrive as JSON numbers from the server; older payloads used
/// strings. Accept either, normalised to a display string.
fn de_id<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(v.map(|v| match v {
        serde_json::Value::String(s) => s,
        other => other.to_string(),
    }))
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Alert {
    #[serde(default, deserialize_with = "de_id")]
    pub id: Option<String>,
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub alert_type: Option<String>,
    #[serde(default)]
    pub condition: Option<String>,
    #[serde(default)]
    pub price: Option<f64>,
    #[serde(default)]
    pub indicator: Option<String>,
    #[serde(default)]
    pub triggered_at: Option<String>,
    #[serde(default)]
    pub triggered_price: Option<f64>,
    #[serde(default)]
    pub note: Option<String>,
}

// ─── New types for carrac alerts list / ack / clear ──────────────────────────

/// Response from GET /api/alerts
#[derive(Debug, Deserialize, Default)]
pub struct AlertListResponse {
    #[serde(default)]
    pub alerts: Vec<AlertFull>,
}

/// Response from GET /api/alerts/triggered
#[derive(Debug, Deserialize, Default)]
pub struct TriggeredAlertsResponse {
    #[serde(default)]
    pub triggered: Vec<AlertFull>,
}

/// Full alert shape returned by /api/alerts and /api/alerts/triggered.
/// The `id` field may arrive as int or string depending on the endpoint.
#[derive(Debug, Deserialize, Clone, Default)]
pub struct AlertFull {
    #[serde(default)]
    pub id: Option<serde_json::Value>,
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub alert_type: Option<String>,
    #[serde(default)]
    pub condition: Option<String>,
    #[serde(default)]
    pub price: Option<f64>,
    #[serde(default)]
    pub indicator: Option<String>,
    #[serde(default)]
    pub target_state: Option<String>,
    #[serde(default)]
    pub triggered_at: Option<String>,
    #[serde(default)]
    pub triggered_price: Option<f64>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default)]
    #[allow(dead_code)] // wire-contract field, not yet surfaced
    pub triggered: Option<bool>,
    #[serde(default)]
    pub frequency: Option<String>,
}

impl AlertFull {
    /// Return the alert id as a display string.
    #[must_use]
    pub fn id_str(&self) -> String {
        match &self.id {
            Some(serde_json::Value::Number(n)) => n.to_string(),
            Some(serde_json::Value::String(s)) => s.clone(),
            _ => "—".to_string(),
        }
    }
}

/// Response from POST /api/alerts/{id}/toggle or DELETE /api/alerts/{id}
#[derive(Debug, Deserialize, Default)]
pub struct AlertActionResponse {
    #[serde(default)]
    #[allow(dead_code)] // wire-contract field, not yet surfaced
    pub status: Option<String>,
    #[serde(default)]
    pub alert: Option<AlertFull>,
    #[serde(default)]
    #[allow(dead_code)] // wire-contract field, not yet surfaced
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alerts_response_parses_the_triggered_wire_shape() {
        // Pin the REAL /api/alerts/triggered payload: array under "triggered"
        // (not "alerts") and a numeric id. Both used to parse to an empty
        // list / a type error, so the alerts view never showed anything.
        let json = r#"{"triggered":[{"alert_type":"price","condition":"below",
            "id":2383142985,"indicator":null,"note":"zone touch","price":61200.0,
            "symbol":"BTC","target_state":null,
            "triggered_at":"2026-01-01T00:00:15","triggered_price":60530.96}]}"#;
        let resp: AlertsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.alerts.len(), 1);
        let a = &resp.alerts[0];
        assert_eq!(a.id.as_deref(), Some("2383142985"));
        assert_eq!(a.symbol.as_deref(), Some("BTC"));
        assert_eq!(a.triggered_price, Some(60530.96));
    }

    #[test]
    fn alerts_response_still_parses_the_alerts_key() {
        let json = r#"{"alerts":[{"id":"abc","symbol":"ETH"}]}"#;
        let resp: AlertsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.alerts.len(), 1);
        assert_eq!(resp.alerts[0].id.as_deref(), Some("abc"));
    }
}
