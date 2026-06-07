use serde::Deserialize;
use std::io::Write;
use std::path::PathBuf;

use crate::client::Client;

const DEFAULT_SYMBOLS: &[&str] = &["BTC", "ETH", "SOL", "BNB", "LINK", "SPX", "GOLD", "WTI"];

#[derive(Deserialize, Default)]
struct WatchlistFile {
    #[serde(default)]
    symbols: Option<Vec<String>>,
    #[serde(default)]
    default: Option<Vec<String>>,
}

fn config_path() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("cc", "carrac", "carrac")?;
    Some(dirs.config_dir().join("watchlist.toml"))
}

/// Load symbols from `~/.config/carrac/carrac/watchlist.toml`.
/// Falls back to baked-in default list without crashing.
/// Writes the default file on first run if the path doesn't exist.
pub fn load() -> Vec<String> {
    write_default_if_missing();

    let Some(path) = config_path() else {
        return default_symbols();
    };

    let content = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return default_symbols(),
    };

    parse_watchlist(&content).unwrap_or_else(|e| {
        eprintln!("carrac: watchlist.toml malformed ({e}); using built-in defaults");
        default_symbols()
    })
}

/// Fetch the caller's server-side watchlist (`GET /api/watchlist` → flat array).
///
/// Returns `Some(list)` only on a successful, non-empty response. Returns `None`
/// on any failure (offline, auth, pre-sync server build) or an empty list — the
/// caller then falls back to the local cache via [`load`]. One-way sync: the
/// server is the source of truth; the TUI does not write the list back (yet).
pub async fn fetch_remote(client: &Client) -> Option<Vec<String>> {
    match client.get::<Vec<String>>("/api/watchlist").await {
        Ok(list) => pick_remote(list),
        Err(_) => None,
    }
}

/// A remote list is adopted only when non-empty; an empty server list means
/// "fall back to the local cache" rather than blanking the user's grid.
fn pick_remote(list: Vec<String>) -> Option<Vec<String>> {
    if list.is_empty() { None } else { Some(list) }
}

/// Overwrite the local `watchlist.toml` cache with `symbols`.
///
/// Best-effort: write failures are ignored (the in-memory list stays
/// authoritative for the session). Persisting the server list lets the next
/// offline launch *and* the background pollers — which independently re-read the
/// file via [`load`] each tick — pick up the user's synced watchlist.
pub fn save_cache(symbols: &[String]) {
    let Some(path) = config_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::File::create(&path) {
        let _ = f.write_all(serialize_watchlist(symbols).as_bytes());
    }
}

/// Serialize a symbol list to the `watchlist.toml` body. Round-trips through
/// [`parse_watchlist`]; extracted so the cache format can be tested without
/// touching the real cache file.
fn serialize_watchlist(symbols: &[String]) -> String {
    let quoted: Vec<String> = symbols.iter().map(|s| format!("\"{s}\"")).collect();
    format!("symbols = [{}]\n", quoted.join(", "))
}

fn parse_watchlist(content: &str) -> Result<Vec<String>, String> {
    let parsed: WatchlistFile = toml::from_str(content).map_err(|e| e.to_string())?;
    // Prefer `symbols`, fall back to `default`, then bail.
    if let Some(list) = parsed.symbols.filter(|v| !v.is_empty()) {
        return Ok(list);
    }
    if let Some(list) = parsed.default.filter(|v| !v.is_empty()) {
        return Ok(list);
    }
    Err("no `symbols` or `default` key with values found".into())
}

/// Seed `~/.config/carrac/carrac/watchlist.toml` the first time carrac is run.
pub fn write_default_if_missing() {
    let Some(path) = config_path() else { return };
    if path.exists() {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let syms: Vec<String> = DEFAULT_SYMBOLS.iter().map(|s| format!("\"{s}\"")).collect();
    let content = format!("symbols = [{}]\n", syms.join(", "));
    if let Ok(mut f) = std::fs::File::create(&path) {
        let _ = f.write_all(content.as_bytes());
    }
}

fn default_symbols() -> Vec<String> {
    DEFAULT_SYMBOLS.iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_symbols_key() {
        let content = r#"symbols = ["BTC", "ETH", "SOL"]"#;
        let result = parse_watchlist(content).unwrap();
        assert_eq!(result, vec!["BTC", "ETH", "SOL"]);
    }

    #[test]
    fn parses_default_key() {
        let content = r#"default = ["BTC", "ETH"]"#;
        let result = parse_watchlist(content).unwrap();
        assert_eq!(result, vec!["BTC", "ETH"]);
    }

    #[test]
    fn parses_multiline_array() {
        // Hand-rolled parser couldn't handle this; toml crate can.
        let content = "symbols = [\n  \"BTC\",\n  \"ETH\",\n  \"SOL\",\n]\n";
        let result = parse_watchlist(content).unwrap();
        assert_eq!(result, vec!["BTC", "ETH", "SOL"]);
    }

    #[test]
    fn falls_back_on_empty() {
        let result = parse_watchlist("# empty file");
        assert!(result.is_err());
    }

    // ── server-sync helpers ──────────────────────────────────────────────────

    #[test]
    fn serialize_round_trips_through_parse() {
        // The cache we write from a server sync must be loadable by `load()`.
        let syms = vec!["BTC".to_string(), "ETH".to_string(), "SOL".to_string()];
        let toml = serialize_watchlist(&syms);
        assert_eq!(parse_watchlist(&toml).unwrap(), syms);
    }

    #[test]
    fn serialize_single_symbol_format() {
        let toml = serialize_watchlist(&["BTC".to_string()]);
        assert_eq!(toml, "symbols = [\"BTC\"]\n");
        assert_eq!(parse_watchlist(&toml).unwrap(), vec!["BTC"]);
    }

    #[test]
    fn pick_remote_keeps_nonempty_server_list() {
        // Non-empty server list takes precedence over the local cache.
        assert_eq!(
            pick_remote(vec!["BTC".to_string(), "ETH".to_string()]),
            Some(vec!["BTC".to_string(), "ETH".to_string()]),
        );
    }

    #[test]
    fn pick_remote_rejects_empty_so_caller_falls_back() {
        // Empty server response → None → caller keeps the local cache rather
        // than blanking the grid.
        assert_eq!(pick_remote(vec![]), None);
    }
}
