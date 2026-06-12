//! Hidden `carrac render` subcommand: read a chart JSON payload on stdin,
//! write a composed ANSI chart to stdout.
//! No config, no network, no API key — pure stdin → stdout.

use std::io::Read;

use clap::Args;

use crate::chart::data::ChartResponse;
use crate::chart::oneshot::{OneshotOpts, render_oneshot};

#[derive(Args, Debug)]
pub struct RenderArgs {
    /// Terminal columns
    #[arg(long, default_value_t = 80)]
    pub cols: usize,
    /// Chart rows (body height)
    #[arg(long, default_value_t = 26)]
    pub rows: usize,
    /// Candles to show (tail of the payload)
    #[arg(long, default_value_t = 80)]
    pub bars: u32,
    /// EMA ribbon overlay
    #[arg(long, default_value_t = false)]
    pub ema: bool,
    /// Strip all ANSI (pure text)
    #[arg(long, default_value_t = false)]
    pub plain: bool,
    /// Chart only — no title, no footer
    #[arg(long, default_value_t = false)]
    pub quiet: bool,
    /// Human window label for the title, e.g. "10d"
    #[arg(long, default_value = "")]
    pub window_label: String,
    /// Chart style (only "sextant" supported)
    #[arg(long, default_value = "sextant")]
    pub style: String,
}

pub fn run(args: RenderArgs) -> i32 {
    if args.style != "sextant" {
        eprintln!("carrac render: unsupported style '{}'", args.style);
        return 2;
    }

    // Force-enable ANSI color: in piped mode stdout is not a TTY, but the
    // consumer of this output still wants the ANSI sequences.
    // Skip this when --plain is set (render_oneshot will strip ANSI anyway, but
    // forcing color here avoids generating then stripping unnecessarily).
    if !args.plain {
        crate::format::force_enable_color();
    }

    let mut buf = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut buf) {
        eprintln!("carrac render: stdin read error: {e}");
        return 2;
    }
    let data: ChartResponse = match serde_json::from_str(&buf) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("carrac render: bad payload JSON: {e}");
            return 2;
        }
    };
    if data.ohlcv.is_empty() {
        eprintln!("carrac render: payload has no candles");
        return 2;
    }
    // Clamp dimensions to sane bounds before forwarding to the renderer.
    // Server sends cols∈{80,100,120,160} and rows=26 — these are all inside
    // the valid range, so the server path is unaffected.
    let cols = args.cols.clamp(20, 400);
    let rows = args.rows.clamp(5, 100);
    let opts = OneshotOpts {
        cols,
        rows,
        show_ema: false,
        show_ema_ribbon: args.ema, // ?ema = the ribbon, per the public :help text
        plain: args.plain,
        quiet: args.quiet,
        window_label: args.window_label,
        visible_bars: args.bars,
        pos: None,
    };
    let out = render_oneshot(&data, &opts);
    if let Err(e) = {
        use std::io::Write;
        std::io::stdout().write_all(out.as_bytes())
    } {
        eprintln!("carrac render: write error: {e}");
        return 74; // EX_IOERR — distinct from the input-error exit code (2)
    }
    0
}
