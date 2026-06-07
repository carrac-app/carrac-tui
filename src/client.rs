use anyhow::{Context, Result, anyhow};
use reqwest::StatusCode;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::time::Duration;

use crate::config::Config;

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    /// Extended-timeout client for SSE / long-running AI endpoints (60 s).
    http_long: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl Client {
    pub fn new(cfg: &Config) -> Result<Self> {
        let ua = concat!("carrac-tui/", env!("CARGO_PKG_VERSION"));
        let http = reqwest::Client::builder()
            .user_agent(ua)
            .timeout(Duration::from_secs(10))
            .gzip(true)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("building reqwest client")?;
        let http_long = reqwest::Client::builder()
            .user_agent(ua)
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("building long-timeout reqwest client")?;
        Ok(Self {
            http,
            http_long,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            api_key: cfg.api_key.clone(),
        })
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self.send(path).await?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .with_context(|| format!("GET {path}: reading response body"))?;
        if !status.is_success() {
            return Err(http_error("GET", path, status, &body));
        }
        serde_json::from_str(&body)
            .with_context(|| format!("GET {path}: decoding response body ({} bytes)", body.len()))
    }

    /// POST a JSON body and deserialize the response.  Same auth, timeout, and
    /// 302 detection as `get()`.  Returns `Err` with status on non-2xx.
    pub async fn post<T: DeserializeOwned, B: Serialize>(&self, path: &str, body: &B) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .post(&url)
            .header("X-API-Key", &self.api_key)
            .header("Accept", "application/json")
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .with_context(|| format!("POST {path}: reading response body"))?;
        if !status.is_success() {
            return Err(http_error("POST", path, status, &text));
        }
        serde_json::from_str(&text)
            .with_context(|| format!("POST {path}: decoding response body ({} bytes)", text.len()))
    }

    /// POST a JSON body and return the raw response text (e.g. for SSE streams).
    /// Uses the 60-second long-timeout client; suitable for AI endpoints.
    pub async fn post_raw<B: Serialize>(&self, path: &str, body: &B) -> Result<String> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http_long
            .post(&url)
            .header("X-API-Key", &self.api_key)
            .header("Accept", "text/event-stream, application/json")
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .with_context(|| format!("POST {path}: reading response body"))?;
        if !status.is_success() {
            return Err(http_error("POST", path, status, &text));
        }
        Ok(text)
    }

    /// POST a JSON body and return the raw `reqwest::Response` for streaming.
    ///
    /// Uses the 60-second long-timeout client; suitable for SSE/AI endpoints.
    /// The caller should consume the body via `resp.chunk().await` in a loop.
    ///
    /// Auth and 302/401 detection mirrors `post_raw`: returns
    /// `Err("authentication required")` on those status codes.
    pub async fn post_stream<B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<reqwest::Response> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http_long
            .post(&url)
            .header("X-API-Key", &self.api_key)
            .header("Accept", "text/event-stream, application/json")
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        if status == StatusCode::FOUND
            || status == StatusCode::SEE_OTHER
            || status == StatusCode::UNAUTHORIZED
        {
            return Err(anyhow!("authentication required (POST {path} -> {status})"));
        }
        if !status.is_success() {
            let text = resp
                .text()
                .await
                .with_context(|| format!("POST {path}: reading error body"))?;
            return Err(http_error("POST", path, status, &text));
        }
        Ok(resp)
    }

    /// DELETE a resource and deserialize the response body.
    /// Same auth and 302 detection as `post()`.  Returns `Err` on non-2xx.
    pub async fn delete<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .delete(&url)
            .header("X-API-Key", &self.api_key)
            .header("Accept", "application/json")
            .send()
            .await
            .with_context(|| format!("DELETE {url}"))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .with_context(|| format!("DELETE {path}: reading response body"))?;
        if !status.is_success() {
            return Err(http_error("DELETE", path, status, &text));
        }
        serde_json::from_str(&text).with_context(|| {
            format!(
                "DELETE {path}: decoding response body ({} bytes)",
                text.len()
            )
        })
    }

    /// GET a path and return the raw response body as `Vec<u8>`.
    /// Returns `Err` on non-2xx status codes, same as `get()`.
    pub async fn get_bytes(&self, path: &str) -> Result<Vec<u8>> {
        let resp = self.send(path).await?;
        let status = resp.status();
        let body = resp
            .bytes()
            .await
            .with_context(|| format!("GET {path}: reading response bytes"))?;
        if !status.is_success() {
            let snippet: String = body.iter().take(200).map(|&b| b as char).collect();
            return Err(http_error("GET", path, status, &snippet));
        }
        Ok(body.to_vec())
    }

    /// Issue the GET with bounded retry + backoff on transient failures.
    ///
    /// Retries (up to `MAX_RETRIES` times) on transport errors (timeout / connect)
    /// and on transient HTTP statuses (429, 502, 503, 504). A 429 honors a sane
    /// `Retry-After` header; otherwise backoff is exponential (300ms, 600ms). All
    /// other statuses — including 4xx/5xx app errors — are returned to the caller
    /// unchanged so the existing status handling still runs.
    async fn send(&self, path: &str) -> Result<reqwest::Response> {
        let url = format!("{}{}", self.base_url, path);
        let mut attempt: u32 = 0;
        loop {
            let result = self
                .http
                .get(&url)
                .header("X-API-Key", &self.api_key)
                .header("Accept", "application/json")
                .send()
                .await;

            match result {
                Ok(resp) => {
                    if is_retryable_status(resp.status()) && attempt < MAX_RETRIES {
                        let delay = retry_delay(&resp, attempt);
                        attempt += 1;
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return Ok(resp);
                }
                Err(e) => {
                    let transient = e.is_timeout() || e.is_connect();
                    if transient && attempt < MAX_RETRIES {
                        let delay = Duration::from_millis(BASE_BACKOFF_MS << attempt);
                        attempt += 1;
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return Err(e).with_context(|| format!("GET {url}"));
                }
            }
        }
    }
}

/// Max retry attempts after the initial request (so 3 total tries).
const MAX_RETRIES: u32 = 2;
/// Base backoff; doubles each attempt (300ms, then 600ms).
const BASE_BACKOFF_MS: u64 = 300;

/// Transient statuses worth retrying. Deliberately excludes 500 (usually a
/// deterministic app error — retrying just hammers the backend).
fn is_retryable_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS    // 429
            | StatusCode::BAD_GATEWAY        // 502
            | StatusCode::SERVICE_UNAVAILABLE // 503
            | StatusCode::GATEWAY_TIMEOUT // 504
    )
}

/// Backoff for a retryable response. On 429, honor `Retry-After` (seconds) when
/// present and ≤10s; otherwise exponential backoff.
fn retry_delay(resp: &reqwest::Response, attempt: u32) -> Duration {
    if resp.status() == StatusCode::TOO_MANY_REQUESTS
        && let Some(secs) = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<u64>().ok())
        && secs <= 10
    {
        return Duration::from_secs(secs);
    }
    Duration::from_millis(BASE_BACKOFF_MS << attempt)
}

fn http_error(method: &str, path: &str, status: StatusCode, body: &str) -> anyhow::Error {
    let snippet = body.chars().take(200).collect::<String>();
    if status == StatusCode::FOUND || status == StatusCode::SEE_OTHER {
        return anyhow!(
            "{method} {path} -> {status} (redirect; endpoint may not accept X-API-Key)"
        );
    }
    anyhow!("{method} {path} -> {status}: {snippet}")
}
