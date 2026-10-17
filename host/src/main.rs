//! `wp`: run a pipeline of WebAssembly components connected by typed streams.
//!
//!     wp 'gen 5 | show'                        two components, one stream<commit>
//!     wp 'gen | take 10 | show'                `take` is a host built-in
//!     wp 'cat access.log | parse-log | errors | top-paths 10'
//!     wp --stats 'gen 1000000 | count'         timing, memory and pipe counters
//!     wp --serial '...'                        run every stage in one Store on one thread
//!     wp --copy wasmtime '...'                 use Wasmtime's built-in copy instead of the pump (implies --serial)

use anyhow::{Context as _, Result, bail};
use std::path::PathBuf;
use wp::stats::{Allocs, CountingAlloc, peak_rss};
use wp::{Copy, Options, Runner};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

const USAGE: &str = "usage: wp [--stats | --json] [--serial] [--copy host|wasmtime] [--tools DIR] [--no-cache] 'gen 5 | take 3 | show'";

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut opts = Options::default();
    if let Ok(dir) = std::env::var("WP_TOOLS") {
        opts.tools = PathBuf::from(dir);
    }
    let (mut stats, mut json) = (false, false);
    let mut words = Vec::new();
    let mut argv = std::env::args().skip(1);
    while let Some(a) = argv.next() {
        match a.as_str() {
            "--stats" | "--time" => stats = true,
            "--json" => json = true,
            "--no-cache" => opts.cache = None,
            "--serial" => opts.parallel = false,
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

    let before = Allocs::now();
    let mut runner = Runner::new(opts)?;
    let report = runner.run(&words.join(" ")).await?;
    let allocs = Allocs::now().since(before);
    let rss = peak_rss().unwrap_or(0);

    let secs = report.run.as_secs_f64();
    let rows = report.count();
    if json {
        let pipes: Vec<String> = report
            .pipes
            .iter()
            .map(|p| {
                format!(
                    r#"{{"label":{:?},"how":{:?},"elems":{},"rows":{},"batches":{},"bytes":{},"reallocs":{},"lift_ms":{:.3},"lower_ms":{:.3}}}"#,
                    p.label,
                    p.how,
                    p.elems,
                    p.rows,
                    p.batches,
                    p.bytes,
                    p.reallocs,
                    p.lift_ns as f64 / 1e6,
                    p.lower_ns as f64 / 1e6
                )
            })
            .collect();
        eprintln!(
            r#"{{"parallel":{},"load_ms":{:.3},"run_ms":{:.3},"rows":{},"host_allocs":{},"host_alloc_bytes":{},"peak_rss":{},"guest_memory":{},"cat_bytes":{},"pipes":[{}]}}"#,
            report.parallel,
            report.load.as_secs_f64() * 1e3,
            secs * 1e3,
            rows.map(|r| r.to_string()).unwrap_or_else(|| "null".into()),
            allocs.count,
            allocs.bytes,
            rss,
            report.guest_memory,
            report.cat_bytes,
            pipes.join(",")
        );
    } else if stats {
        let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
        eprint!(
            "{} · load {:.1} ms · run {:.1} ms",
            if report.parallel { "parallel" } else { "serial" },
            report.load.as_secs_f64() * 1e3,
            secs * 1e3
        );
        if let Some(r) = rows {
            eprint!(" · {r} rows · {:.2} M rows/s", r as f64 / secs / 1e6);
        }
        if report.cat_bytes > 0 {
            eprint!(" · {:.0} MB in at {:.0} MB/s", mb(report.cat_bytes), mb(report.cat_bytes) / secs);
        }
        eprintln!();
        eprintln!(
            "memory: peak RSS {:.1} MB · guest linear memory {:.1} MB · host allocations {} ({:.1} MB)",
            mb(rss),
            mb(report.guest_memory),
            allocs.count,
            mb(allocs.bytes)
        );
        for p in &report.pipes {
            eprintln!(
                "pipe {:<24} {} elems · {} rows · {} batches · {:.1} MB staged · {} reallocs · copy in {:.1} ms, out {:.1} ms",
                p.label,
                p.elems,
                p.rows,
                p.batches,
                mb(p.bytes),
                p.reallocs,
                p.lift_ns as f64 / 1e6,
                p.lower_ns as f64 / 1e6
            );
        }
    }
    Ok(())
}
