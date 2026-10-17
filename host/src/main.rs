//! `wp`: run a pipeline of WebAssembly components connected by typed streams.
//!
//!     wp 'gen 5 | show'                        two components, one stream<commit>
//!     wp 'gen | take 10 | show'                `take` is a host built-in
//!     wp 'cat access.log | parse-log | errors | top-paths 10'
//!     wp --stats 'gen 1000000 | count'         timing, memory and pipe counters
//!     wp --serial '...'                        every stage in one Store on one thread
//!     wp --copy wasmtime '...'                 Wasmtime's built-in copy instead of the pump

use anyhow::{Context as _, Result, bail};
use serde::Serialize;
use std::alloc::System;
use wp::stats::{INSTRUMENTED_SYSTEM, Region, StatsAlloc, peak_rss};
use wp::{Copy, Options, PipeReport, Runner};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const USAGE: &str =
    "usage: wp [--stats | --json] [--serial] [--copy host|wasmtime] [--tools DIR] [--no-cache] 'gen 5 | take 3 | show'";

/// `--json` output.
#[derive(Serialize)]
struct Summary<'a> {
    parallel: bool,
    load_ms: f64,
    run_ms: f64,
    rows: Option<u64>,
    host_allocs: usize,
    host_alloc_bytes: usize,
    peak_rss: u64,
    guest_memory: u64,
    cat_bytes: u64,
    pipes: &'a [PipeReport],
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut opts = Options::default();
    if let Ok(dir) = std::env::var("WP_TOOLS") {
        opts.tools = dir.into();
    }
    let (mut stats, mut json, mut words) = (false, false, Vec::new());
    let mut argv = std::env::args().skip(1);
    while let Some(a) = argv.next() {
        match a.as_str() {
            "--stats" => stats = true,
            "--json" => json = true,
            "--serial" => opts.parallel = false,
            "--no-cache" => opts.cache = None,
            "--tools" => opts.tools = argv.next().context("--tools needs a directory")?.into(),
            "--copy" => {
                opts.copy = match argv.next().as_deref() {
                    Some("host") => Copy::Host,
                    Some("wasmtime") => Copy::Wasmtime,
                    _ => bail!("--copy takes `host` or `wasmtime`"),
                }
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            _ => words.push(a),
        }
    }
    if words.is_empty() {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }

    let region = Region::new(GLOBAL);
    let report = Runner::new(opts)?.run(&words.join(" ")).await?;
    let allocs = region.change();
    let summary = Summary {
        parallel: report.parallel,
        load_ms: report.load.as_secs_f64() * 1e3,
        run_ms: report.run.as_secs_f64() * 1e3,
        rows: report.count(),
        host_allocs: allocs.allocations + allocs.reallocations,
        host_alloc_bytes: allocs.bytes_allocated,
        peak_rss: peak_rss(),
        guest_memory: report.guest_memory,
        cat_bytes: report.cat_bytes,
        pipes: &report.pipes,
    };
    if json {
        eprintln!("{}", serde_json::to_string(&summary)?);
    } else if stats {
        print_stats(&summary);
    }
    Ok(())
}

fn print_stats(s: &Summary) {
    let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
    let secs = s.run_ms / 1e3;
    let mut line = format!(
        "{} · load {:.1} ms · run {:.1} ms",
        if s.parallel { "parallel" } else { "serial" },
        s.load_ms,
        s.run_ms
    );
    if let Some(r) = s.rows {
        line += &format!(" · {r} rows · {:.2} M rows/s", r as f64 / secs / 1e6);
    }
    if s.cat_bytes > 0 {
        line += &format!(" · {:.0} MB in at {:.0} MB/s", mb(s.cat_bytes), mb(s.cat_bytes) / secs);
    }
    eprintln!("{line}");
    eprintln!(
        "memory: peak RSS {:.1} MB · guest linear memory {:.1} MB · host allocations {} ({:.1} MB)",
        mb(s.peak_rss),
        mb(s.guest_memory),
        s.host_allocs,
        mb(s.host_alloc_bytes as u64)
    );
    for p in s.pipes {
        eprintln!(
            "pipe {}: {} elems · {} rows · {} batches · {:.1} MB staged · {} reallocs · copy in {:.1} ms, out {:.1} ms",
            p.label,
            p.elems,
            p.rows,
            p.batches,
            mb(p.bytes),
            p.reallocs,
            p.lift_ms,
            p.lower_ms
        );
    }
}
