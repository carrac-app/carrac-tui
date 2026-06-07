//! Server-contract snapshot tests — pinned against the observed live API response shape.
//!
//! PURPOSE: make `cargo test` LOUDLY fail when the server schema changes beneath us.
//! Each test deserializes a hand-crafted fixture JSON that represents the minimum (or full)
//! shape the server is expected to return.  If the server drops or renames a required field,
//! serde will reject the fixture and the error message tells you exactly which field broke.
//!
//! When a test fails in CI:
//!   1. Check the actual live response for the affected endpoint.
//!   2. Update the fixture + the type definition in `src/types/*.rs` together.
//!   3. Commit both changes — the test is the contract, not just a regression guard.
//!
//! Endpoints covered:
//!   GET /api/positions          → PositionsResponse / Position
//!   GET /api/batch?assets=BTC   → BatchResponse / AssetBatch
//!   GET /api/plays/active       → PlaysResponse / Play / Conditions / Rule
//!   GET /api/alerts/triggered   → TriggeredAlertsResponse / AlertFull (int id + string id)
//!   GET /api/sentiment          → SentimentResponse / FearGreed / Regime

use carrac_tui::types::alerts::TriggeredAlertsResponse;
use carrac_tui::types::batch::BatchResponse;
use carrac_tui::types::plays::{Play, PlaysResponse};
use carrac_tui::types::positions::{Direction, PositionsResponse};
use carrac_tui::types::sentiment::SentimentResponse;

// ─── helpers ─────────────────────────────────────────────────────────────────

fn de<'de, T: serde::Deserialize<'de>>(s: &'de str) -> T {
    serde_json::from_str(s).expect("fixture failed to deserialize")
}

// ═══════════════════════════════════════════════════════════════════════════
// GET /api/positions
// ═══════════════════════════════════════════════════════════════════════════

/// Minimum required fields for a long position — no optionals populated.
#[test]
fn position_with_required_fields_deserializes() {
    let json = r#"{
        "positions": [
            {
                "id": 1,
                "asset": "BTC",
                "direction": "long"
            }
        ]
    }"#;
    let resp: PositionsResponse = de(json);
    assert_eq!(resp.positions.len(), 1);
    let pos = &resp.positions[0];
    assert_eq!(pos.id, 1);
    assert_eq!(pos.asset, "BTC");
    assert_eq!(pos.direction, Direction::Long);
    assert!(pos.entry_price.is_none());
    assert!(pos.linked_play.is_none());
}

/// Short position — direction field stays intact through serde.
#[test]
fn position_direction_short_deserializes() {
    let json = r#"{
        "positions": [
            {
                "id": 42,
                "asset": "ETH",
                "direction": "short"
            }
        ]
    }"#;
    let resp: PositionsResponse = de(json);
    assert_eq!(resp.positions[0].direction, Direction::Short);
}

/// Full position with all optional fields populated including linked_play.
#[test]
fn position_with_all_optional_fields_deserializes() {
    let json = r#"{
        "positions": [
            {
                "id": 7,
                "asset": "SOL",
                "direction": "long",
                "entry_price": 150.00,
                "current_price": 165.50,
                "stop_price": 140.00,
                "target_price": 200.00,
                "unrealized_pnl_pct": 10.33,
                "current_rr": 1.5,
                "risk_status": "ok",
                "scanner_verdict": "STRONG_LONG",
                "time_in_trade": "3d",
                "setup_type": "breakout",
                "opened_at": "2026-05-17T09:00:00Z",
                "planned_rr": 3.0,
                "unrealized_pnl_usd": 1550.0,
                "timeframe": "4H",
                "linked_play": {
                    "id": 99,
                    "title": "SOL breakout thesis",
                    "conditions_met": "3/4"
                }
            }
        ],
        "portfolio_heat": {
            "total_exposure": 20,
            "long_count": 2,
            "short_count": 0
        }
    }"#;
    let resp: PositionsResponse = de(json);
    let pos = &resp.positions[0];
    assert_eq!(pos.asset, "SOL");
    assert_eq!(pos.entry_price, Some(150.0));
    assert_eq!(pos.unrealized_pnl_pct, Some(10.33));
    let lp = pos.linked_play.as_ref().unwrap();
    assert_eq!(lp.id, 99);
    assert_eq!(lp.title, "SOL breakout thesis");
    let heat = resp.portfolio_heat.as_ref().unwrap();
    assert_eq!(heat.long_count, 2);
}

// ═══════════════════════════════════════════════════════════════════════════
// GET /api/batch?assets=BTC
// ═══════════════════════════════════════════════════════════════════════════

/// Minimum batch response — asset present but all inner blocks absent.
#[test]
fn batch_response_with_empty_asset_block_deserializes() {
    let json = r#"{
        "assets": {
            "BTC": {}
        }
    }"#;
    let resp: BatchResponse = de(json);
    let btc = resp.assets.get("BTC").expect("BTC block missing");
    assert!(btc.market.is_none());
    assert!(btc.scanner.is_none());
    assert!(btc.plays.is_empty());
}

