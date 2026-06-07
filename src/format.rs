use std::io::IsTerminal;
use std::sync::OnceLock;

use crate::types::positions::RiskStatus;

// ─── Style types (ratatui-shaped) ───────────────────────────────────────────
//
// These are a minimal internal colour/modifier representation intentionally
// mirroring `ratatui::style::Style`, so CLI rendering stays decoupled from the
// TUI toolkit. Keep this module free of a ratatui dependency.

/// Named terminal foreground colours understood by the style API.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Color {
    #[default]
    Default,
    Green,
    Red,
    Yellow,
    Dim,
}

/// Text modifiers, stored as a plain bitmask (no bitflags dep).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifier(pub u8);

impl Modifier {
    pub const NONE: Modifier = Modifier(0);
    pub const BOLD: Modifier = Modifier(0b01);
    pub const DIM: Modifier = Modifier(0b10);

    pub fn contains(self, other: Modifier) -> bool {
        self.0 & other.0 != 0
    }
}

impl std::ops::BitOr for Modifier {
    type Output = Modifier;
    fn bitor(self, rhs: Modifier) -> Modifier {
        Modifier(self.0 | rhs.0)
    }
}

/// Lightweight style descriptor, ratatui-shaped by design.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: Option<Color>,
    pub modifier: Modifier,
}

impl Style {
    pub const fn new() -> Style {
        Style {
            fg: None,
            modifier: Modifier::NONE,
        }
    }
    pub const fn fg(mut self, color: Color) -> Style {
        self.fg = Some(color);
        self
    }
    pub const fn modifier(mut self, m: Modifier) -> Style {
        self.modifier = m;
        self
    }
}

static COLOR_ENABLED: OnceLock<bool> = OnceLock::new();

pub fn init_color() {
    let _ = COLOR_ENABLED.get_or_init(|| {
        if std::env::var_os("NO_COLOR").is_some() {
            return false;
        }
        std::io::stdout().is_terminal()
    });
}

/// Force-enable ANSI color regardless of whether stdout is a TTY.
///
/// Used by the hidden `render` subcommand: the server pipes our stdout into a
/// curl response, so `is_terminal()` returns false even though the downstream
/// client wants ANSI.  Must be called before any `color_on()` invocation (i.e.,
/// before `render_oneshot`), and only when `--plain` is NOT set.
pub fn force_enable_color() {
    let _ = COLOR_ENABLED.set(true);
}

fn color_on() -> bool {
    *COLOR_ENABLED
        .get_or_init(|| std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal())
}

/// Public re-export of the color check for modules that need to build ANSI codes manually.
pub fn color_on_pub() -> bool {
    color_on()
}

fn paint(code: &str, s: &str) -> String {
    if color_on() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn ok(s: &str) -> String {
    paint("32", s)
} // green
pub fn alert(s: &str) -> String {
    paint("31", s)
} // red
pub fn caution(s: &str) -> String {
    paint("33", s)
} // yellow
pub fn neutral(s: &str) -> String {
    paint("39", s)
} // default fg
pub fn dim(s: &str) -> String {
    paint("2", s)
} // dim
pub fn bold(s: &str) -> String {
    paint("1", s)
}
pub fn alert_dim(s: &str) -> String {
    paint("31;2", s)
} // red + dim

/// Color a number by sign. Zero is neutral.
pub fn pnl(s: &str, value: f64) -> String {
    if value > 0.0 {
        ok(s)
    } else if value < 0.0 {
        alert(s)
    } else {
        neutral(s)
    }
}

/// Color based on RiskStatus enum value.
pub fn risk(s: &str, status: &RiskStatus) -> String {
    match status {
        RiskStatus::AtRisk => caution(s),
        RiskStatus::StoppedOut => alert_dim(s),
        _ => neutral(s),
    }
}

/// Render a row of cells aligned to the given column widths.
/// `aligns[i]` selects 'l' (left) or 'r' (right). Cells are padded with spaces.
/// Color codes inside cells are stripped only for width calc.
pub fn row(cells: &[&str], widths: &[usize], aligns: &[char]) -> String {
    let mut out = String::new();
    for (i, cell) in cells.iter().enumerate() {
        let w = widths.get(i).copied().unwrap_or(0);
        let a = aligns.get(i).copied().unwrap_or('l');
        let visible = strip_ansi_width(cell);
        let pad = w.saturating_sub(visible);
        if a == 'r' {
            out.push_str(&" ".repeat(pad));
            out.push_str(cell);
        } else {
            out.push_str(cell);
            out.push_str(&" ".repeat(pad));
        }
        if i + 1 < cells.len() {
            out.push_str("  ");
        }
    }
    out
}

/// Render a table of `cells` (first row = header) to a `String`.
///
/// `aligns[i]` is `'l'` or `'r'`.  When `dim_header` is true the first row is
/// wrapped in `dim()`.  Callers that previously accumulated into `&mut String`
/// should switch to `out.push_str(&render_table(...))`.
///
/// This is the shared table renderer used by the CLI (`carrac positions` and
/// friends): callers pass pre-coloured cells and `strip_ansi_width` handles
/// width measurement.
pub fn render_table(cells: &[Vec<String>], aligns: &[char], dim_header: bool) -> String {
    let cols = aligns.len();
    let mut widths = vec![0usize; cols];
    for row in cells {
        for (i, c) in row.iter().enumerate().take(cols) {
            widths[i] = widths[i].max(strip_ansi_width(c));
        }
    }
    let mut out = String::new();
    for (idx, table_row) in cells.iter().enumerate() {
        let refs: Vec<&str> = table_row.iter().map(|s| s.as_str()).collect();
        let line = row(&refs, &widths, aligns);
        if idx == 0 && dim_header {
            out.push_str(&dim(&line));
        } else {
            out.push_str(&line);
        }
        out.push('\n');
    }
    out
}

/// Remove all CSI/SGR escape sequences (`ESC [ … letter`).
/// Uses the correct 0x40–0x7E final-byte range (includes '@', '['..'_', etc.).
pub(crate) fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            chars.next(); // consume '['
            for c2 in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&c2) {
                    break;
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

/// Count visible columns ignoring CSI escape sequences.
/// Uses the correct 0x40–0x7E final-byte range.
pub fn strip_ansi_width(s: &str) -> usize {
    let mut count = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            while let Some(&p) = chars.peek() {
                chars.next();
                if ('\u{40}'..='\u{7e}').contains(&p) {
                    break;
                }
            }
        } else {
            count += 1;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_counts_visible() {
        let s = format!("{}{}", ok("hello"), neutral(""));
        assert_eq!(strip_ansi_width(&s), 5);
    }

    #[test]
    fn strip_ansi_removes_sgr() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m ok"), "red ok");
    }

    #[test]
    fn strip_ansi_at_terminated_csi() {
        // \x1b[1@ is a valid CSI sequence with '@' (0x40) as final byte.
        // Must be stripped correctly; the alphabetic-only test would miss this.
        assert_eq!(strip_ansi("\x1b[1@keep"), "keep");
        assert_eq!(strip_ansi_width("\x1b[1@hi"), 2);
    }

    #[test]
    fn pnl_signs() {
        // colors may be off in test env — just confirm no panic & substring present
        assert!(pnl("-1.2", -1.2).contains("-1.2"));
        assert!(pnl("+0.0", 0.0).contains("+0.0"));
    }
}
