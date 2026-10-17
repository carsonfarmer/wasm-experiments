//! End-to-end pipeline tests, run against every copy engine.

mod common;

use common::*;
use wasmtime::component::Val;

#[test]
fn two_components_one_stream() {
    for e in ENGINES {
        let r = ok(e, "gen 5 | show");
        let out = stdout(&r);
        assert_eq!(out.lines().count(), 5, "{e:?}");
        assert!(out.contains("change #4"), "{e:?}");
        assert!(matches!(r.result, Val::U64(5)), "{e:?}");
    }
}

#[test]
fn rows_and_columns_carry_the_same_data() {
    let rows = stdout(&ok(Engine::Pump, "gen 1000 | show"));
    for e in ENGINES {
        // An odd batch size, so batches don't line up with anything.
        let cols = stdout(&ok(e, "gen-cols 1000 37 | show-cols"));
        assert_eq!(cols, rows, "{e:?}");
    }
}

#[test]
fn take_forwards_exactly_n_rows() {
    let all = stdout(&ok(Engine::Pump, "gen 1000 | show"));
    let first = |n: usize| all.lines().take(n).map(|l| format!("{l}\n")).collect::<String>();
    for e in ENGINES {
        for n in [0usize, 1, 3, 255, 256, 257, 999, 1000] {
            let out = stdout(&ok(e, &format!("gen 1000 | take {n} | show")));
            assert_eq!(out, first(n), "{e:?} take {n}");
        }
        // The producer ending first is fine too.
        assert_eq!(stdout(&ok(e, "gen 2 | take 10 | show")), first(2), "{e:?}");
    }
}

#[test]
fn take_cuts_columnar_batches_by_rows() {
    let all = stdout(&ok(Engine::Pump, "gen 1000 | show"));
    let first = |n: usize| all.lines().take(n).map(|l| format!("{l}\n")).collect::<String>();
    for e in ENGINES {
        for n in [0usize, 1, 63, 64, 65, 130, 999, 1000, 5000] {
            let r = ok(e, &format!("gen-cols 1000 64 | take {n} | show-cols"));
            assert_eq!(stdout(&r), first(n.min(1000)), "{e:?} take {n}");
            assert!(matches!(r.result, Val::U64(c) if c == n.min(1000) as u64), "{e:?} take {n}");
        }
    }
}

#[test]
fn chained_takes_use_the_smallest() {
    for e in ENGINES {
        assert_eq!(stdout(&ok(e, "gen 100 | take 50 | take 7 | show")).lines().count(), 7, "{e:?}");
        assert_eq!(stdout(&ok(e, "gen 100 | take 5 | take 70 | show")).lines().count(), 5, "{e:?}");
    }
}

#[test]
fn take_cancels_an_infinite_producer() {
    for e in ENGINES {
        let r = ok(e, "gen | take 10 | show");
        assert_eq!(stdout(&r).lines().count(), 10, "{e:?}");
        assert!(stderr(&r).contains("reader hung up after 10 commits"), "{e:?}: {}", stderr(&r));

        let r = ok(e, "gen-cols | take 5000 | count-cols");
        assert!(matches!(r.result, Val::U64(5000)), "{e:?}");
    }
}

#[test]
fn every_value_kind_round_trips() {
    for e in ENGINES {
        for seed in [1u64, 2, 99] {
            let r = ok(e, &format!("zoo 3000 {seed} | zoo-check 3000 {seed}"));
            assert!(matches!(r.result, Val::U64(3000)), "{e:?} seed {seed}: {:?}\n{}", r.result, stderr(&r));
        }
    }
}

#[test]
fn projection_hands_consumers_only_their_fields() {
    for e in ENGINES {
        let r = ok(e, "zoo 2000 5 | zoo-check-lite 2000 5");
        assert!(matches!(r.result, Val::U64(2000)), "{e:?}: {:?}\n{}", r.result, stderr(&r));
        // Projection and take compose.
        let r = ok(e, "zoo 2000 5 | take 333 | zoo-check-lite 333 5");
        assert!(matches!(r.result, Val::U64(333)), "{e:?}: {:?}\n{}", r.result, stderr(&r));
    }
}

#[test]
fn take_preserves_every_value_kind() {
    for e in ENGINES {
        let r = ok(e, "zoo 3000 11 | take 777 | zoo-check 777 11");
        assert!(matches!(r.result, Val::U64(777)), "{e:?}: {:?}\n{}", r.result, stderr(&r));
    }
}

#[test]
fn log_queries_match_the_native_answer() {
    let log = access_log().display().to_string();
    let (q1, q1_rows) = expected_top(access_log(), 10, Some(500));
    let (q2, q2_rows) = expected_top(access_log(), 10, None);
    assert!(q1_rows > 100 && q2_rows == 60_000);
    for e in ENGINES {
        for p in [
            format!("cat {log} | parse-log | errors | top-paths 10"),
            format!("cat {log} | parse-log | errors | top-paths-full 10"),
            format!("cat {log} | parse-log-cols | errors-cols | top-paths-cols 10"),
            format!("cat {log} | parse-log-cols | errors-cols | top-paths-cols-full 10"),
        ] {
            let r = ok(e, &p);
            assert_eq!(stdout(&r), q1, "{e:?}: {p}");
            assert_eq!(r.count(), Some(q1_rows), "{e:?}: {p}");
        }
        for p in [
            format!("cat {log} | parse-log | top-paths 10"),
            format!("cat {log} | parse-log | top-paths-full 10"),
            format!("cat {log} | parse-log-cols | top-paths-cols 10"),
            format!("cat {log} | parse-log-cols | top-paths-cols-full 10"),
        ] {
            let r = ok(e, &p);
            assert_eq!(stdout(&r), q2, "{e:?}: {p}");
            assert_eq!(r.count(), Some(q2_rows), "{e:?}: {p}");
        }
    }
}

#[test]
fn errors_can_raise_the_threshold() {
    let log = access_log().display().to_string();
    let (want, rows) = expected_top(access_log(), 5, Some(404));
    for e in ENGINES {
        let r = ok(e, &format!("cat {log} | parse-log | errors 404 | top-paths 5"));
        assert_eq!((stdout(&r), r.count()), (want.clone(), Some(rows)), "{e:?}");
        let r = ok(e, &format!("cat {log} | parse-log-cols | errors-cols 404 | top-paths-cols 5"));
        assert_eq!((stdout(&r), r.count()), (want.clone(), Some(rows)), "{e:?}");
    }
}

#[test]
fn different_row_types_flow_through_take() {
    for e in ENGINES {
        let r = ok(e, "fruit | take 4 | weigh");
        let out = stdout(&r);
        assert_eq!(out.lines().count(), 4, "{e:?}");
        assert!(out.contains("[fuzzy, green, tart]") && out.contains("87.5 g"), "{e:?}: {out}");
    }
}
