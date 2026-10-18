use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;
use wasmtime_wasi::WasiCtx;
use wp::Wp;

#[cfg(feature = "stats")]
#[global_allocator]
static ALLOC: &stats_alloc::StatsAlloc<std::alloc::System> = &stats_alloc::INSTRUMENTED_SYSTEM;

/// Run a pipeline of WebAssembly components connected by typed streams.
///
/// Example: wp 'parse-log | errors 500 | top-paths 10' < access.log
#[derive(Parser)]
struct Cli {
    /// The pipeline: tools separated by `|`, each followed by its arguments.
    #[arg(required_unless_present = "list")]
    pipeline: Vec<String>,
    /// Directories to find tools (NAME.wasm) in, separated by `:`.
    #[arg(long, env = "WP_PATH", value_delimiter = ':', default_value = ".")]
    path: Vec<PathBuf>,
    /// List the available tools and their signatures.
    #[arg(long)]
    list: bool,
    /// Print timings (and, with the `stats` feature, host allocations) as JSON to stderr.
    #[arg(long)]
    stats: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("wp: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    let wp = Wp::new(cli.path)?;
    if cli.list {
        for tool in wp.tools()? {
            println!("{tool}");
        }
        return Ok(());
    }
    #[cfg(feature = "stats")]
    let allocs = stats_alloc::Region::new(ALLOC);
    let start = Instant::now();
    let pipeline = wp.pipeline(&cli.pipeline.join(" "))?;
    let loaded = Instant::now();
    let wasi = WasiCtx::builder().inherit_stdout().inherit_stderr().build();
    let result = pipeline.run(wasi, tokio::io::stdin()).await?;
    let done = Instant::now();
    if let Some(value) = result {
        println!("{}", value.to_wave()?);
    }
    if cli.stats {
        #[allow(unused_mut)]
        let mut stats = serde_json::json!({
            "load_ms": (loaded - start).as_secs_f64() * 1e3,
            "run_ms": (done - loaded).as_secs_f64() * 1e3,
        });
        #[cfg(feature = "stats")]
        {
            let a = allocs.change();
            stats["allocs"] = (a.allocations + a.reallocations).into();
            stats["alloc_bytes"] = a.bytes_allocated.into();
        }
        eprintln!("{stats}");
    }
    Ok(())
}
