# Contributing to carrac-tui

Thanks for your interest — contributions are welcome.

## How this repo works

This is the public home of `carrac-tui`. Development happens in a separate
upstream repository, and releases are published here. In practice that means:

- **Issues are the best way to help.** Bug reports, feature requests, terminal
  compatibility notes and questions are all very welcome — open an issue.
- **Pull requests are welcome for focused fixes.** Because changes are
  integrated upstream, a merged idea may land here as part of a later release
  commit rather than as your commit directly. Either way you'll be credited in
  the changelog. For anything larger than a small fix, please open an issue
  first so we can agree on the approach before you spend time on it.

## Developing

Standard Rust toolchain, MSRV **1.88**:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

All three must pass. The chart renderer has a byte-exact golden test
(`tests/render_oneshot`); if a change intentionally alters rendered output,
regenerate it with `UPDATE_GOLDEN=1 cargo test --test render_oneshot` and say so.

## Bringing your own backend

`carrac-tui` talks to a backend through a single provider seam. The data
contract is the `ChartResponse` shape in `src/chart/data.rs`; `src/chart/provider.rs`
and `src/chart/kraken.rs` are the reference adapters. A new OHLCV source can be
added without touching the renderer.

## Licensing

By contributing you agree your contribution is dual-licensed under MIT and
Apache-2.0, matching the project license. There's no CLA.
