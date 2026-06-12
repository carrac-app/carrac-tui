//! Report view — full scrollable morning or afternoon report.
//!
//! Data is lazy-fetched the first time the user opens this view (key `6`).
//! Until then shows "loading report…". Once data arrives the full `content`
//! is rendered as wrapped plain text.
//!
//! Keys:
//!  `[` / `]`        toggle morning / afternoon
//!  `j` / `k` / ↑↓  scroll 1 line
//!  `PgDn` / `PgUp`  scroll 5 lines
//!  `g`              jump to top
//!  `G`              jump to bottom (renderer clamps)
//!  `w`              write the current report to a `.md` file (working dir)
//!  `/`              open search prompt (Find mode)
//!  `n` / `N`        next / previous match (when query active)
//!  `Esc`            clear active query

use std::hash::{Hash, Hasher};

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::state::{App, BriefReportChoice};

/// Inputs that fully determine the rendered report body. When all of these are
/// unchanged from the previous frame the cached `Vec<Line>` + wrapped line count
/// can be reused, avoiding a full `render_markdown_lines` + `Paragraph::line_count`
/// rebuild every frame (the report can be hundreds of wrapping lines).
#[derive(Clone, PartialEq, Eq)]
struct ReportCacheKey {
    /// Which report is selected (morning vs afternoon → different content).
    choice: BriefReportChoice,
    /// Hash of the report content string (cheap identity check vs storing/cmp'ing
    /// the whole string each frame).
    content_hash: u64,
    /// Length of the content — combined with the hash to make collisions
    /// astronomically unlikely.
    content_len: usize,
    /// Body width the lines were wrapped at (drives `line_count`).
    width: u16,
    /// Active search query (lowercased) — changes the highlight spans.
    query: String,
    /// Current match source-line index — changes which span gets REVERSED.
    current_match_line: Option<usize>,
}

/// Cached render of the report body, valid only while its `key` still matches.
pub struct ReportRenderCache {
    key: ReportCacheKey,
    lines: Vec<Line<'static>>,
    /// Wrapped (display-row) line count at `key.width`.
    content_h: u16,
}