/// Full batch block: scanner with long/short sides, market block, plays list.
#[test]
fn batch_response_with_scanner_market_and_plays_deserializes() {
    let json = r#"{
        "assets": {
            "BTC": {
                "market": {
                    "symbol": "BTC",
                    "price": 68000.0,
                    "change_24h": 2.5
                },
                "scanner": {
                    "long": { "score": 72, "verdict": "STRONG_LONG" },
                    "short": { "score": 28, "verdict": "NO_TRADE" },
                    "best_verdict": "STRONG_LONG"
                },
                "plays": [
                    {
                        "id": 5,
                        "title": "BTC retest long",
                        "status": "active",
                        "direction": "long"
                    }
                ]
            }
        },
        "availability": {
            "scanner": "ok",
            "prices": "ok",
            "plays": "ok"
        }
    }"#;
    let resp: BatchResponse = de(json);
    let btc = resp.assets.get("BTC").unwrap();

    let market = btc.market.as_ref().unwrap();
    assert_eq!(market.symbol, "BTC");
    assert_eq!(market.price, Some(68000.0));

    let scanner = btc.scanner.as_ref().unwrap();
    let long_side = scanner.long.as_ref().unwrap();
    assert_eq!(long_side.score, 72);
    assert_eq!(long_side.verdict, "STRONG_LONG");
    assert_eq!(scanner.best_verdict.as_deref(), Some("STRONG_LONG"));

    assert_eq!(btc.plays.len(), 1);
    assert_eq!(btc.plays[0].id, 5);
    assert_eq!(btc.plays[0].title, "BTC retest long");
}

// ═══════════════════════════════════════════════════════════════════════════
// GET /api/plays/active
// ═══════════════════════════════════════════════════════════════════════════

/// Play with minimum fields (id + title only).
#[test]
fn play_with_required_fields_deserializes() {
    let json = r#"{
        "plays": [
            { "id": 1, "title": "BTC weekly squeeze" }
        ],
        "count": 1
    }"#;
    let resp: PlaysResponse = de(json);
    assert_eq!(resp.plays.len(), 1);
    let play = &resp.plays[0];
    assert_eq!(play.id, 1);
    assert_eq!(play.title, "BTC weekly squeeze");
    assert!(play.conditions.is_none());
}

/// Play conditions match the DSL shape: logic + rules array with met/description/evidence.
#[test]
fn play_conditions_match_dsl_shape() {
    let json = r#"{
        "plays": [
            {
                "id": 3,
                "title": "ETH accumulation zone",
                "direction": "long",
                "primary_asset": "ETH",
                "status": "active",
                "timeframe": "1D",
                "thesis": "Price consolidating above key support",
                "action_plan": "Enter on break of 3200, stop 2900",
                "expiry": "2026-06-01",
                "conditions": {
                    "logic": "AND",
                    "rules": [
                        { "met": true,  "description": "Price above 200 EMA", "evidence": "price 3150 > ema200 2980" },
                        { "met": false, "description": "RSI reclaim above 50",  "evidence": "rsi 47.3" }
                    ]
                },
                "invalidation": {
                    "met": false,
                    "description": "Close below 2800",
                    "evidence": null
                }
            }
        ],
        "count": 1
    }"#;
    let resp: PlaysResponse = de(json);
    let play = &resp.plays[0];
    assert_eq!(play.primary_asset.as_deref(), Some("ETH"));
    assert_eq!(play.status.as_deref(), Some("active"));

    let cond = play.conditions.as_ref().unwrap();
    assert_eq!(cond.logic.as_deref(), Some("AND"));
    assert_eq!(cond.rules.len(), 2);
    assert!(cond.rules[0].met);
    assert!(!cond.rules[1].met);
    assert_eq!(
        cond.rules[0].description.as_deref(),
        Some("Price above 200 EMA")
    );

    // progress() helper must reflect the fixture state
    assert_eq!(play.progress(), "1/2");
    let (met, total) = play.condition_counts();
    assert_eq!((met, total), (1, 2));

    let inv = play.invalidation.as_ref().unwrap();
    assert!(!inv.met);
}

