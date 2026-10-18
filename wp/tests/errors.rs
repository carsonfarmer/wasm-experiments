//! Pipelines that must be rejected before anything runs, with clear messages.

mod common;

use common::*;

#[test]
fn row_types_must_match() {
    assert_eq!(error("gen 3 | top-paths"), "`top-paths` reads stream<request>, but `gen` produces stream<commit>");
    assert_eq!(error("gen 3 | count-flat"), "`count-flat` reads stream<tick>, but `gen` produces stream<commit>");
    assert_eq!(
        error("gen 3 | parse-log | top-paths"),
        "`parse-log` reads stream<u8>, but `gen` produces stream<commit>"
    );
}

#[test]
fn streams_must_connect() {
    assert_eq!(error("gen 3"), "`gen` produces stream<commit>, but nothing reads it");
    assert_eq!(error("gen 3 | gen 3 | show"), "`gen` produces stream<commit>, but `gen` doesn't read a stream");
    assert_eq!(error("gen 3 | show | show"), "`show` doesn't produce a stream, so nothing can follow it");
    assert_eq!(error("count"), "`count` reads stream<commit>, but a pipeline can only start with bytes");
}

#[test]
fn pipelines_must_be_well_formed() {
    assert_eq!(error(""), "empty stage in pipeline");
    assert_eq!(error("gen 3 | | show"), "empty stage in pipeline");
    assert_eq!(error("gen 3 |"), "empty stage in pipeline");
    assert_eq!(error("gen 'oops | show"), "unbalanced quotes in pipeline");
    assert_eq!(error("nope | show"), "no tool named `nope`");
}

#[test]
fn arguments_must_fit_the_tool() {
    assert_eq!(error("gen 1 2 3 | show"), "arguments to `gen`: takes 2 argument(s), got 3");
    assert_eq!(error("gen x | show"), "arguments to `gen`: `count`: expected option<u64>, got `x`");
    assert_eq!(error("gen -1 | show"), "arguments to `gen`: `count`: expected option<u64>, got `-1`");
    assert_eq!(error("zoo 5 | zoo-check 5 1"), "arguments to `zoo`: missing `seed` (u64)");
    assert_eq!(error("gen 3 | show many"), "arguments to `show`: `limit`: expected option<u64>, got `many`");
}

#[test]
fn arguments_are_wave_values() {
    // Options can be bare, `some(...)` or `none`; trailing ones can be left out.
    assert_eq!(u64(&run("gen some(5) some(2) | count 1")), 5);
    assert_eq!(run("gen none 64 | show 3").stdout.lines().count(), 3);
}

/// A directory of extra tools, searched before the example tools.
fn with_tools(files: &[(&str, &[u8])], test: impl FnOnce(wp::Wp)) {
    let dir = std::env::temp_dir().join(format!("wp-test-{}-{}", std::process::id(), files[0].0));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, bytes) in files {
        std::fs::write(dir.join(name), bytes).unwrap();
    }
    test(wp::Wp::new(vec![dir.clone(), tools_dir()]).unwrap());
    std::fs::remove_dir_all(dir).unwrap();
}

/// A tool producing `stream<commit>` where `commit` has only a `sha`.
const OLD_GEN: &str = r#"(component
  (import "wp:demo/types@0.1.0" (instance $types
    (type $commit (record (field "sha" string)))
    (export "commit" (type (eq $commit)))))
  (alias export $types "commit" (type $commit))
  (core module $m (func (export "run") (result i32) unreachable))
  (core instance $i (instantiate $m))
  (func (export "run") (result (stream $commit)) (canon lift (core func $i "run"))))"#;

#[test]
fn same_named_types_must_also_match_in_structure() {
    with_tools(&[("old-gen.wasm", OLD_GEN.as_bytes())], |wp| {
        let err = wp.pipeline("old-gen | count").err().unwrap().to_string();
        assert_eq!(
            err,
            "`count` reads stream<record { sha: string, author: string, message: string, time: s64 }>, \
             but `old-gen` produces stream<record { sha: string }>"
        );
    });
}

#[test]
fn files_that_arent_tools_are_rejected() {
    let no_run = b"(component)";
    with_tools(&[("junk.wasm", b"not wasm"), ("no-run.wasm", no_run)], |wp| {
        assert!(wp.tool("junk").is_err());
        assert_eq!(wp.tool("no-run").err().unwrap().to_string(), "`no-run` doesn't export a `run` function");
        let names: Vec<String> = wp.tools().unwrap().into_iter().map(|t| t.name).collect();
        assert!(!names.iter().any(|n| n == "junk" || n == "no-run"), "{names:?}");
        assert!(names.iter().any(|n| n == "gen"));
    });
}

#[test]
fn earlier_directories_on_the_path_win() {
    with_tools(&[("count.wasm", OLD_GEN.as_bytes())], |wp| {
        assert_eq!(wp.tool("count").unwrap().to_string(), "count() -> stream<commit>");
    });
}

#[test]
fn tools_describe_themselves() {
    let tools: Vec<String> = wp().tools().unwrap().iter().map(|t| t.to_string()).collect();
    assert_eq!(
        tools,
        [
            "count(input: stream<commit>, batch: option<u32>) -> u64",
            "count-flat(input: stream<tick>, batch: option<u32>) -> u64",
            "errors(input: stream<request>, min-status: option<u16>) -> stream<request>",
            "gen(count: option<u64>, batch: option<u32>) -> stream<commit>",
            "gen-flat(count: option<u64>, batch: option<u32>) -> stream<tick>",
            "parse-log(input: stream<u8>) -> stream<request>",
            "show(input: stream<commit>, limit: option<u64>)",
            "top-paths(input: stream<request>, n: option<u32>)",
            "zoo(count: u64, seed: u64) -> stream<row>",
            "zoo-check(input: stream<row>, count: u64, seed: u64) -> u64",
        ]
    );
}