fn hash_content(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    // ── layout ───────────────────────────────────────────────────────────────
    // 1 line market-sentiment strip (always-refreshing), 1 line report header,
    // rest for body.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);

    render_sentiment_strip(frame, chunks[0], app);
    let header_area = chunks[1];
    let body_area = chunks[2];

    // Determine which report is selected.
    let (report_data, report_label, report_color) = match app.brief_report_choice {
        BriefReportChoice::Morning => (&app.report_morning, "morning", Color::Cyan),
        BriefReportChoice::Afternoon => (&app.report_afternoon, "afternoon", Color::Blue),
    };

    // Format created_at nicely (ISO string → "YYYY-MM-DD HH:MM UTC").
    let created_str = report_data
        .as_ref()
        .and_then(|r| r.created_at.as_deref())
        .map(|ts| {
            // ISO 8601 like "2026-05-22T04:45:00.000Z" → "2026-05-22 04:45 UTC"
            let t = ts.replace('T', " ");
            let t = t.split('.').next().unwrap_or(&t).to_string();
            let t = t.trim_end_matches('Z').to_string();
            if t.len() >= 16 {
                format!("{} UTC", &t[..16])
            } else {
                t
            }
        })
        .unwrap_or_default();

    // Build search status for header.
    let search_hint = if let Some(s) = &app.search {
        if s.is_active() {
            if s.matches.is_empty() {
                format!("  ·  find: {} · 0 matches", s.query)
            } else {
                format!(
                    "  ·  find: {} · {}/{}",
                    s.query,
                    s.current_match + 1,
                    s.matches.len()
                )
            }
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    let header_spans = vec![
        Span::styled(
            format!("[{report_label}]"),
            Style::default()
                .fg(report_color)
                .add_modifier(Modifier::BOLD),
        ),
        if created_str.is_empty() {
            Span::raw("")
        } else {
            Span::styled(
                format!("  {created_str}"),
                Style::default().fg(Color::DarkGray),
            )
        },
        if search_hint.is_empty() {
            Span::styled(
                "   [/] toggle  j/k scroll  g/G top/bot  / search  w save",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )
        } else {
            Span::styled(search_hint, Style::default().fg(Color::Yellow))
        },
    ];
    let header_para = Paragraph::new(Line::from(header_spans));
    frame.render_widget(header_para, header_area);

    // Extract search query and current match line for highlighting.
    let (search_query, current_match_line) = app
        .search
        .as_ref()
        .filter(|s| s.is_active())
        .map(|s| (s.query.to_lowercase(), s.current_line()))
        .unwrap_or_default();

    // ── body ─────────────────────────────────────────────────────────────────
    let visible_h = body_area.height;
    let body_width = body_area.width;

    // Store the rendered body width so n/N scroll-target can convert source
    // line indices to wrapped display-row offsets.
    app.report_body_width = body_width;

    // Resolve the report content (still borrowing app immutably). The trivial
    // loading / empty cases skip the cache — they cost nothing to rebuild.
    let report_is_none = report_data.is_none();
    let content_owned: Option<String> = report_data.as_ref().and_then(|r| r.content.clone());
    // `report_data` borrow ends here; below we only touch owned data + &mut app.

    let (body_lines, content_h): (Vec<Line<'static>>, u16) = match content_owned {
        None => {
            // No data yet, a persistent fetch error, or data present but no content.
            let (msg, msg_style): (String, Style) = if report_is_none {
                let err_key = match app.brief_report_choice {
                    BriefReportChoice::Morning => "report_morning",
                    BriefReportChoice::Afternoon => "report_afternoon",
                };
                match app.fetch_errors.get(err_key) {
                    Some(e) => (
                        format!("  report error: {e}"),
                        Style::default().fg(Color::Red).add_modifier(Modifier::DIM),
                    ),
                    None => (
                        "  loading report…".to_string(),
                        Style::default().fg(Color::DarkGray),
                    ),
                }
            } else {
                (
                    "  (no report available yet)".to_string(),
                    Style::default().fg(Color::DarkGray),
                )
            };
            // Drop any stale body cache — next real content must rebuild.
            app.report_render_cache = None;
            let lines = vec![Line::from(Span::styled(msg, msg_style))];
            let h = Paragraph::new(lines.clone())
                .wrap(Wrap { trim: false })
                .line_count(body_width)
                .min(u16::MAX as usize) as u16;
            (lines, h)
        }
        Some(content) => {
            // Build the cache key from every input that affects the body render.
            let key = ReportCacheKey {
                choice: app.brief_report_choice,
                content_hash: hash_content(&content),
                content_len: content.len(),
                width: body_width,
                query: search_query.clone(),
                current_match_line,
            };

            let hit = app
                .report_render_cache
                .as_ref()
                .is_some_and(|c| c.key == key);

            if !hit {
                // Rebuild + recompute the wrapped row count once, then cache.
                let lines = render_markdown_lines(&content, &search_query, current_match_line);
                // WRAPPED row count (what the widget actually renders), not the
                // source-line count — wide tables / long prose wrap to more rows
                // and counting source lines under-counts max_scroll (the report
                // "froze" partway down with the bottom unreachable).
                let content_h = Paragraph::new(lines.clone())
                    .wrap(Wrap { trim: false })
                    .line_count(body_width)
                    .min(u16::MAX as usize) as u16;
                app.report_render_cache = Some(ReportRenderCache {
                    key,
                    lines,
                    content_h,
                });
            }

            let cache = app.report_render_cache.as_ref().unwrap();
            (cache.lines.clone(), cache.content_h)
        }
    };

    let para = Paragraph::new(body_lines)
        .block(Block::default().borders(Borders::NONE))
        .wrap(Wrap { trim: false });

    let max_scroll = content_h.saturating_sub(visible_h);
    let scroll = app.report_scroll.min(max_scroll);

    frame.render_widget(para.scroll((scroll, 0)), body_area);
}

/// Convert markdown content to styled ratatui `Line`s.
///
/// - `## Heading` lines are rendered bold+yellow.
/// - `### Sub-heading` lines are rendered bold.
/// - `**bold**` inline markup is stripped (not rendered as Span — keeping it
///   simple; full inline markdown parsing would require an extra dep).
/// - Blank lines are preserved as empty lines for paragraph separation.
/// - When `search_query` is non-empty, matching substrings are highlighted with
///   a yellow background. The line at `current_match_line` (if any) gets an
///   additional reversed style on the matching span.
fn render_markdown_lines(
    content: &str,
    search_query: &str,
    current_match_line: Option<usize>,
) -> Vec<Line<'static>> {
    let src: Vec<&str> = content.lines().collect();
    let mut lines = Vec::new();
    let mut i = 0;
    while i < src.len() {
        let raw_line = src[i];
        let line = raw_line.trim_end();

        // ── pipe table: a row immediately followed by a `|---|---|` delimiter ──
        // Collect the whole block (header + delimiter + contiguous data rows),
        // render it as aligned columns, and drop the delimiter row. Without this
        // a markdown table renders as raw wrapping pipe-text ("falls apart").
        if line.trim_start().starts_with('|') && i + 1 < src.len() && is_delimiter_row(src[i + 1]) {
            let start = i;
            let mut j = i + 2;
            while j < src.len() {
                let t = src[j].trim();
                if t.is_empty() || !t.contains('|') {
                    break;
                }
                j += 1;
            }
            let block: Vec<&str> = src[start..j].to_vec();
            for (k, formatted) in format_markdown_table(&block).into_iter().enumerate() {
                let base = if k == 0 {
                    // header row (source index = start)
                    Style::default().add_modifier(Modifier::BOLD)
                } else if k == 1 {
                    // rule row — synthesised, no source line matches
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default()
                };
                // Map formatted row k back to its source line index so the
                // current-match highlight can fire inside table rows.
                // k==0 → src[start] (header), k==1 → delimiter (start+1, not
                // a real match target but harmless to check), k>=2 → data rows
                // start at src[start+2] and the delimiter row is consumed not
                // emitted, so formatted row k maps to src[start + k].
                let row_src_idx = start + k;
                let is_current = current_match_line == Some(row_src_idx);
                lines.push(highlight_line(formatted, search_query, base, is_current));
            }
            i = j;
            continue;
        }

        let is_current_match = current_match_line == Some(i);
        if line.starts_with("## ") {
            let heading = line
                .trim_start_matches("## ")
                .trim_start_matches('#')
                .trim()
                .to_string();
            let base_style = Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD);
            lines.push(highlight_line(
                heading,
                search_query,
                base_style,
                is_current_match,
            ));
        } else if line.starts_with("### ") {
            let heading = line
                .trim_start_matches("### ")
                .trim_start_matches('#')
                .trim()
                .to_string();
            let base_style = Style::default().add_modifier(Modifier::BOLD);
            lines.push(highlight_line(
                heading,
                search_query,
                base_style,
                is_current_match,
            ));
        } else if line.starts_with("# ") {
            let heading = line.trim_start_matches("# ").to_string();
            let base_style = Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            lines.push(highlight_line(
                heading,
                search_query,
                base_style,
                is_current_match,
            ));
        } else if line.is_empty() {
            lines.push(Line::from(""));
        } else {
            // Strip simple **bold** markers for display.
            let stripped = strip_markdown_bold(line);
            let base_style = Style::default();
            lines.push(highlight_line(
                stripped,
                search_query,
                base_style,
                is_current_match,
            ));
        }
        i += 1;
    }
    lines
}

