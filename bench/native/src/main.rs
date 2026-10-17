//! Native baseline: the same parsing and counting code the log components
//! run, compiled into one native binary with no sandbox and no pipes.
//!
//!     native-logs top-paths FILE [N] [MIN_STATUS]
//!
//! Prints exactly what `cat FILE | parse-log | [errors MIN_STATUS |] top-paths N` prints.

use logparse::{Lines, Parser, Top};
use std::io::Read;

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 || args[1] != "top-paths" {
        eprintln!("usage: native-logs top-paths FILE [N] [MIN_STATUS]");
        std::process::exit(2);
    }
    let n: usize = args.get(3).map_or(10, |s| s.parse().unwrap());
    let min_status: Option<u16> = args.get(4).map(|s| s.parse().unwrap());

    let (mut top, mut parser, mut rows) = (Top::default(), Parser::default(), 0u64);
    let mut count = |line: &str| {
        if let Some(r) = parser.parse(line)
            && min_status.is_none_or(|m| r.status >= m)
        {
            rows += 1;
            top.add(r.path);
        }
    };
    let (mut file, mut buf, mut lines) = (std::fs::File::open(&args[2])?, vec![0u8; 1 << 20], Lines::default());
    loop {
        match file.read(&mut buf)? {
            0 => break,
            k => lines.feed(&buf[..k], &mut count),
        }
    }
    lines.finish(&mut count);
    print!("{}", top.report(n));
    eprintln!("{rows} rows");
    Ok(())
}
