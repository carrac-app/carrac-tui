//! Server-Sent Events token collector for `/api/ai/chat`.
//!
//! Both `carrac ask` and `carrac scan --batch-ai` consume the same SSE protocol:
//! each `data: {"t":"..."}` line is one streamed token; a `data: {"error":"..."}`
//! line surfaces a server-side tool error. This module is the single source
//! of truth for that parser.

use serde_json::Value;

/// Parse the raw SSE body returned by `Client::post_raw`.
///
/// Returns `(tokens, sse_error)` so callers can decide how to render the
/// mid-stream error (dim note, markdown emphasis, etc.).
pub fn collect_sse_tokens(raw: &str) -> (String, Option<String>) {
    let mut tokens = String::new();
    let mut sse_error: Option<String> = None;

    for line in raw.lines() {
        let line = line.trim();
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        if let Some(token) = v.get("t").and_then(|t| t.as_str()) {
            tokens.push_str(token);
        } else if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
            sse_error = Some(err.to_string());
        }
    }

    (tokens, sse_error)
}

/// Stateful parser for a streaming SSE response consumed chunk-by-chunk.
///
/// Call [`SseStreamParser::feed`] for each raw bytes chunk received from the
/// network. It accumulates a line-buffer and yields fully-formed tokens as
/// they complete. Any leftover incomplete data is kept until the next call.
#[derive(Default)]
pub struct SseStreamParser {
    buf: String,
}

impl SseStreamParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `chunk` to the internal buffer and extract any complete SSE events.
    ///
    /// Returns `(tokens, sse_error)`:
    /// - `tokens` — zero or more `"t"` values, in order.
    /// - `sse_error` — the first `"error"` value seen in this chunk, if any.
    pub fn feed(&mut self, chunk: &str) -> (Vec<String>, Option<String>) {
        self.buf.push_str(chunk);
        let (tokens, sse_err) = parse_sse_lines(&self.buf);
        // Keep only the trailing incomplete line (everything after the last '\n').
        if let Some(last_nl) = self.buf.rfind('\n') {
            self.buf = self.buf[last_nl + 1..].to_string();
        }
        // If there was no newline at all the whole buffer is still incomplete —
        // keep it intact (tokens/err will be empty anyway in that case).
        (tokens, sse_err)
    }
}

/// Parse complete SSE `data:` lines from `text`.
///
/// Only lines that end (i.e. are followed by '\n') are processed; a trailing
/// partial line without a newline is left to the caller to buffer.
fn parse_sse_lines(text: &str) -> (Vec<String>, Option<String>) {
    let mut tokens = Vec::new();
    let mut sse_error: Option<String> = None;

    // Iterate only over lines that were terminated by a newline — the last
    // element of `lines()` may be a partial line if there's no trailing '\n',
    // so we use the raw byte approach instead.
    let complete_part = match text.rfind('\n') {
        Some(pos) => &text[..=pos],
        None => return (tokens, sse_error), // nothing complete yet
    };

    for line in complete_part.lines() {
        let line = line.trim();
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        if let Some(token) = v.get("t").and_then(|t| t.as_str()) {
            tokens.push(token.to_string());
        } else if let Some(err) = v.get("error").and_then(|e| e.as_str())
            && sse_error.is_none()
        {
            sse_error = Some(err.to_string());
        }
    }

    (tokens, sse_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_tokens_in_order() {
        let raw = "data: {\"t\":\"hello \"}\n\ndata: {\"t\":\"world\"}\n";
        let (tokens, err) = collect_sse_tokens(raw);
        assert_eq!(tokens, "hello world");
        assert!(err.is_none());
    }

    #[test]
    fn captures_sse_error_with_tokens() {
        let raw = "data: {\"t\":\"partial\"}\ndata: {\"error\":\"tool x failed\"}\n";
        let (tokens, err) = collect_sse_tokens(raw);
        assert_eq!(tokens, "partial");
        assert_eq!(err.as_deref(), Some("tool x failed"));
    }

    #[test]
    fn captures_error_only() {
        let raw = "data: {\"error\":\"no tool available\"}\n";
        let (tokens, err) = collect_sse_tokens(raw);
        assert!(tokens.is_empty());
        assert_eq!(err.as_deref(), Some("no tool available"));
    }

    #[test]
    fn ignores_unparseable_data_lines() {
        let raw = "data: not-json\ndata: {\"t\":\"ok\"}\n";
        let (tokens, _) = collect_sse_tokens(raw);
        assert_eq!(tokens, "ok");
    }

    // ─── SseStreamParser tests ────────────────────────────────────────────────

    #[test]
    fn parser_partial_event_at_chunk_boundary() {
        // The SSE event is split across two chunks — no token emitted until
        // the second chunk brings the newline terminator.
        let mut p = SseStreamParser::new();
        let (tokens, err) = p.feed("data: {\"t\":\"hel");
        assert!(tokens.is_empty(), "no complete event yet");
        assert!(err.is_none());
        let (tokens, err) = p.feed("lo\"}\n");
        assert_eq!(tokens, vec!["hello"]);
        assert!(err.is_none());
    }

    #[test]
    fn parser_multiple_events_in_one_chunk() {
        let mut p = SseStreamParser::new();
        let chunk = "data: {\"t\":\"foo\"}\ndata: {\"t\":\"bar\"}\ndata: {\"t\":\"baz\"}\n";
        let (tokens, err) = p.feed(chunk);
        assert_eq!(tokens, vec!["foo", "bar", "baz"]);
        assert!(err.is_none());
    }

    #[test]
    fn parser_malformed_line_in_middle_of_stream() {
        // A non-JSON line between valid events should be skipped; subsequent
        // good tokens must still arrive.
        let mut p = SseStreamParser::new();
        let chunk = "data: {\"t\":\"before\"}\ndata: not-json\ndata: {\"t\":\"after\"}\n";
        let (tokens, err) = p.feed(chunk);
        assert_eq!(tokens, vec!["before", "after"]);
        assert!(err.is_none());
    }

    #[test]
    fn parser_sse_error_mid_stream() {
        let mut p = SseStreamParser::new();
        let chunk =
            "data: {\"t\":\"partial\"}\ndata: {\"error\":\"tool boom\"}\ndata: {\"t\":\"rest\"}\n";
        let (tokens, err) = p.feed(chunk);
        // Tokens before and after the error line are collected.
        assert_eq!(tokens, vec!["partial", "rest"]);
        assert_eq!(err.as_deref(), Some("tool boom"));
    }

    #[test]
    fn parser_retains_incomplete_trailing_data() {
        // Two complete events + a partial trailing line.  The partial must be
        // held in the buffer and combined with the next chunk.
        let mut p = SseStreamParser::new();
        let (t1, _) = p.feed("data: {\"t\":\"a\"}\ndata: {\"t");
        assert_eq!(t1, vec!["a"]);
        let (t2, _) = p.feed("\":\"b\"}\n");
        assert_eq!(t2, vec!["b"]);
    }
}
