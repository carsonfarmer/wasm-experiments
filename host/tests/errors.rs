//! Pipelines that must be rejected, with useful messages, before running.

mod common;

use common::*;

#[test]
fn mismatched_row_types() {
    for e in ENGINES {
        let msg = err(e, "gen 3 | weigh");
        assert!(msg.contains("`weigh` expects stream<record { name: string"), "{e:?}: {msg}");
        assert!(msg.contains("but `gen` produces stream<record { sha: string"), "{e:?}: {msg}");

        let msg = err(e, "gen 3 | take 2 | weigh");
        assert!(msg.contains("but `take 2` produces"), "{e:?}: {msg}");
    }
}

#[test]
fn projections_must_be_subsets() {
    // `row-lite` has fields `gen`'s commits don't.
    let msg = err(Engine::Pump, "gen 3 | zoo-check-lite 3 1");
    assert!(msg.contains("`zoo-check-lite` expects stream<record { shape:"), "{msg}");
    // Bytes aren't records.
    let msg = err(Engine::Pump, "cat Cargo.toml | top-paths");
    assert!(
        msg.contains("`top-paths` expects stream<record { path: string }>, but `cat Cargo.toml` produces stream<u8>"),
        "{msg}"
    );
}

#[test]
fn malformed_pipelines() {
    let e = Engine::Pump;
    assert!(err(e, "gen 3").contains("at least a producer and a consumer"));
    assert!(err(e, "take 3 | show").contains("can't start with take"));
    assert!(err(e, "gen 3 | take 2").contains("must end with a component"));
    assert!(err(e, "gen 3 | | show").contains("empty stage"));
    assert!(err(e, "gen 3 | take x | show").contains("take expects a number"));
    assert!(err(e, "gen 3 | cat Cargo.toml | show").contains("cat can only start a pipeline"));
    assert!(err(e, "cat Cargo.toml | take 3 | parse-log | top-paths").contains("take can't follow cat"));
    assert!(err(e, "gen 3 | gen 4 | show").contains("`gen` doesn't read a stream"));
    assert!(err(e, "gen 3 | show | show").contains("`show` doesn't produce a stream"));
    assert!(err(e, "gen 3 | errors").contains("`errors` expects"));
}

#[test]
fn bad_tools_and_arguments() {
    let e = Engine::Pump;
    assert!(err(e, "nope | show").contains("no tool named `nope`"));
    assert!(err(e, "gen abc | show").contains("arguments to `gen`"));
    assert!(err(e, "gen 1 2 3 | show").contains("`gen` takes 2 argument(s), got 3"));
    assert!(err(e, "zoo 5 | zoo-check").contains("missing argument of type u64"));
    assert!(err(e, "cat /definitely/not/here.log | parse-log | top-paths").contains("cat /definitely/not/here.log"));
}

#[test]
fn optional_arguments_can_be_skipped() {
    // `_` skips `count` (forever) so `batch` can be given.
    let r = ok(Engine::Pump, "gen _ 64 | take 3 | show");
    assert_eq!(stdout(&r).lines().count(), 3);
}
