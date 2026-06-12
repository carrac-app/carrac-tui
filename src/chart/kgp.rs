/// KGP / inline-image display pass.
///
/// Decodes a PNG byte buffer and renders it to the terminal using `viuer`.
/// `viuer` handles KGP (kitty), iTerm2, and halfblock fallback automatically.
///
/// When `--out` is given, the caller writes the file directly (bypassing this
/// module). This module is only invoked when inline display is desired.
use anyhow::{Context, Result};
use image::DynamicImage;

/// Detect whether stdout is likely a kitty-compatible terminal (KGP supported).
///
/// Heuristic: checks `TERM=xterm-kitty`, `TERM_PROGRAM=kitty`, or `KITTY_WINDOW_ID`.
/// Also accepts ghostty (`TERM_PROGRAM=ghostty` or `GHOSTTY_RESOURCES_DIR`).
/// Returns `true` if KGP is likely available, `false` if we should fall back.
pub fn likely_kgp() -> bool {
    let term = std::env::var("TERM").unwrap_or_default();
    let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    let kitty_id = std::env::var("KITTY_WINDOW_ID").is_ok();
    let ghostty = std::env::var("GHOSTTY_RESOURCES_DIR").is_ok()
        || term_program.to_lowercase().contains("ghostty");
    let is_kitty =
        term.contains("kitty") || term_program.to_lowercase().contains("kitty") || kitty_id;
    is_kitty || ghostty
}

/// Display a PNG (as raw bytes) inline in the terminal via `viuer`.
///
/// `term_cols` is used to set the display width in character cells. If
/// `likely_kgp()` is false, viuer falls back to halfblock automatically.
pub fn display_png(png_bytes: &[u8], term_cols: u16) -> Result<()> {
    // Decode PNG bytes into a DynamicImage.
    let img: DynamicImage = image::load_from_memory_with_format(png_bytes, image::ImageFormat::Png)
        .context("decoding PNG for display")?;

    // viuer config: width in terminal cells, height auto.
    let conf = viuer::Config {
        width: Some(u32::from(term_cols.min(220))),
        height: None,
        absolute_offset: false,
        x: 0,
        y: 0,
        ..Default::default()
    };

    if !likely_kgp() {
        eprintln!(
            "carrac chart: terminal does not support KGP (kitty/ghostty/wezterm recommended). \
             Falling back to halfblock rendering — install kitty for best results, \
             or use --out <file> to save the chart."
        );
    }

    viuer::print(&img, &conf).context("viuer::print failed")?;
    Ok(())
}

/// Write PNG bytes to a file path and print a confirmation line.
pub fn write_png_file(png_bytes: &[u8], path: &str) -> Result<()> {
    std::fs::write(path, png_bytes).with_context(|| format!("writing PNG to {path}"))?;
    println!("chart saved: {path}  ({} KB)", png_bytes.len() / 1024);
    Ok(())
}
