# carrac-tui

[Website](https://carrac.cc) · [Get an API key](https://carrac.cc/reg) · [Discord](https://discord.gg/jD4he3hq4A)

A terminal market cockpit for [Carrac.cc](https://carrac.cc). Candlestick charts, open positions, trading plays, alerts, reports, news and AI workflows — all rendered as crisp Unicode in your terminal, fast enough to live in a tmux pane all day. The browser is the canvas; the terminal is the cockpit.

Not signals to follow — a tool for trading on process.

Want a taste with zero install? Pipe a chart straight into your terminal:

```bash
curl carrac.cc/btc
```

<!-- SCREENSHOT: drop a terminal capture of `carrac tui` (chart view, sextant candles + EMA ribbon) here -->

## Install

**Linux and macOS:**

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/Carrac-cc/carrac-tui/releases/latest/download/carrac-tui-installer.sh | sh
```

**Windows** (PowerShell):

```powershell
irm https://github.com/Carrac-cc/carrac-tui/releases/latest/download/carrac-tui-installer.ps1 | iex
```

**Prebuilt archives** — grab one from the [latest release](https://github.com/Carrac-cc/carrac-tui/releases/latest); each contains a single `carrac` binary, drop it on your `PATH`:

| Platform | Archive |
| -------- | ------- |
| Linux x86_64 (glibc) | `carrac-tui-x86_64-unknown-linux-gnu.tar.xz` |
| Linux x86_64 (musl, fully static) | `carrac-tui-x86_64-unknown-linux-musl.tar.xz` |
| Linux aarch64 | `carrac-tui-aarch64-unknown-linux-gnu.tar.xz` |
| macOS Apple Silicon | `carrac-tui-aarch64-apple-darwin.tar.xz` |
| macOS Intel | `carrac-tui-x86_64-apple-darwin.tar.xz` |
| Windows x86_64 | `carrac-tui-x86_64-pc-windows-msvc.zip` |

> On Windows, use a terminal with good Unicode coverage — **Windows Terminal** renders the sextant candles correctly; the legacy console host does not.

**From source** (Rust 1.85+, 2024 edition):

```bash
cargo install --path .
```

**Upgrading:** re-run the installer one-liner (or `cargo install` again) — it
replaces the binary in place. Your settings, API key and watchlist live under
`~/.config/carrac/` and are never touched by an upgrade.

## Quickstart

`carrac` runs **keyless out of the box**. With no API key configured it drops into a demo backed by public Binance market data — every chart works immediately:

```bash
carrac chart BTC      # interactive candlestick chart
carrac tui            # full-screen cockpit
```

Add a **Carrac API key** to unlock every surface — positions, plays, alerts, reports, news, the details matrix, the scanner and AI workflows. Register at [carrac.cc/reg](https://carrac.cc/reg) and grab your key from [app.carrac.cc](https://app.carrac.cc).

## Configure

Settings live at `~/.config/carrac/config.toml` (Linux; on macOS the
equivalent lives under `~/Library/Application Support/`):

```toml
api_key  = "your-carrac-key"
base_url = "https://app.carrac.cc"   # default
backend  = "carrac"                  # "carrac" | "binance" — defaults to carrac when a key is set, else binance
```

Environment variables override the file:

| Variable           | Purpose            |
| ------------------ | ------------------ |
| `CARRAC_API_KEY`   | API key            |
| `CARRAC_BASE_URL`  | Backend base URL   |

## Surfaces

### CLI commands

| Command              | What it does                                                         |
| -------------------- | ------------------------------------------------------------------- |
| `carrac positions`   | Open positions snapshot (`--watch N` to poll, `--detail`, `--asset`) |
| `carrac plays`       | Inspect and evaluate trading plays (`list` / `show <id>` / `eval <id>`) |
| `carrac alerts`      | Alert triage (`list` / `ack <id>` / `clear <id>`)                   |
| `carrac chart <SYM>` | Interactive candlestick chart (`--tf`, `--bars`, `--indicators`, `--position`, `--png`) |
| `carrac tui`         | Full-screen cockpit (all views below)                              |

### `carrac tui` cockpit views

Switch with number keys `1`–`7` (or `Tab` / `Space` to cycle):

`chart` · `positions` · `plays` · `alerts` · `report` · `news` · `details`

Press `a` for the asset selector, `r` to force-refresh, and `?` for the full key reference.

### Bring your own AI agent

There is no built-in chatbot. Press `I` ("Intelligence") in the cockpit and `carrac` suspends itself, writes the current view's context to a workspace — for the chart view that's a rendered `chart.png` plus the underlying data as JSON — and hands your terminal to **your own agent** (Claude Code by default; configurable via `[agent] command = "..."` in config.toml, e.g. `aider` or `gemini`). Quit the agent to drop back into the cockpit.

Your agent doesn't reason over a screenshot of price levels: it gets the chart as structured data, and — once you've connected it to the Carrac MCP server — live access to your positions, open plays, journal and the market structure underneath. Data, not pixels. Your agent, your tokens, your tooling.

## Chart

The default renderer uses a **sextant** raster (six sub-cells per character) so candles stay readable at any terminal size: bodies are cell-quantized rectangles with sharp edges, while wicks are drawn at sub-cell resolution for precision. On top of price it layers the CPR pivot set, an EMA ribbon (EMA30/EMA60 with per-bar trend coloring) plus EMA21, and optional volume, RSI and MACD sub-panes and a market-session strip. Everything uses ANSI named colors, so your terminal palette governs the hue. Toggle `s` for a clean close-price line instead.

> **Terminal support:** sextant glyphs come from Unicode's *Symbols for Legacy Computing* block. Terminals that synthesize block glyphs themselves — **kitty, Ghostty, WezTerm, foot** — render them pixel-perfect out of the box. On other terminals you need a font that covers that block (e.g. Iosevka, Cascadia Code); if you see hollow boxes, switch font or toggle `s` for the line style, which uses plain box-drawing characters everywhere.

Main keys in the chart view:

| Key            | Action                                              |
| -------------- | --------------------------------------------------- |
| `t` / `T`      | Cycle timeframe (`5m → 15m → 1H → 4H → 1D → 1W`)     |
| `d` / `w`      | Jump to 1D / 1W                                      |
| `h` / `l`      | Pan left / right · `H` / `L` half-window            |
| `j` / `k`      | Vertical zoom out / in · `↑` / `↓` pan the price window |
| `]` / `[`      | More / fewer candles (zoom)                         |
| `0`            | Reset pan + zoom                                     |
| `c e E p v R o m` | Toggle CPR · EMA21 · EMA ribbon · position · volume · RSI · overlays · MACD |
| `z`            | Toggle market-session strip                         |
| `s`            | Toggle candle style (sextant ⇄ line)                |
| `P`            | Render a PNG chart (inline on kitty/ghostty/wezterm)|
| left-click     | Pin the crosshair to a column · right-click / `x` clears it |

## Bring your own backend

Chart data flows through a single provider seam (the `Provider` enum). Two backends ship today: the authenticated Carrac platform and a keyless public Binance adapter (crypto OHLCV only). The seam is enum-dispatched and the renderer treats every Carrac-computed overlay as optional, so a third data source can slot in without touching the render code. A formal wire-contract document for backend authors is on the way.

## License

Dual-licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

PNG chart rendering embeds the **Liberation Sans** font, © Red Hat, licensed
under the [SIL Open Font License 1.1](assets/fonts/OFL.txt) (bundled unmodified
so the binary needs no system fonts).

Built with [Claude Code](https://claude.com/claude-code).
