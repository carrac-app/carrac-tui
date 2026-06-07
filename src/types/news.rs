//! Types for the `/api/market-news` endpoint.

use std::collections::HashMap;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct NewsItem {
    #[allow(dead_code)] // wire-contract field, not yet surfaced
    pub id: String,
    pub title: String,
    pub summary: String,
    pub link: String,
    pub published: String,
    pub source: String,
    pub source_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewsResponse {
    pub items: Vec<NewsItem>,
    pub feeds: HashMap<String, String>,
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
        assert_eq!(resp.feeds.get("coindesk").map(|s| s.as_str()), Some("ok"));
        assert_eq!(resp.timestamp, "2026-05-23T23:07:19+00:00");
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
