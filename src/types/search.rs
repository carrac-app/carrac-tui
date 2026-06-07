use serde::{Deserialize, Deserializer};

/// Deserialize a possibly-`null`/missing string field as `""`.
///
/// One hit with `"name": null` (Yahoo symbols occasionally lack names) must
/// not fail the whole `Vec<SymbolHit>` deserialize — that silently emptied
/// the search results with no error shown.
fn null_to_empty<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

/// One hit returned by `GET /api/symbol-lookup?q=<query>&limit=15`.
///
/// The server returns a JSON array of these objects, ranked and deduplicated
/// across the curated list, Binance spot pairs, and Yahoo Finance symbols.
#[derive(Debug, Clone, Deserialize)]
pub struct SymbolHit {
    pub symbol: String,
    #[serde(default, deserialize_with = "null_to_empty")]
    pub name: String,
    #[serde(rename = "type", default, deserialize_with = "null_to_empty")]
    #[allow(dead_code)] // wire-contract field, not yet surfaced
    pub kind: String,
    #[serde(default)]
    #[allow(dead_code)] // wire-contract field, not yet surfaced
    pub tier: String,
    #[serde(default)]
    #[allow(dead_code)] // wire-contract field, not yet surfaced
    pub exchange: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_symbol_hit_from_json() {
        let json = r#"{"symbol":"BTCUSDT","name":"Bitcoin / Tether","type":"crypto","tier":"premium","exchange":"BINANCE"}"#;
        let hit: SymbolHit = serde_json::from_str(json).unwrap();
        assert_eq!(hit.symbol, "BTCUSDT");
        assert_eq!(hit.name, "Bitcoin / Tether");
        assert_eq!(hit.kind, "crypto");
        assert_eq!(hit.tier, "premium");
        assert_eq!(hit.exchange, "BINANCE");
    }

    #[test]
    fn deserialize_symbol_hit_array_from_json() {
        let json = r#"[
            {"symbol":"BTC","name":"Bitcoin","type":"crypto","tier":"free","exchange":""},
            {"symbol":"ETH","name":"Ethereum","type":"crypto","tier":"free","exchange":""},
            {"symbol":"AAPL","name":"Apple Inc.","type":"stock","tier":"premium","exchange":"NASDAQ"}
        ]"#;
        let hits: Vec<SymbolHit> = serde_json::from_str(json).unwrap();
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].symbol, "BTC");
        assert_eq!(hits[1].symbol, "ETH");
        assert_eq!(hits[2].kind, "stock");
        assert_eq!(hits[2].exchange, "NASDAQ");
    }

    #[test]
    fn deserialize_symbol_hit_missing_optional_fields() {
        // tier and exchange have serde(default) so they are optional in the payload.
        let json = r#"{"symbol":"GOLD","name":"Gold Spot","type":"commodity"}"#;
        let hit: SymbolHit = serde_json::from_str(json).unwrap();
        assert_eq!(hit.symbol, "GOLD");
        assert_eq!(hit.tier, "");
        assert_eq!(hit.exchange, "");
    }

    #[test]
    fn deserialize_symbol_hit_null_name_does_not_fail_array() {
        // A single null name/type must not sink the whole result set.
        let json = r#"[
            {"symbol":"XYZUSD","name":null,"type":null},
            {"symbol":"BTC","name":"Bitcoin","type":"crypto"}
        ]"#;
        let hits: Vec<SymbolHit> = serde_json::from_str(json).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].name, "");
        assert_eq!(hits[0].kind, "");
        assert_eq!(hits[1].name, "Bitcoin");
    }
}
