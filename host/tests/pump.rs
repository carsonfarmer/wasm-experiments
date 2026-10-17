//! What the host pump does (and doesn't) do: which pipes it carries, how
//! much it copies, and that steady-state forwarding doesn't allocate.

mod common;

use common::*;
use std::process::Command;

#[test]
fn flat_and_matching_pipes_are_left_to_wasmtime_in_one_store() {
    // Pointer-free rows (u8) between cat and parse-log aren't pumped.
    let log = access_log().display().to_string();
    let r = ok(Engine::Pump, &format!("cat {log} | parse-log-cols | top-paths-cols-full 3"));
    assert_eq!(r.pipes.len(), 1, "{:?}", r.pipes);
    // With Wasmtime's copy, only pipes that need the host are pumped.
    let r = ok(Engine::Wasmtime, "gen 100 | show");
    assert!(r.pipes.is_empty());
    let r = ok(Engine::Wasmtime, "gen 100 | take 5 | show");
    assert_eq!(r.pipes.len(), 1);
}

#[test]
fn projection_copies_only_requested_fields() {
    let log = access_log().display().to_string();
    for e in [Engine::Pump, Engine::Parallel] {
        let full = ok(e, &format!("cat {log} | parse-log | top-paths-full 3"));
        let lean = ok(e, &format!("cat {log} | parse-log | top-paths 3"));
        let (f, l) = (&full.pipes[0], &lean.pipes[0]);
        assert_eq!((f.rows, l.rows), (60_000, 60_000));
        // Whole rows hold 4 strings; `top-paths` asked for one.
        assert_eq!(f.reallocs, 4 * 60_000, "{e:?}");
        assert_eq!(l.reallocs, 60_000, "{e:?}");
        assert!(l.bytes * 2 < f.bytes, "{e:?}: staged {} vs {}", l.bytes, f.bytes);

        // Columnar: one realloc per column per batch, and only `path` is staged.
        let full = ok(e, &format!("cat {log} | parse-log-cols | top-paths-cols-full 3"));
        let lean = ok(e, &format!("cat {log} | parse-log-cols | top-paths-cols 3"));
        let (f, l) = (&full.pipes[0], &lean.pipes[0]);
        assert_eq!((f.rows, l.rows), (60_000, 60_000));
        assert_eq!(f.reallocs, 12 * f.batches, "{e:?}");
        assert_eq!(l.reallocs, 2 * l.batches, "{e:?}");
        assert!(l.bytes * 5 < f.bytes, "{e:?}: staged {} vs {}", l.bytes, f.bytes);
    }
}

#[test]
fn pipe_counters_add_up() {
    let log = access_log().display().to_string();
    let (_, errors) = expected_top(access_log(), 10, Some(500));
    let r = ok(Engine::Parallel, &format!("cat {log} | parse-log | errors | top-paths 10"));
    assert_eq!(r.pipes.len(), 2);
    assert_eq!(r.pipes[0].label, "parse-log → errors");
    assert_eq!(r.pipes[0].rows, 60_000);
    assert_eq!(r.pipes[1].label, "errors → top-paths");
    assert_eq!(r.pipes[1].rows, errors);
    assert!(r.parallel);
    assert!(r.cat_bytes > 10_000_000);
}

fn wp_json(args: &[&str]) -> serde_json::Value {
    let out = Command::new(env!("CARGO_BIN_EXE_wp"))
        .current_dir(workspace())
        .env("WP_TOOLS", tools())
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let err = String::from_utf8(out.stderr).unwrap();
    serde_json::from_str(err.lines().rev().find(|l| l.starts_with('{')).unwrap()).unwrap()
}

#[test]
fn steady_state_forwarding_does_not_allocate_per_row() {
    for (small, big) in [
        (["--json", "--serial", "gen 20000 256 | count 256"], ["--json", "--serial", "gen 400000 256 | count 256"]),
        (
            ["--json", "--serial", "gen-cols 20000 1024 | count-cols"],
            ["--json", "--serial", "gen-cols 400000 1024 | count-cols"],
        ),
    ] {
        let a = wp_json(&small);
        let b = wp_json(&big);
        let extra = b["host_allocs"].as_u64().unwrap() as f64 - a["host_allocs"].as_u64().unwrap() as f64;
        // 380,000 more rows; a handful of allocations per batch at most.
        assert!(extra / 380_000.0 < 0.05, "{}: {extra} extra host allocations", small[2]);
    }
}

#[test]
fn wasmtime_copy_allocates_per_value() {
    let a = wp_json(&["--json", "--copy", "wasmtime", "gen 100000 256 | count 256"]);
    let b = wp_json(&["--json", "--serial", "gen 100000 256 | count 256"]);
    let (wasmtime, pump) = (a["host_allocs"].as_u64().unwrap(), b["host_allocs"].as_u64().unwrap());
    assert!(wasmtime > 100 * pump, "Wasmtime copy {wasmtime} vs pump {pump}");
}

#[test]
fn cli_reports_stats_and_fails_cleanly() {
    let v = wp_json(&["--json", "gen 1000 | take 10 | count"]);
    assert_eq!(v["rows"], 10);
    assert_eq!(v["parallel"], true);
    assert_eq!(v["pipes"][0]["label"], "gen → take 10 → count");

    let out = Command::new(env!("CARGO_BIN_EXE_wp")).env("WP_TOOLS", tools()).arg("gen 3 | weigh").output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("`weigh` expects"));
}
