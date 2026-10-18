//! Native baseline: the parsing and counting code the log tools run, compiled
//! into one native binary with no sandbox and no pipes.
//!
//!     native-logs [N] [MIN_STATUS] < access.log
//!
//! Prints exactly what `wp 'parse-log | [errors MIN_STATUS |] top-paths N'` prints.

use logparse::{Lines, Parser, Top};
use std::io::Read;

fn main() -> std::io::Result<()> {
    let args: Vec<u64> =
        std::env::args().skip(1).map(|a| a.parse().expect("usage: native-logs [N] [MIN_STATUS]")).collect();
    let n = args.first().map_or(10, |&n| n as usize);
    let min_status = args.get(1).map(|&s| s as u16);

    let (mut top, mut parser) = (Top::default(), Parser::default());
    let mut count = |line: &str| {
        if let Some(r) = parser.parse(line)
            && min_status.is_none_or(|m| r.status >= m)
        {
            top.add(r.path);
        }
    };
    let (mut stdin, mut buf, mut lines) = (std::io::stdin().lock(), vec![0u8; 1 << 20], Lines::default());
    loop {
        match stdin.read(&mut buf)? {
            0 => break,
            k => lines.feed(&buf[..k], &mut count),
        }
    }
    lines.finish(&mut count);
    print!("{}", top.report(n));
    Ok(())
}
