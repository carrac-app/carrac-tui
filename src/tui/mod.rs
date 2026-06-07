pub mod event;
pub mod layout;
pub mod widgets;

use anyhow::Result;
use crossterm::{
    ExecutableCommand,
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::Stdout;

/// Alias for the backend we use throughout.
pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Enter alternate screen, enable raw mode, install panic hook that restores
/// the terminal before printing the panic message.
pub fn enter() -> Result<Tui> {
    // Panic hook: always restore terminal before unwinding.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // best-effort restore; ignore errors
        restore_terminal_raw();
        original_hook(info);
    }));

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

/// Restore the terminal to its original state.
pub fn exit(mut terminal: Tui) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// Raw restore (no Tui handle) — used from panic hook.
fn restore_terminal_raw() {
    let _ = disable_raw_mode();
    let _ = std::io::stdout().execute(LeaveAlternateScreen);
    let _ = std::io::stdout().execute(DisableMouseCapture);
}

// ─── format::Style → ratatui::style::Style bridge ───────────────────────────

use ratatui::style::{Color as RColor, Modifier as RModifier, Style as RStyle};

use crate::format::{Color, Modifier, Style};

impl From<Style> for RStyle {
    fn from(s: Style) -> RStyle {
        let mut rs = RStyle::default();
        if let Some(fg) = s.fg {
            rs = rs.fg(match fg {
                Color::Green => RColor::Green,
                Color::Red => RColor::Red,
                Color::Yellow => RColor::Yellow,
                Color::Dim => RColor::DarkGray,
                Color::Default => RColor::Reset,
            });
        }
        if s.modifier.contains(Modifier::BOLD) {
            rs = rs.add_modifier(RModifier::BOLD);
        }
        if s.modifier.contains(Modifier::DIM) {
            rs = rs.add_modifier(RModifier::DIM);
        }
        rs
    }
}
