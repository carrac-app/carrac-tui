//! Open a URL or file with the platform's default handler.
//!
//! One place for the per-OS opener so the news "open link" action and the PNG
//! viewer don't each hand-roll (and drift on) platform handling. All failures
//! are best-effort and silent — opening is a convenience, never load-bearing.

use std::process::{Command, Stdio};

/// Spawn `program` with `args`, detached and silenced. Returns whether the
/// spawn itself succeeded (not whether the target program did anything useful).
fn spawn_detached(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

/// Open a URL or file path with the OS default handler.
///
/// Linux: `$BROWSER` if set, else `xdg-open`. macOS: `open`. Windows:
/// `cmd /c start`. Best-effort; never panics, never blocks.
pub fn open_external(target: &str) {
    // Tests must never spawn a real handler (a stray browser tab / viewer on
    // every `cargo test`).
    if cfg!(test) {
        return;
    }

    #[cfg(target_os = "macos")]
    {
        spawn_detached("open", &[target]);
    }
    #[cfg(target_os = "windows")]
    {
        // `start` is a cmd builtin; the empty "" is the window-title argument
        // so a quoted target isn't consumed as the title.
        spawn_detached("cmd", &["/C", "start", "", target]);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Ok(browser) = std::env::var("BROWSER")
            && !browser.is_empty()
            && spawn_detached(&browser, &[target])
        {
            return;
        }
        spawn_detached("xdg-open", &[target]);
    }
}