/// `/api/plays/active` ships the invalidation text as `note` (with `type` +
/// `params` metadata) — the client must surface it as `description`, or the
/// detail card renders an empty "• —" line.
#[test]
fn play_invalidation_note_shape_maps_to_description() {
    let json = r#"{
        "id": 19,
        "title": "ETH Relative-Weakness Short",
        "invalidation": {
            "note": "1D close back above the 1,480 broken shelf",
            "type": "candle_close",
            "params": { "level": 1480, "relation": "above", "timeframe": "1D" }
        }
    }"#;
    let play: Play = de(json);
    let inv = play.invalidation.as_ref().unwrap();
    assert!(!inv.met, "met defaults false when the wire omits it");
    assert_eq!(
        inv.description.as_deref(),
        Some("1D close back above the 1,480 broken shelf"),
        "note must alias into description"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// GET /api/alerts/triggered
// ═══════════════════════════════════════════════════════════════════════════

/// Alert where `id` arrives as an integer (most common server shape).
#[test]
fn triggered_alert_with_integer_id_deserializes() {
    let json = r#"{
        "triggered": [
            {
                "id": 101,
                "symbol": "BTC",
                "alert_type": "price",
                "condition": "above",
                "price": 70000.0,
                "triggered_at": "2026-05-20T08:00:00Z",
                "triggered_price": 70050.0,
                "active": false,
                "triggered": true
            }
        ]
    }"#;
    let resp: TriggeredAlertsResponse = de(json);
    assert_eq!(resp.triggered.len(), 1);
    let alert = &resp.triggered[0];
    assert_eq!(alert.id_str(), "101");
    assert_eq!(alert.symbol.as_deref(), Some("BTC"));
    assert_eq!(alert.condition.as_deref(), Some("above"));
    assert_eq!(alert.price, Some(70000.0));
}

/// Alert where `id` arrives as a string — known polymorphism in the server response.
#[test]
fn triggered_alert_with_string_id_deserializes() {
    let json = r#"{
        "triggered": [
            {
                "id": "alert-uuid-abc123",
                "symbol": "ETH",
                "alert_type": "indicator",
                "indicator": "RSI",
                "target_state": "oversold",
                "active": true,
                "triggered": false,
                "frequency": "once",
                "note": "Weekly oversold check"
            }
        ]
    }"#;
    let resp: TriggeredAlertsResponse = de(json);
    let alert = &resp.triggered[0];
    assert_eq!(alert.id_str(), "alert-uuid-abc123");
    assert_eq!(alert.symbol.as_deref(), Some("ETH"));
    assert_eq!(alert.alert_type.as_deref(), Some("indicator"));
    assert_eq!(alert.target_state.as_deref(), Some("oversold"));
    assert_eq!(alert.frequency.as_deref(), Some("once"));
    assert_eq!(alert.note.as_deref(), Some("Weekly oversold check"));
}

/// Alert with no id at all — id field absent, id_str() returns "—".
#[test]
fn triggered_alert_with_missing_id_returns_dash() {
    let json = r#"{
        "triggered": [
            { "symbol": "SOL", "alert_type": "price" }
        ]
    }"#;
    let resp: TriggeredAlertsResponse = de(json);
    let alert = &resp.triggered[0];
    assert_eq!(alert.id_str(), "—");
}

// ═══════════════════════════════════════════════════════════════════════════
// GET /api/sentiment
// ═══════════════════════════════════════════════════════════════════════════

/// Fear & Greed minimum fixture — value and classification required.
#[test]
fn fear_greed_required_fields_deserialize() {
    let json = r#"{
        "sentiment": {
            "fear_greed": {
                "value": 62,
                "classification": "Greed"
            }
        }
    }"#;
    let resp: SentimentResponse = de(json);
    let fg = resp.sentiment.fear_greed.as_ref().unwrap();
    assert_eq!(fg.value, 62);
    assert_eq!(fg.classification, "Greed");
    assert!(resp.sentiment.regime.is_none());
}

/// Full sentiment response with FearGreed + Regime + components.
#[test]
fn sentiment_with_regime_and_components_deserializes() {
    let json = r#"{
        "sentiment": {
            "fear_greed": {
                "value": 35,
                "classification": "Fear"
            },
            "regime": {
                "regime": "Bear",
                "score": -42,
                "components": [
                    { "name": "Breadth", "detail": "narrow", "score": -20 },
                    { "name": "Momentum", "detail": "declining", "score": -22 }
                ]
            }
        }
    }"#;
    let resp: SentimentResponse = de(json);
    let fg = resp.sentiment.fear_greed.as_ref().unwrap();
    assert_eq!(fg.value, 35);
    assert_eq!(fg.classification, "Fear");

    let regime = resp.sentiment.regime.as_ref().unwrap();
    assert_eq!(regime.regime, "Bear");
    assert_eq!(regime.score, -42);
    assert_eq!(regime.components.len(), 2);
    assert_eq!(regime.breadth(), Some("narrow"));
}

/// Sentiment with no fear_greed or regime — fully absent inner fields are fine.
#[test]
fn sentiment_with_empty_inner_blocks_deserializes() {
    let json = r#"{ "sentiment": {} }"#;
    let resp: SentimentResponse = de(json);
    assert!(resp.sentiment.fear_greed.is_none());
    assert!(resp.sentiment.regime.is_none());
}
