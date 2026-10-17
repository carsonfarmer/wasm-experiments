//! Shared helpers for the integration tests. The components must be built
//! first: `cargo tools`.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::OnceLock;
use wp::{Copy, Options, Report, Runner};

/// The three ways rows can move between components.
#[derive(Clone, Copy, Debug)]
pub enum Engine {
    /// Wasmtime's built-in guest-to-guest copy, one Store.
    Wasmtime,
    /// The host pump, one Store.
    Pump,
    /// The host pump, one Store and thread per stage.
    Parallel,
}

pub const ENGINES: [Engine; 3] = [Engine::Wasmtime, Engine::Pump, Engine::Parallel];

pub fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

pub fn tools() -> PathBuf {
    let dir = std::env::var("WP_TOOLS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| workspace().join("target/wasm32-wasip2/release"));
    assert!(
        dir.join("gen.wasm").exists(),
        "components not built: run `cargo tools` first (looked in {})",
        dir.display()
    );
    dir
}

pub fn runner(engine: Engine) -> Runner {
    Runner::new(Options {
        tools: tools(),
        copy: match engine {
            Engine::Wasmtime => Copy::Wasmtime,
            _ => Copy::Host,
        },
        parallel: matches!(engine, Engine::Parallel),
        capture: true,
        cache: Some(workspace().join("target/wp-cache")),
    })
    .unwrap()
}

/// Run a pipeline to completion on a fresh runtime.
pub fn run(engine: Engine, pipeline: &str) -> anyhow::Result<Report> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(runner(engine).run(pipeline))
}

pub fn ok(engine: Engine, pipeline: &str) -> Report {
    run(engine, pipeline).unwrap_or_else(|e| panic!("{engine:?}: `{pipeline}` failed: {e:#}"))
}

pub fn stdout(r: &Report) -> String {
    String::from_utf8(r.stdout.clone()).unwrap()
}

pub fn stderr(r: &Report) -> String {
    String::from_utf8(r.stderr.clone()).unwrap()
}

pub fn err(engine: Engine, pipeline: &str) -> String {
    match run(engine, pipeline) {
        Ok(_) => panic!("{engine:?}: `{pipeline}` should have failed"),
        Err(e) => format!("{e:#}"),
    }
}

/// A generated access log, shared by every test that needs one.
pub fn access_log() -> &'static PathBuf {
    static LOG: OnceLock<PathBuf> = OnceLock::new();
    LOG.get_or_init(|| {
        let dir = workspace().join("target/test-data");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("access-60000.log");
        if !path.exists() {
            let tmp = dir.join(format!("access-60000.{}.tmp", std::process::id()));
            loggen::generate(60_000, 7, std::io::BufWriter::new(std::fs::File::create(&tmp).unwrap())).unwrap();
            std::fs::rename(&tmp, &path).unwrap();
        }
        path
    })
}

/// What `top-paths` should print for `log`, computed natively.
pub fn expected_top(log: &PathBuf, n: usize, min_status: Option<u16>) -> (String, u64) {
    let text = std::fs::read_to_string(log).unwrap();
    let (mut top, mut parser, mut rows) = (logparse::Top::default(), logparse::Parser::default(), 0);
    for line in text.lines() {
        if let Some(r) = parser.parse(line)
            && min_status.is_none_or(|m| r.status >= m)
        {
            top.add(r.path);
            rows += 1;
        }
    }
    (top.report(n), rows)
}