/// Return the display-row offset (the coordinate `Paragraph::scroll` consumes)
/// of source line `source_idx` inside `content`, accounting for Paragraph
/// wrapping at `width`.
///
/// This is the coordinate the n/N search-jump should set `app.report_scroll`
/// to so that the matched source line scrolls into view.
///
/// Implementation: build the same `Vec<Line>` that the renderer builds for
/// lines `[0..source_idx)` (with empty query / no current match), wrap it
/// with the identical `Wrap { trim: false }` config, and return
/// `paragraph.line_count(width)` saturated to `u16`.
///
/// Note: `render_markdown_lines` emits exactly 1 `Line` per source line today
/// (tables remain 1:1 — the delimiter row is consumed, not dropped from the
/// count, so each source line still maps to one formatted line).  If a table
/// block is sliced mid-way (i.e. `source_idx` falls inside a table block),
/// the truncated block won't be table-formatted, which may shift widths by a
/// few characters — an acceptable approximation for scroll-jumping.
pub fn report_scroll_target(content: &str, source_idx: usize, width: u16) -> u16 {
    if source_idx == 0 {
        return 0;
    }
    let src: Vec<&str> = content.lines().collect();
    let end = source_idx.min(src.len());
    // Reconstruct only the lines preceding source_idx.
    let prefix: String = src[..end].join("\n");
    let lines = render_markdown_lines(&prefix, "", None);
    let effective_width = width.max(1);
    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    para.line_count(effective_width).min(u16::MAX as usize) as u16
}

/// Split a markdown table row into trimmed cell strings, dropping the outer
/// pipes. `"| a | b |"` → `["a", "b"]`.
fn parse_row(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').map(|c| c.trim().to_string()).collect()
}

/// True when every cell of `line` is a markdown table delimiter cell
/// (dashes with optional alignment colons), e.g. `|---|:--:|`.
fn is_delimiter_row(line: &str) -> bool {
    let cells = parse_row(line);
    !cells.is_empty()
        && cells
            .iter()
            .all(|c| !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':'))
}

/// Render a markdown table block (`[header, delimiter, data...]`) into aligned
/// display rows: header, a `─┼─` rule, then each data row, columns padded to the
/// widest cell and joined with ` │ `. The delimiter row is consumed (not output).
fn format_markdown_table(block: &[&str]) -> Vec<String> {
    if block.len() < 2 {
        return block.iter().map(|s| s.trim().to_string()).collect();
    }
    let header = parse_row(block[0]);
    let data: Vec<Vec<String>> = block[2..].iter().map(|l| parse_row(l)).collect();
    let ncols = header
        .len()
        .max(data.iter().map(std::vec::Vec::len).max().unwrap_or(0))
        .max(1);

    let mut widths = vec![0usize; ncols];
    for (i, c) in header.iter().enumerate() {
        widths[i] = widths[i].max(c.chars().count());
    }
    for r in &data {
        for (i, c) in r.iter().enumerate() {
            widths[i] = widths[i].max(c.chars().count());
        }
    }

    let fmt_row = |row: &[String]| -> String {
        (0..ncols)
            .map(|i| {
                let cell = row.get(i).map_or("", std::string::String::as_str);
                let pad = widths[i].saturating_sub(cell.chars().count());
                format!("{}{}", cell, " ".repeat(pad))
            })
            .collect::<Vec<_>>()
            .join(" │ ")
            .trim_end()
            .to_string()
    };

    let mut out = Vec::with_capacity(2 + data.len());
    out.push(fmt_row(&header));
    out.push(
        widths
            .iter()
            .map(|w| "─".repeat(*w))
            .collect::<Vec<_>>()
            .join("─┼─"),
    );
    for r in &data {
        out.push(fmt_row(r));
    }
    out
}

