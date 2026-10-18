//! The `wp` command itself.

mod common;

use std::io::Write;
use std::process::{Command, Output, Stdio};

fn wp(args: &[&str], input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_wp"))
        .args(args)
        .env("WP_PATH", common::tools_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).unwrap()
}

#[test]
fn prints_the_result_as_wave() {
    let out = wp(&["gen 3 | count"], b"");
    assert!(out.status.success());
    assert_eq!(text(&out.stdout), "3\n");
}

#[test]
fn the_pipeline_can_be_split_across_arguments() {
    let out = wp(&["gen", "3", "|", "count"], b"");
    assert_eq!(text(&out.stdout), "3\n");
}

#[test]
fn reads_stdin() {
    let log = common::access_log();
    let out = wp(&["parse-log | top-paths 3"], log);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), common::expected_top(log, 3, None));
}

#[test]
fn lists_tools() {
    let out = wp(&["--list"], b"");
    assert!(out.stdout.starts_with(b"count(input: stream<commit>, batch: option<u32>) -> u64\n"));
    assert_eq!(text(&out.stdout).lines().count(), 10);
}

#[test]
fn errors_exit_nonzero_with_a_message() {
    let out = wp(&["gen 3 | top-paths"], b"");
    assert!(!out.status.success());
    assert_eq!(text(&out.stderr), "wp: `top-paths` reads stream<request>, but `gen` produces stream<commit>\n");
    assert_eq!(out.stdout, b"");
}

#[test]
fn stats_are_json() {
    let out = wp(&["--stats", "gen 10 | count"], b"");
    let stats: serde_json::Value = serde_json::from_slice(&out.stderr).unwrap();
    assert!(stats["load_ms"].as_f64().unwrap() > 0.0);
    assert!(stats["run_ms"].as_f64().unwrap() > 0.0);
}
