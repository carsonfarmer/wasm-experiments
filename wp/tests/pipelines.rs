//! Pipelines that should work, and what they should produce.

mod common;

use common::*;
use std::time::{Duration, Instant};

#[test]
fn rows_flow_from_one_tool_to_the_next() {
    let out = run("gen 3 | show");
    assert_eq!(
        out.stdout,
        "0000000000  ada      change #0\n9e3779b97f  grace    change #1\n3c6ef372fe  linus    change #2\n"
    );
    assert_eq!(out.result, None, "show returns nothing");
    assert_eq!(out.stderr, "");
}

#[test]
fn every_row_arrives_whatever_the_batch_sizes() {
    for write in [1, 2, 7, 256, 1000] {
        for read in [1, 3, 256, 5000] {
            assert_eq!(u64(&run(&format!("gen 1000 {write} | count {read}"))), 1000, "write {write}, read {read}");
            assert_eq!(
                u64(&run(&format!("gen-flat 1000 {write} | count-flat {read}"))),
                1000,
                "write {write}, read {read}"
            );
        }
    }
}

#[test]
fn empty_and_single_row_streams() {
    assert_eq!(u64(&run("gen 0 | count")), 0);
    assert_eq!(u64(&run("gen 1 | count")), 1);
    assert_eq!(u64(&run("gen-flat 0 | count-flat")), 0);
    assert_eq!(run("gen 0 | show").stdout, "");
}

#[test]
fn a_consumer_can_stop_early() {
    let all = run("gen 100 | show").stdout;
    let first = |n: usize| all.lines().take(n).map(|l| format!("{l}\n")).collect::<String>();
    for n in [0, 1, 7, 99, 100, 250] {
        assert_eq!(run(&format!("gen 100 | show {n}")).stdout, first(n), "show {n}");
    }
}

#[test]
fn stopping_early_cancels_an_infinite_producer() {
    let start = Instant::now();
    let out = run("gen | show 10");
    assert_eq!(out.stdout.lines().count(), 10);
    assert!(start.elapsed() < Duration::from_secs(5), "took {:?}", start.elapsed());
}

#[test]
fn every_kind_of_wit_value_round_trips() {
    for seed in [1, 2, 3, 99] {
        for count in [0, 1, 37, 1000] {
            let out = run(&format!("zoo {count} {seed} | zoo-check {count} {seed}"));
            assert_eq!(u64(&out), count, "seed {seed}, count {count}: {}", out.stderr);
            assert_eq!(out.stderr, "");
        }
    }
}

#[test]
fn log_queries_match_a_native_computation() {
    let log = access_log();
    let all = run_with("parse-log | top-paths 10", log.clone());
    assert_eq!(all.stdout, expected_top(log, 10, None));
    assert_eq!(all.stderr, "", "every generated line parses");
    for min in [400, 404, 500, 503] {
        let out = run_with(&format!("parse-log | errors {min} | top-paths 5"), log.clone());
        assert_eq!(out.stdout, expected_top(log, 5, Some(min)), "errors {min}");
    }
    // Nothing has status 600, and the default threshold is 500.
    assert_eq!(run_with("parse-log | errors 600 | top-paths", log.clone()).stdout, "");
    assert_eq!(run_with("parse-log | errors | top-paths 5", log.clone()).stdout, expected_top(log, 5, Some(500)));
}

#[test]
fn long_pipelines_compose() {
    let log = access_log();
    let out = run_with("parse-log | errors 0 | errors 400 | errors 500 | top-paths 10", log.clone());
    assert_eq!(out.stdout, expected_top(log, 10, Some(500)));
}

#[test]
fn bad_input_lines_are_skipped() {
    let ok = r#"10.0.0.1 - - [01/Jan/2026:00:00:00 +0000] "GET /a HTTP/1.1" 200 5 "-" "-""#;
    let ok_crlf = r#"10.0.0.2 - - [01/Jan/2026:00:00:01 +0000] "GET /b HTTP/1.1" 500 5 "-" "-""#;
    let mut input = format!("{ok}\nnot a log line\n\n{ok_crlf}\r\n{ok}").into_bytes();
    input.extend_from_slice(b"\n\xff\xfe invalid utf-8\n");
    input.extend_from_slice(ok.as_bytes()); // no trailing newline
    let out = run_with("parse-log | top-paths", input);
    assert_eq!(out.stdout, "        3 /a\n        1 /b\n");
    assert_eq!(out.stderr, "parse-log: skipped 3 lines\n");
}

#[test]
fn empty_input() {
    let out = run_with("parse-log | top-paths", Vec::new());
    assert_eq!((out.stdout.as_str(), out.stderr.as_str()), ("", ""));
}

#[test]
fn input_split_into_tiny_reads_still_parses() {
    // Bytes arrive in whatever chunks the reader returns; lines span them.
    struct Trickle(Vec<u8>, usize);
    impl tokio::io::AsyncRead for Trickle {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            let (data, pos) = (&self.0, self.1);
            let n = 7.min(data.len() - pos).min(buf.remaining());
            buf.put_slice(&data[pos..pos + n]);
            self.1 += n;
            std::task::Poll::Ready(Ok(()))
        }
    }
    let log: Vec<u8> = access_log()[..200_000].to_vec();
    let expected = run_with("parse-log | top-paths 5", log.clone()).stdout;
    let wp = wp();
    let (stdout, wasi) = {
        let p = wasmtime_wasi::p2::pipe::MemoryOutputPipe::new(usize::MAX);
        (p.clone(), wasmtime_wasi::WasiCtx::builder().stdout(p).build())
    };
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    rt.block_on(async { wp.pipeline("parse-log | top-paths 5")?.run(wasi, Trickle(log, 0)).await }).unwrap();
    assert_eq!(String::from_utf8(stdout.contents().to_vec()).unwrap(), expected);
}

#[test]
fn runs_are_deterministic() {
    let log = access_log();
    let a = run_with("parse-log | errors 404 | top-paths 20", log.clone());
    let b = run_with("parse-log | errors 404 | top-paths 20", log.clone());
    assert_eq!(a, b);
}