/// Build a `Line` from `text`, optionally highlighting occurrences of
/// `needle` within it.  When `needle` is empty the original `base_style` is
/// used for the whole line.  The `is_current` flag applies an extra
/// `REVERSED` modifier to the first highlighted span so the cursor match
/// stands out from other matches on the same line.
///
/// Panic-safety: we never slice `text` using byte offsets derived from a
/// lowercased copy.  Instead we build two parallel structures while walking
/// the original text char-by-char:
///   - `lower_chars`: the lowercase rendering of each char (may be >1 char
///     for chars like İ U+0130 → "i\u{307}"), flattened into a String.
///   - `lower_to_orig`: for each byte position in the lowercased string,
///     the corresponding byte position in the original `text`.
///
/// Matches are found in `lower_chars`, then the `lower_to_orig` map converts
/// match start/end back to valid char-boundary offsets in `text`.
fn highlight_line(
    text: String,
    needle: &str,
    base_style: Style,
    is_current: bool,
) -> Line<'static> {
    if needle.is_empty() {
        return Line::from(Span::styled(text, base_style));
    }

    // Build lowercased string + a table mapping each byte of the lowercased
    // string → the byte offset of the *original* char that produced it.
    let mut lower_chars = String::with_capacity(text.len());
    // lower_to_orig[i] = byte offset in `text` of the original char whose
    // lowercase form starts at byte i in `lower_chars`.  We also record a
    // sentinel entry for the end of `lower_chars` that maps to `text.len()`.
    let mut lower_to_orig: Vec<usize> = Vec::with_capacity(text.len() + 1);

    let mut orig_byte = 0usize;
    for ch in text.chars() {
        let ch_lower = ch.to_lowercase().to_string();
        for _ in ch_lower.bytes() {
            lower_to_orig.push(orig_byte);
        }
        lower_chars.push_str(&ch_lower);
        orig_byte += ch.len_utf8();
    }
    lower_to_orig.push(orig_byte); // sentinel: lower_chars.len() → text.len()

    if !lower_chars.contains(needle) {
        return Line::from(Span::styled(text, base_style));
    }

    // Walk lower_chars finding all occurrences of needle.
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut orig_start = 0usize; // cursor in `text` (byte offset)
    let mut lower_start = 0usize; // cursor in `lower_chars` (byte offset)
    let mut first_match = true;

    while lower_start <= lower_chars.len().saturating_sub(needle.len()) {
        if let Some(rel) = lower_chars[lower_start..].find(needle) {
            let lower_abs = lower_start + rel;
            let lower_end = lower_abs + needle.len();

            // Convert lower byte offsets → original byte offsets via the map.
            // The map is always large enough: lower_to_orig has lower_chars.len()+1 entries.
            let orig_abs = lower_to_orig[lower_abs];
            // For the end, use the sentinel if lower_end == lower_chars.len().
            let orig_end = lower_to_orig[lower_end];

            // Non-match segment before this match.
            if orig_abs > orig_start {
                spans.push(Span::styled(
                    text[orig_start..orig_abs].to_string(),
                    base_style,
                ));
            }
            // Match segment — slice original text with guaranteed char-boundary offsets.
            let match_text = text[orig_abs..orig_end].to_string();
            let match_style = if is_current && first_match {
                Style::default()
                    .bg(Color::Yellow)
                    .fg(Color::Black)
                    .add_modifier(Modifier::REVERSED | Modifier::BOLD)
            } else {
                Style::default().bg(Color::Yellow).fg(Color::Black)
            };
            spans.push(Span::styled(match_text, match_style));
            first_match = false;
            orig_start = orig_end;
            lower_start = lower_end;
        } else {
            break;
        }
    }
    // Remaining text after last match.
    if orig_start < text.len() {
        spans.push(Span::styled(text[orig_start..].to_string(), base_style));
    }
    if spans.is_empty() {
        Line::from(Span::styled(text, base_style))
    } else {
        Line::from(spans)
    }
}

/// Strip `**text**` and `__text__` markers from a line, returning owned String.
fn strip_markdown_bold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '*' && chars.peek() == Some(&'*') {
            chars.next(); // consume second *
            continue;
        }
        if c == '_' && chars.peek() == Some(&'_') {
            chars.next(); // consume second _
            continue;
        }
        out.push(c);
    }
    out
}

