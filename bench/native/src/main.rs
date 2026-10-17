//! Native baseline: the same parsing and counting code the log components
//! run, compiled to a single native binary with no sandbox and no pipes.
//!
//!     native-logs top-paths FILE [N] [MIN_STATUS]
//!
//! Prints exactly what `cat FILE | parse-log | [errors MIN_STATUS |] top-paths N` prints.

use logparse::{Lines, Top, parse};
use std::io::Read;

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 || args[1] != "top-paths" {
        eprintln!("usage: native-logs top-paths FILE [N] [MIN_STATUS]");
        std::process::exit(2);
    }
    let n: usize = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(10);
    let min_status: Option<u16> = args.get(4).map(|s| s.parse().unwrap());

    let mut file = std::fs::File::open(&args[2])?;
    let mut buf = vec![0u8; 1 << 20];
    let mut lines = Lines::default();
    let mut top = Top::default();
    let mut rows = 0u64;
    loop {
        let k = file.read(&mut buf)?;
        if k == 0 {
            break;
        }
        lines.feed(&buf[..k], |l| {
            if let Some(r) = parse(l)
                && min_status.is_none_or(|m| r.status >= m)
            {
                rows += 1;
                top.add(r.path);
            }
        });
    }
    lines.finish(|l| {
        if let Some(r) = parse(l)
            && min_status.is_none_or(|m| r.status >= m)
        {
            rows += 1;
            top.add(r.path);
        }
    });
    print!("{}", top.report(n));
    eprintln!("{rows} rows");
    Ok(())
}
