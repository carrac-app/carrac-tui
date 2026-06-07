//! Screening-matrix types — the `GET /api/details/matrix` response.
//!
//! This is the web's own Screening Matrix endpoint: it returns typed indicator
//! columns for the caller's watchlist assets, computed by the server's
//! indicator engine. The TUI Details grid consumes
//! it directly so its columns match the web **by construction** — no more
//! re-deriving values from scanner `evidence` strings.
//!
//! Only the fields the Details grid renders are modelled; serde ignores the rest
//! of the (much wider) row.

use serde::Deserialize;
use std::collections::HashMap;

/// Weekly-RSI block. `zone` is one of `oversold` / `overbought` / `neutral`.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct MatrixRsi {
    #[serde(default)]
    pub value: Option<f64>,
    #[serde(default)]
    pub zone: Option<String>,
}

/// Market-structure block. `label` is the web's `HH/HL`-style string built from
/// the last swing-high and swing-low types; `trend` drives the colour.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct MatrixStructure {
    #[serde(default)]
    pub trend: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
}

/// Consensio block. `positions`/`max_positions` form the web's `n/9` fraction;
/// `signal` (net, can be negative) drives the colour.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct MatrixConsensio {
    #[serde(default)]
    pub signal: Option<f64>,
    #[serde(default)]
    pub positions: Option<f64>,
    #[serde(default)]
    pub max_positions: Option<f64>,
}

/// CPR block. `price_level` is the fine zone label (`TC-R1`, `S1-BC`, …) the web
/// screener colours; `position` is the coarse above/inside/below fallback.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct MatrixCpr {
    #[serde(default)]
    #[allow(dead_code)] // wire-contract field, not yet surfaced
    pub position: Option<String>,
    #[serde(default)]
    pub price_level: Option<String>,
}

/// Bollinger-band block. `squeeze_pct` (band width vs its 60-day average) is the
/// metric the web colours (<30 extreme, <50 squeeze, >150 expansion).
#[derive(Debug, Deserialize, Default, Clone)]
pub struct MatrixBb {
    #[serde(default)]
    #[allow(dead_code)] // wire-contract field, not yet surfaced
    pub squeezed: Option<bool>,
    #[serde(default)]
    pub squeeze_pct: Option<f64>,
    #[serde(default)]
    #[allow(dead_code)] // wire-contract field, not yet surfaced
    pub width_pct: Option<f64>,
}

/// One matrix row = one asset. Numeric fields are `f64` to tolerate int-or-float
/// JSON; counts are rendered as integers at display time.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct MatrixRow {
    pub symbol: String,
    #[serde(default)]
    pub price: Option<f64>,
    #[serde(default)]
    pub change_pct: Option<f64>,
    #[serde(default)]
    pub rsi: MatrixRsi,
    #[serde(default)]
    pub structure: MatrixStructure,
    #[serde(default)]
    pub consensio: MatrixConsensio,
    #[serde(default)]
    pub cpr: MatrixCpr,
    #[serde(default)]
    pub bb_squeeze: MatrixBb,
}

#[derive(Debug, Deserialize, Default)]
pub struct MatrixResponse {
    #[serde(default)]
    pub rows: Vec<MatrixRow>,
    /// Free-tier callers get a row-limited preview; the TUI is Premium so this is
    /// normally absent. Surfaced so the grid can flag a truncated view if seen.
    #[serde(default)]
    #[allow(dead_code)] // wire-contract field, not yet surfaced
    pub preview_mode: bool,
}

impl MatrixResponse {
    /// Convert the `rows` list into a by-symbol map for O(1) lookup in the render
    /// loop (the grid iterates the watchlist in its own order).
    pub fn into_map(self) -> HashMap<String, MatrixRow> {
        self.rows
            .into_iter()
            .map(|r| (r.symbol.clone(), r))
            .collect()
    }
}
