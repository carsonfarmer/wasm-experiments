//! Helpers for the end-to-end tests. Build the example tools first:
//! `cargo tools` (`WP_PATH` points at them; see `.cargo/config.toml`).

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::OnceLock;
use wasmtime::component::Val;
use wasmtime_wasi::WasiCtx;
use wasmtime_wasi::p2::pipe::MemoryOutputPipe;
use wp::Wp;

pub fn tools_dir() -> PathBuf {
    let dir = PathBuf::from(std::env::var("WP_PATH").expect("WP_PATH is set in .cargo/config.toml"));
    assert!(dir.join("gen.wasm").exists(), "build the tools first: cargo tools");
    dir
}

pub fn wp() -> Wp {
    Wp::new(vec![tools_dir()]).unwrap()
}

/// What a pipeline printed and returned.
#[derive(Debug, PartialEq)]
pub struct Out {
    pub result: Option<Val>,
    pub stdout: String,
    pub stderr: String,
}

pub fn run(pipeline: &str) -> Out {
    run_with(pipeline, Vec::new())
}

/// Run `pipeline` with `input` as its input bytes.
pub fn run_with(pipeline: &str, input: Vec<u8>) -> Out {
    try_run(pipeline, input).unwrap_or_else(|e| panic!("`{pipeline}` failed: {e:#}"))
}

pub fn try_run(pipeline: &str, input: Vec<u8>) -> anyhow::Result<Out> {
    let (stdout, stderr) = (MemoryOutputPipe::new(usize::MAX), MemoryOutputPipe::new(usize::MAX));
    let wasi = WasiCtx::builder().stdout(stdout.clone()).stderr(stderr.clone()).build();
    let wp = wp();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let result = rt.block_on(async { wp.pipeline(pipeline)?.run(wasi, std::io::Cursor::new(input)).await })?;
    let text = |p: MemoryOutputPipe| String::from_utf8(p.contents().to_vec()).unwrap();
    Ok(Out { result, stdout: text(stdout), stderr: text(stderr) })
}

/// The error a pipeline fails with, before or while running.
pub fn error(pipeline: &str) -> String {
    match try_run(pipeline, Vec::new()) {
        Ok(out) => panic!("`{pipeline}` should have failed, but returned {out:?}"),
        Err(e) => format!("{e:#}"),
    }
}

pub fn u64(out: &Out) -> u64 {
    match out.result {
        Some(Val::U64(n)) => n,
        ref other => panic!("expected a u64 result, got {other:?}"),
    }
}

/// A generated access log, shared by the tests that need one.
pub fn access_log() -> &'static Vec<u8> {
    static LOG: OnceLock<Vec<u8>> = OnceLock::new();
    LOG.get_or_init(|| {
        let mut log = Vec::new();
        loggen::generate(60_000, 7, &mut log).unwrap();
        log
    })
}

/// What `top-paths n` prints for `log`, computed natively.
pub fn expected_top(log: &[u8], n: usize, min_status: Option<u16>) -> String {
    let (mut top, mut parser) = (logparse::Top::default(), logparse::Parser::default());
    for line in std::str::from_utf8(log).unwrap().lines() {
        if let Some(r) = parser.parse(line)
            && min_status.is_none_or(|m| r.status >= m)
        {
            top.add(r.path);
        }
    }
    top.report(n)
}
