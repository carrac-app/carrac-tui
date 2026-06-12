use anyhow::Result;

use crate::client::Client;
use crate::types::batch::BatchResponse;

// `/api/batch` rejects requests with more than this many symbols, so larger
// chunks would 400 the whole fetch. fetch_batch chunks to stay within the limit
// and merges the results transparently.
const CHUNK_SIZE: usize = 10;

/// Fetch batch data for multiple symbols, chunking into groups of `CHUNK_SIZE`.
/// Multiple chunks are fetched sequentially; a failed chunk is skipped and the
/// successful chunks are merged into a single `BatchResponse`. This is library
/// code, so it never prints — an `eprintln` here would smear an active TUI's
/// alternate screen (callers surface missing data themselves).
pub async fn fetch_batch(
    client: &Client,
    symbols: &[String],
    include: &[&str],
) -> Result<BatchResponse> {
    if symbols.is_empty() {
        return Ok(BatchResponse::default());
    }

    let include_str = include.join(",");
    let chunks: Vec<&[String]> = symbols.chunks(CHUNK_SIZE).collect();

    // Single chunk — zero-copy fast path.
    if chunks.len() == 1 {
        return fetch_chunk(client, chunks[0], &include_str).await;
    }

    // Multiple chunks: sequential fetch + merge.
    let mut merged = BatchResponse::default();
    for chunk in chunks {
        match fetch_chunk(client, chunk, &include_str).await {
            Ok(resp) => {
                merged.assets.extend(resp.assets);
                merge_availability(&mut merged, resp.availability);
            }
            Err(_e) => {
                // Skip the failed chunk and keep going — never print from here
                // (it would corrupt an active TUI). Callers report missing data.
            }
        }
    }

    Ok(merged)
}

async fn fetch_chunk(
    client: &Client,
    symbols: &[String],
    include_str: &str,
) -> Result<BatchResponse> {
    let assets_param = symbols.join(",");
    let path = format!("/api/batch?assets={assets_param}&include={include_str}");
    client.get::<BatchResponse>(&path).await
}

fn merge_availability(dst: &mut BatchResponse, src: crate::types::batch::BatchAvailability) {
    if src.scanner.is_some() {
        dst.availability.scanner = src.scanner;
    }
    if src.prices.is_some() {
        dst.availability.prices = src.prices;
    }
    if src.plays.is_some() {
        dst.availability.plays = src.plays;
    }
    if src.htf.is_some() {
        dst.availability.htf = src.htf;
    }
}
