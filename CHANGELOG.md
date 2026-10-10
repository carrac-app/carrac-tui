# Changelog

All notable changes to carrac-tui are documented here.

## [1.0.1]

Carrac is now a self-hosted, open-source app, and the client follows it.

- The default backend is a Carrac server on this machine
  (`http://localhost:5002`) instead of the hosted service
- The API-key prompts say how to connect to your own server
- The project lives at `github.com/carrac-app/carrac-tui`; installer and
  `cargo install` URLs follow it
- Security reports go through GitHub's private vulnerability reporting

## [1.0.0]

Initial public release.

- Interactive sextant candlestick charts (`carrac chart`, `carrac tui`) with
  CPR pivot sets, EMA-21, EMA ribbon, volume, RSI, market-session strip
  and position SL/Entry/TP overlays
- Full-screen cockpit (`carrac tui`): chart, positions, plays, alerts,
  reports, news and a multi-asset details matrix
- CLI commands: `positions`, `plays`, `alerts`, `chart`
- Keyless demo mode backed by public Binance market data; the full surface
  unlocks with a Carrac API key
- Bring-your-own-agent: `I` suspends the TUI and hands your terminal to your
  own AI agent (Claude Code, aider, gemini, …) with the current view's context
  prepared in a workspace
- PNG chart rendering, inline on kitty/Ghostty/WezTerm (`--png`, `P`)
- Prebuilt binaries for Linux (x86_64 gnu/musl, aarch64), macOS (Apple Silicon,
  Intel) and Windows (x86_64) via cargo-dist, with shell + PowerShell installers
