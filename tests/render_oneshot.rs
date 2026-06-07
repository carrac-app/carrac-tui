//! Integration tests for `carrac render` (hidden stdin→stdout subcommand).
//!
//! To regenerate golden files after an intentional output change:
//!   UPDATE_GOLDEN=1 cargo test --test render_oneshot

use std::io::Write;
use std::process::{Command, Stdio};

fn run_render(args: &[&str], stdin_payload: &[u8]) -> (Vec<u8>, Vec<u8>, i32) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_carrac"))
        .arg("render")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn carrac render");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin_payload)
        .unwrap();
    let out = child.wait_with_output().expect("wait");
    (out.stdout, out.stderr, out.status.code().unwrap_or(-1))
}

#[test]
fn golden_btc_4h_80x26_plain() {
    let payload = std::fs::read("tests/fixtures/chart_btc_4h.json").expect("fixture");
    let (stdout, _stderr, code) = run_render(
        &[
            "--cols",
            "80",
            "--rows",
            "26",
            "--bars",
            "24",
            "--window-label",
            "1d",
            "--plain",
        ],
        &payload,
    );
    assert_eq!(code, 0);
    let out = String::from_utf8(stdout).unwrap();
    let golden_path = "tests/golden/btc_4h_80x26.txt";
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(golden_path, &out).unwrap();
    }
    let golden = std::fs::read_to_string(golden_path).expect("golden — run UPDATE_GOLDEN=1 once");
    assert_eq!(out, golden, "byte-identical render (determinism contract)");
}

#[test]
fn colored_output_contains_ansi_and_title() {
    // The render subcommand force-enables ANSI even though stdout is a pipe
    // (non-TTY).  This is the whole point of the curl endpoint integration:
    // the server pipes our output to the HTTP response, and the downstream
    // client expects ANSI colour codes.
    let payload = std::fs::read("tests/fixtures/chart_btc_4h.json").expect("fixture");
    let (stdout, _, code) = run_render(&["--bars", "24", "--window-label", "1d"], &payload);
    assert_eq!(code, 0);
    let out = String::from_utf8(stdout).unwrap();
    assert!(out.contains('\x1b'), "ANSI present without --plain");
    assert!(out.contains("viewing 1d"));
}

#[test]
fn bad_json_exits_2_with_empty_stdout() {
    let (stdout, stderr, code) = run_render(&[], b"not json");
    assert_eq!(code, 2);
    assert!(stdout.is_empty());
    assert!(String::from_utf8_lossy(&stderr).contains("bad payload JSON"));
}

#[test]
fn empty_candles_exits_2() {
    // Minimal valid ChartResponse JSON with required fields but no candles.
    let (stdout, stderr, code) = run_render(&[], br#"{"symbol":"X","timeframe":"4h","ohlcv":[]}"#);
    assert_eq!(code, 2);
    assert!(stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&stderr).contains("no candles"),
        "stderr should mention no candles, got: {}",
        String::from_utf8_lossy(&stderr),
    );
}

#[test]
fn non_utf8_stdin_exits_2() {
    let (stdout, _, code) = run_render(&[], &[0xFF, 0xFE]);
    assert_eq!(code, 2);
    assert!(stdout.is_empty());
}

#[test]
fn clamp_cols_zero_produces_non_empty_output() {
    // cols=0 is clamped to 20; rows=0 is clamped to 5.
    // Must produce non-empty output and exit 0 (no panic).
    let payload = std::fs::read("tests/fixtures/chart_btc_4h.json").expect("fixture");
    let (stdout, _stderr, code) = run_render(
        &["--cols", "0", "--rows", "0", "--bars", "24", "--plain"],
        &payload,
    );
    assert_eq!(code, 0, "exit code 0 for clamped dims");
    assert!(
        !stdout.is_empty(),
        "non-empty output after clamping cols/rows to min"
    );
}

#[test]
fn clamp_cols_huge_produces_non_empty_output() {
    // cols=9999 is clamped to 400; rows=9999 is clamped to 100.
    let payload = std::fs::read("tests/fixtures/chart_btc_4h.json").expect("fixture");
    let (stdout, _stderr, code) = run_render(
        &[
            "--cols", "9999", "--rows", "9999", "--bars", "24", "--plain",
        ],
        &payload,
    );
    assert_eq!(code, 0, "exit code 0 for oversized dims");
    assert!(
        !stdout.is_empty(),
        "non-empty output after clamping cols/rows to max"
    );
}
