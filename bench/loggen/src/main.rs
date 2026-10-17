//! Generate a realistic, deterministic web access log (Combined Log Format).
//!
//!     loggen LINES OUT.log [SEED]

use std::io::BufWriter;

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: loggen LINES OUT.log [SEED]");
        std::process::exit(2);
    }
    let lines: u64 = args[1].parse().expect("LINES must be a number");
    let seed: u64 = args.get(3).map(|s| s.parse().expect("SEED must be a number")).unwrap_or(42);
    let out = BufWriter::with_capacity(1 << 20, std::fs::File::create(&args[2])?);
    loggen::generate(lines, seed, out)
}
