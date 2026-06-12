//! Types for the `/api/market-news` endpoint.

use std::collections::HashMap;

use serde::{Deserialize, Deserializer};

/// Deserialize a possibly-`null`/missing string field as `""`.
///
/// The news feed aggregates third-party RSS sources; an individual item with a
/// `null` summary or link must not fail the whole `Vec<NewsItem>` deserialize
/// and silently empty the tab.
fn null_to_empty<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewsItem {
    #[serde(default, deserialize_with = "null_to_empty")]
    #[allow(dead_code)] // wire-contract field, not yet surfaced
    pub id: String,
    #[serde(default, deserialize_with = "null_to_empty")]
    pub title: String,
    #[serde(default, deserialize_with = "null_to_empty")]
    pub summary: String,
    #[serde(default, deserialize_with = "null_to_empty")]
    pub link: String,
    #[serde(default, deserialize_with = "null_to_empty")]
    pub published: String,
    #[serde(default, deserialize_with = "null_to_empty")]
    pub source: String,
    #[serde(default, deserialize_with = "null_to_empty")]
    pub source_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewsResponse {
    #[serde(default)]
    pub items: Vec<NewsItem>,
    #[serde(default)]
    pub feeds: HashMap<String, String>,
    #[serde(default)]
    #[allow(dead_code)] // wire-contract field, not yet surfaced
    pub timestamp: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_news_response() {
        let json = r#"{
            "status": "ok",
            "timestamp": "2026-05-23T23:07:19+00:00",
            "items": [
                {
                    "id": "abc123",
                    "title": "BTC hits new high",
                    "summary": "Bitcoin set a new ATH today.",
                    "link": "https://example.com/btc",
                    "published": "2026-05-23T23:07:19+00:00",
                    "source": "CoinTelegraph",
                    "source_id": "cointelegraph"
                }
            ],
            "feeds": {
                "cointelegraph": "ok",
                "coindesk": "ok"
            }
        }"#;
        let resp: NewsResponse = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(resp.items.len(), 1);
        assert_eq!(resp.items[0].title, "BTC hits new high");
        assert_eq!(resp.items[0].source_id, "cointelegraph");
        assert_eq!(
            resp.feeds.get("coindesk").map(std::string::String::as_str),
            Some("ok")
        );
        assert_eq!(resp.timestamp, "2026-05-23T23:07:19+00:00");
    }

    #[test]
    fn null_summary_does_not_sink_the_feed() {
        // A single item with a null summary/link must still deserialize — one
        // malformed scraped item should never empty the whole News tab.
        let json = r#"{
            "items": [
                {"id":"1","title":"Headline","summary":null,"link":null,
                 "published":"2026-05-23T23:07:19+00:00","source":"X","source_id":"x"},
                {"id":"2","title":"Second","summary":"ok","link":"https://example.com",
                 "published":"2026-05-23T23:07:19+00:00","source":"Y","source_id":"y"}
            ],
            "feeds": {}
        }"#;
        let resp: NewsResponse = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(resp.items.len(), 2);
        assert_eq!(resp.items[0].summary, "");
        assert_eq!(resp.items[0].link, "");
        assert_eq!(resp.items[1].summary, "ok");
    }

    #[test]
    fn deserialize_empty_items() {
        let json = r#"{
            "status": "ok",
            "timestamp": "2026-05-23T12:00:00+00:00",
            "items": [],
            "feeds": {}
        }"#;
        let resp: NewsResponse = serde_json::from_str(json).expect("should deserialize");
        assert!(resp.items.is_empty());
        assert!(resp.feeds.is_empty());
    }
}