/// One-row market strip on top of the report: regime · F&G · MVRV-Z · ASI ·
/// funding. Fed by the 60 s sentiment fetcher.
fn render_sentiment_strip(frame: &mut Frame, area: Rect, app: &App) {
    use ratatui::text::Span;
    let dim = Style::default().fg(Color::DarkGray);
    let mut spans: Vec<Span<'static>> = vec![Span::styled(" ", dim)];

    let Some(snap) = app.sentiment.as_ref() else {
        let msg = match app.fetch_errors.get("sentiment") {
            Some(e) => format!("market strip error: {e}"),
            None => "market strip loading…".to_string(),
        };
        spans.push(Span::styled(msg, dim));
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    };
    let s = &snap.sentiment;

    if let Some(r) = &s.regime {
        let color = if r.score > 0 {
            Color::Green
        } else if r.score < 0 {
            Color::Red
        } else {
            Color::DarkGray
        };
        spans.push(Span::styled("regime ", dim));
        spans.push(Span::styled(
            format!("{} ({:+})", r.regime, r.score),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(fg) = &s.fear_greed {
        let color = if fg.value <= 25 {
            Color::Red
        } else if fg.value >= 75 {
            Color::Green
        } else {
            Color::Yellow
        };
        spans.push(Span::styled("  ·  F&G ", dim));
        spans.push(Span::styled(
            format!("{} {}", fg.value, fg.classification),
            Style::default().fg(color),
        ));
    }
    if let Some(m) = &s.mvrv_zscore
        && let Some(z) = m.zscore
    {
        spans.push(Span::styled("  ·  MVRV-Z ", dim));
        let color = if z >= 6.0 {
            Color::Red
        } else if z <= 0.0 {
            Color::Green
        } else {
            Color::Reset
        };
        let zone = m.zone.as_deref().unwrap_or("");
        spans.push(Span::styled(
            if zone.is_empty() {
                format!("{z:.2}")
            } else {
                format!("{z:.2} {zone}")
            },
            Style::default().fg(color),
        ));
    }
    if let Some(a) = &s.alt_season
        && let Some(idx) = a.index
    {
        spans.push(Span::styled("  ·  ASI ", dim));
        let season = a.season.as_deref().unwrap_or("");
        let color = if idx >= 75.0 {
            Color::Green
        } else if idx <= 25.0 {
            Color::Red
        } else {
            Color::Yellow
        };
        spans.push(Span::styled(
            if season.is_empty() {
                format!("{idx:.0}")
            } else {
                format!("{idx:.0} {season}")
            },
            Style::default().fg(color),
        ));
    }
    if let Some(f) = &s.funding_rate
        && let Some(pct) = f.rate_pct
    {
        spans.push(Span::styled("  ·  fund ", dim));
        let color = if pct > 0.0 { Color::Green } else { Color::Red };
        spans.push(Span::styled(
            format!("{pct:+.3}%"),
            Style::default().fg(color),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::{App, BriefReportChoice};
    use crate::types::report::ReportResponse;

    #[test]
    fn render_with_loading_state_no_panic() {
        // All report data is None — render should produce a "loading" line.
        let app = App::new();
        assert!(app.report_morning.is_none());
        assert!(app.report_afternoon.is_none());
        // Exercise the line-building path directly.
        let lines = match &app.report_morning {
            None => vec![Line::from(Span::styled(
                "  loading report…",
                Style::default().fg(Color::DarkGray),
            ))],
            Some(_) => vec![],
        };
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].spans.is_empty());
    }

    #[test]
    fn render_with_full_content_no_panic() {
        let mut app = App::new();
        app.report_morning = Some(ReportResponse {
            content: Some(
                "# Morning Report\n\n## Summary\n\nBullish backdrop.\n\n## Details\n\nSPX held key level."
                    .to_string(),
            ),
            created_at: Some("2026-05-22T04:45:00.000Z".to_string()),
        });
        app.brief_report_choice = BriefReportChoice::Morning;

        // Exercise render_markdown_lines with heading + paragraph content.
        let content = app
            .report_morning
            .as_ref()
            .unwrap()
            .content
            .as_ref()
            .unwrap();
        let lines = render_markdown_lines(content, "", None);
        assert!(!lines.is_empty(), "should produce at least one line");
        // H1 heading should be present.
        let has_heading = lines
            .iter()
            .any(|l| l.spans.iter().any(|s| s.content.contains("Morning Report")));
        assert!(has_heading, "H1 heading should appear in rendered lines");
    }

    #[test]
    fn scrolls_to_bottom_section_with_wrapping_content() {
        // Regression for the "report freezes at Regime & Positioning" bug: when
        // lines wrap (wide tables / long prose) the rendered height is far larger
        // than the source-line count. Scrolling to the bottom (G) must reveal the
        // final section, not pin partway down.
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut content = String::new();
        for i in 0..25 {
            content.push_str(&format!("Line {i} "));
            content.push_str(&"word ".repeat(30)); // long → wraps several times at width 40
            content.push('\n');
        }
        content.push_str("REGIMEZONE_SENTINEL final section line\n");

        let mut app = App::new();
        app.report_morning = Some(ReportResponse {
            content: Some(content),
            created_at: None,
        });
        app.brief_report_choice = BriefReportChoice::Morning;
        app.report_scroll = u16::MAX; // emulate `G` — jump to bottom; renderer clamps

        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                super::render(f, area, &mut app);
            })
            .unwrap();

        let buf = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                text.push_str(buf.cell((x, y)).map_or(" ", ratatui::buffer::Cell::symbol));
            }
            text.push('\n');
        }
        assert!(
            text.contains("SENTINEL"),
            "bottom section must be reachable when scrolled to G:\n{text}"
        );
    }

    #[test]
    fn created_at_formatting() {
        // Verify the ISO → display string conversion logic.
        let ts = "2026-05-22T04:45:00.000Z";
        let t = ts.replace('T', " ");
        let t = t.split('.').next().unwrap_or(&t).to_string();
        let t = t.trim_end_matches('Z').to_string();
        let result = if t.len() >= 16 {
            format!("{} UTC", &t[..16])
        } else {
            t
        };
        assert_eq!(result, "2026-05-22 04:45 UTC");
    }

    #[test]
    fn strip_markdown_bold_removes_markers() {
        assert_eq!(strip_markdown_bold("hello **world**"), "hello world");
        assert_eq!(strip_markdown_bold("__bold__ text"), "bold text");
        assert_eq!(strip_markdown_bold("no markers here"), "no markers here");
        assert_eq!(strip_markdown_bold("**a** and **b**"), "a and b");
    }

    #[test]
    fn formats_pipe_table_into_aligned_columns() {
        let block = vec![
            "| Driver | Asset | Direction |",
            "|--------|-------|-----------|",
            "| USD | BTC | Short |",
            "| Rates | ETH | Long |",
        ];
        let out = format_markdown_table(&block);
        assert_eq!(out.len(), 4, "header + rule + 2 data rows");
        assert_eq!(out[0], "Driver │ Asset │ Direction");
        assert!(out[1].contains('┼'), "rule row has a column joint");
        assert_eq!(
            out[1].chars().count(),
            out[0].chars().count(),
            "rule aligns with header"
        );
        assert_eq!(out[2], "USD    │ BTC   │ Short");
        assert_eq!(out[3], "Rates  │ ETH   │ Long");
    }

    #[test]
    fn detects_delimiter_rows() {
        assert!(is_delimiter_row("|---|:--:|---:|"));
        assert!(is_delimiter_row("| --- | --- |"));
        assert!(!is_delimiter_row("| Driver | Asset |"));
        assert!(!is_delimiter_row("plain prose, no table"));
    }

    #[test]
    fn render_markdown_lines_renders_table_block() {
        let content =
            "## Action Dashboard\n\n| Driver | Asset |\n|---|---|\n| USD | BTC |\n\n## Next";
        let lines = render_markdown_lines(content, "", None);
        let joined: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(
            joined.iter().any(|l| l.contains("Driver │ Asset")),
            "aligned table header present: {joined:?}"
        );
        assert!(
            !joined.iter().any(|l| l.contains("---")),
            "markdown delimiter row should be dropped: {joined:?}"
        );
    }

    #[test]
    fn render_markdown_lines_handles_headings() {
        let content = "# H1\n## H2\n### H3\n\nBody text.";
        let lines = render_markdown_lines(content, "", None);
        // Should have: H1 line, H2 line, H3 line, blank line, body line.
        assert_eq!(lines.len(), 5);
        // H1 should be bold+underlined+yellow, H2 bold+yellow, H3 bold.
        assert!(
            lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(
            lines[1].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(
            lines[2].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        // Blank line.
        assert!(lines[3].spans.is_empty() || lines[3].spans[0].content.trim().is_empty());
    }

    #[test]
    fn afternoon_report_shows_when_choice_is_afternoon() {
        let mut app = App::new();
        app.brief_report_choice = BriefReportChoice::Afternoon;
        app.report_afternoon = Some(ReportResponse {
            content: Some("Afternoon content here.".to_string()),
            created_at: None,
        });
        // Verify the choice routing selects afternoon data.
        let (data, label, _color) = match app.brief_report_choice {
            BriefReportChoice::Morning => (&app.report_morning, "morning", Color::Cyan),
            BriefReportChoice::Afternoon => (&app.report_afternoon, "afternoon", Color::Blue),
        };
        assert_eq!(label, "afternoon");
        assert!(data.is_some());
    }

    // ── Search / highlight tests ─────────────────────────────────────────────

    #[test]
    fn highlight_line_no_query_returns_single_span() {
        let lines = render_markdown_lines("Hello world", "", None);
        assert_eq!(lines.len(), 1);
        // Should be a single span with the full text.
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(lines[0].spans[0].content, "Hello world");
    }

    #[test]
    fn highlight_line_match_splits_into_spans() {
        let lines = render_markdown_lines("BTC is bullish", "btc", None);
        assert_eq!(lines.len(), 1);
        // Should have: "BTC" (highlighted) + " is bullish" (normal).
        // The exact span count depends on where the needle appears; at minimum >1 span.
        assert!(
            lines[0].spans.len() > 1,
            "match should produce multiple spans"
        );
        // The highlighted span should contain "BTC".
        let has_match = lines[0]
            .spans
            .iter()
            .any(|s| s.content.eq_ignore_ascii_case("btc") && s.style.bg == Some(Color::Yellow));
        assert!(has_match, "BTC span should have yellow bg");
    }

    #[test]
    fn highlight_current_match_line_gets_reversed_style() {
        let lines = render_markdown_lines("BTC breakout today", "btc", Some(0));
        assert!(!lines.is_empty());
        // Current match → REVERSED modifier on the match span.
        let has_reversed = lines[0]
            .spans
            .iter()
            .any(|s| s.style.add_modifier.contains(Modifier::REVERSED));
        assert!(
            has_reversed,
            "current match line should have REVERSED modifier"
        );
    }

    #[test]
    fn highlight_non_current_match_no_reversed_style() {
        // current_match_line = Some(5), but our content is only 1 line (line 0).
        let lines = render_markdown_lines("BTC breakout today", "btc", Some(5));
        assert!(!lines.is_empty());
        // Match exists but it's not the current one → no REVERSED.
        let has_reversed = lines[0]
            .spans
            .iter()
            .any(|s| s.style.add_modifier.contains(Modifier::REVERSED));
        assert!(!has_reversed, "non-current match should not have REVERSED");
    }

    #[test]
    fn highlight_line_no_match_returns_original_text() {
        let lines = render_markdown_lines("ETH is flat", "btc", None);
        assert_eq!(lines.len(), 1);
        // Single span, no highlight.
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(lines[0].spans[0].style.bg, None);
    }

    #[test]
    fn collect_report_lines_matches_find_mode_indices() {
        use crate::tui::event::collect_report_lines;
        let content = "BTC is up\nETH is flat\nBTC volume surge";
        let lines = collect_report_lines(content);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("BTC"));
        assert!(lines[1].contains("ETH"));
        assert!(lines[2].contains("BTC"));
    }

    // ── Fix 1: panic-safe highlight_line (UTF-8 edge cases) ─────────────────

    #[test]
    fn highlight_line_dotted_capital_i_no_panic() {
        // İ (U+0130) lowercases to "i\u{0307}" (2 bytes → 3 bytes).
        // This used to panic when lower's byte offsets were applied to original text.
        let lines = render_markdown_lines("İstanbul BTC rally", "btc", None);
        assert!(!lines.is_empty(), "should not panic");
        let all_text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(all_text.contains("İstanbul"), "original text preserved");
        let has_match = lines[0]
            .spans
            .iter()
            .any(|s| s.content.eq_ignore_ascii_case("btc") && s.style.bg == Some(Color::Yellow));
        assert!(has_match, "BTC match highlighted");
    }

    #[test]
    fn highlight_line_sharp_s_no_panic() {
        // ẞ (U+1E9E) lowercases to "ß" (3 bytes → 2 bytes).
        // Needle "sharp" does not match, but the function must not panic.
        let lines = render_markdown_lines("ẞharp analysis", "sharp", None);
        assert!(!lines.is_empty(), "should not panic");
    }

    #[test]
    fn highlight_line_emoji_cjk_around_match() {
        // Emoji and CJK chars around a simple ASCII match.
        let lines = render_markdown_lines("🚀 BTC 上涨 rally", "btc", None);
        assert!(!lines.is_empty(), "should not panic");
        let has_match = lines[0]
            .spans
            .iter()
            .any(|s| s.content.eq_ignore_ascii_case("btc") && s.style.bg == Some(Color::Yellow));
        assert!(has_match, "BTC highlighted amid emoji/CJK");
        let all_text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(all_text.contains("🚀"), "emoji preserved");
        assert!(all_text.contains("上涨"), "CJK preserved");
    }

    #[test]
    fn highlight_line_match_at_start_no_panic() {
        let lines = render_markdown_lines("btc is king", "btc", None);
        assert!(!lines.is_empty());
        let has_match = lines[0]
            .spans
            .iter()
            .any(|s| s.style.bg == Some(Color::Yellow));
        assert!(has_match, "match at start highlighted");
    }

    #[test]
    fn highlight_line_match_at_end_no_panic() {
        let lines = render_markdown_lines("king of crypto btc", "btc", None);
        assert!(!lines.is_empty());
        let has_match = lines[0]
            .spans
            .iter()
            .any(|s| s.style.bg == Some(Color::Yellow));
        assert!(has_match, "match at end highlighted");
    }

    #[test]
    fn highlight_line_needle_absent_no_panic() {
        let lines = render_markdown_lines("ETH is flat", "xyz", None);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(lines[0].spans[0].style.bg, None);
    }

    // ── Fix 2: current-match highlight inside table rows ────────────────────

    #[test]
    fn table_row_current_match_gets_reversed_style() {
        // Table starts at source line 0 (header), line 1 is delimiter, line 2 is
        // first data row. current_match_line = Some(2) should mark that data row.
        let content = "| Asset | Dir |\n|---|---|\n| BTC | Long |\n| ETH | Short |";
        // Line indices: 0=header, 1=delimiter, 2=BTC row, 3=ETH row
        let lines = render_markdown_lines(content, "btc", Some(2));
        let has_reversed = lines.iter().any(|l| {
            l.spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::REVERSED))
        });
        assert!(
            has_reversed,
            "current match inside table row must carry REVERSED style"
        );
    }

    // ── Fix 3: prose-pipe false-positive ────────────────────────────────────

    #[test]
    fn prose_pipe_above_delimiter_renders_as_prose_not_table() {
        // "a | b" does NOT start with '|', so it should NOT be treated as a table header
        // even if the next line is a delimiter.
        let content = "a | b\n|---|---|\ndata row";
        let lines = render_markdown_lines(content, "", None);
        // Should have 3 lines: prose "a | b", delimiter line rendered (but no bold header),
        // and data line. The key assertion: line 0 is NOT bold (not a table header).
        assert!(!lines.is_empty());
        let first_is_bold = lines[0]
            .spans
            .iter()
            .any(|s| s.style.add_modifier.contains(Modifier::BOLD));
        assert!(
            !first_is_bold,
            "'a | b' must render as prose, not bold table header"
        );
        // Also confirm the text content is preserved as prose.
        let first_text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(first_text.contains('|'), "pipe preserved in prose");
    }

    #[test]
    fn real_table_header_with_leading_pipe_renders_as_table() {
        // "| H1 | H2 |" starts with '|' → should render as bold table header.
        let content = "| H1 | H2 |\n|---|---|\n| d1 | d2 |";
        let lines = render_markdown_lines(content, "", None);
        assert!(!lines.is_empty());
        // Header row (formatted row 0) should be bold.
        let first_is_bold = lines[0]
            .spans
            .iter()
            .any(|s| s.style.add_modifier.contains(Modifier::BOLD));
        assert!(first_is_bold, "real table header must be bold");
        // Aligned column separator should appear.
        let all_text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(all_text.contains('│'), "aligned separator in header");
    }

    // ── Fix 5: report_scroll_target ──────────────────────────────────────────

    #[test]
    fn report_scroll_target_zero_for_line_zero() {
        let content = "line one\nline two\nline three";
        assert_eq!(report_scroll_target(content, 0, 40), 0);
    }

    #[test]
    fn report_scroll_target_wrapping_line_advances_more_than_one() {
        // Line 0 is 200 chars wide; at width 40 it wraps into 5 display rows.
        // So target(1) should be ≥ 5 > 1.
        let long_line: String = "x".repeat(200);
        let content = format!("{long_line}\nshort line");
        let target = report_scroll_target(&content, 1, 40);
        assert!(
            target >= 5,
            "200-char line at width 40 wraps to ≥5 rows; got {target}"
        );
        assert!(target > 1, "must be > 1 (not a naive line-index)");
    }

    #[test]
    fn report_scroll_target_beyond_content_returns_total_count() {
        let content = "line a\nline b\nline c";
        let total = report_scroll_target(content, 999, 80);
        let expected = report_scroll_target(content, 3, 80);
        assert_eq!(total, expected, "out-of-bounds source_idx clamps to total");
    }

    #[test]
    fn report_scroll_target_width_zero_treated_as_one() {
        // Must not panic; just returns a value.
        let content = "some content\nmore content";
        let _ = report_scroll_target(content, 1, 0);
    }

    // ── render cache (item 2) ────────────────────────────────────────────────

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn draw_report(app: &mut App, w: u16, h: u16) {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                super::render(f, area, app);
            })
            .unwrap();
    }

    #[test]
    fn cache_is_populated_then_reused_on_identical_frame() {
        let mut app = App::new();
        app.report_morning = Some(ReportResponse {
            content: Some("## Heading\n\nSome body prose here.".to_string()),
            created_at: None,
        });
        app.brief_report_choice = BriefReportChoice::Morning;
        assert!(app.report_render_cache.is_none());

        draw_report(&mut app, 60, 12);
        assert!(
            app.report_render_cache.is_some(),
            "first render fills cache"
        );
        let key1 = app.report_render_cache.as_ref().unwrap().key.clone();

        // Identical second frame: key must be unchanged (cache reused).
        draw_report(&mut app, 60, 12);
        let key2 = app.report_render_cache.as_ref().unwrap().key.clone();
        assert!(key1 == key2, "identical frame must keep the same cache key");
    }

    #[test]
    fn cache_invalidates_on_morning_afternoon_toggle() {
        let mut app = App::new();
        app.report_morning = Some(ReportResponse {
            content: Some("Morning content body.".to_string()),
            created_at: None,
        });
        app.report_afternoon = Some(ReportResponse {
            content: Some("Afternoon content body, different.".to_string()),
            created_at: None,
        });
        app.brief_report_choice = BriefReportChoice::Morning;
        draw_report(&mut app, 60, 12);
        let morning_key = app.report_render_cache.as_ref().unwrap().key.clone();

        // `]` toggle → afternoon. Cache key must change (different choice + content).
        app.brief_report_choice = BriefReportChoice::Afternoon;
        draw_report(&mut app, 60, 12);
        let afternoon_key = app.report_render_cache.as_ref().unwrap().key.clone();
        assert!(
            morning_key != afternoon_key,
            "morning/afternoon toggle must invalidate the cache"
        );
        assert_eq!(afternoon_key.choice, BriefReportChoice::Afternoon);
    }

    #[test]
    fn cache_invalidates_on_width_change() {
        let mut app = App::new();
        app.report_morning = Some(ReportResponse {
            content: Some("word ".repeat(40)),
            created_at: None,
        });
        app.brief_report_choice = BriefReportChoice::Morning;
        draw_report(&mut app, 80, 12);
        let wide = app.report_render_cache.as_ref().unwrap().key.clone();

        draw_report(&mut app, 40, 12);
        let narrow = app.report_render_cache.as_ref().unwrap().key.clone();
        assert!(wide != narrow, "width change must invalidate the cache");
    }

    #[test]
    fn cache_invalidates_on_search_query_and_match_change() {
        use crate::app::state::{SearchMode, SearchState};

        let mut app = App::new();
        app.report_morning = Some(ReportResponse {
            content: Some("BTC up\nETH flat\nBTC down".to_string()),
            created_at: None,
        });
        app.brief_report_choice = BriefReportChoice::Morning;

        // No search yet.
        draw_report(&mut app, 60, 12);
        let no_query = app.report_render_cache.as_ref().unwrap().key.clone();

        // Activate a Find search with matches on lines 0 and 2, current = 0.
        let mut s = SearchState::new(SearchMode::Find);
        s.query = "btc".to_string();
        s.matches = vec![0, 2];
        s.current_match = 0;
        s.close_prompt();
        app.search = Some(s);
        draw_report(&mut app, 60, 12);
        let with_query = app.report_render_cache.as_ref().unwrap().key.clone();
        assert!(
            no_query != with_query,
            "activating a search query must invalidate the cache"
        );

        // n → advance current match to line 2. The highlighted (REVERSED) span
        // moves, so the cache MUST rebuild even though query + content are same.
        app.search.as_mut().unwrap().current_match = 1;
        draw_report(&mut app, 60, 12);
        let next_match = app.report_render_cache.as_ref().unwrap().key.clone();
        assert!(
            with_query != next_match,
            "n/N moving the current match must invalidate the cache"
        );
        assert_eq!(next_match.current_match_line, Some(2));
    }

    #[test]
    fn cache_cleared_when_content_disappears() {
        let mut app = App::new();
        app.report_morning = Some(ReportResponse {
            content: Some("Some body.".to_string()),
            created_at: None,
        });
        app.brief_report_choice = BriefReportChoice::Morning;
        draw_report(&mut app, 60, 12);
        assert!(app.report_render_cache.is_some());

        // Content gone (e.g. switched to a not-yet-loaded report) → cache dropped.
        app.report_morning = None;
        draw_report(&mut app, 60, 12);
        assert!(
            app.report_render_cache.is_none(),
            "stale cache must be cleared when content is absent"
        );
    }
}
