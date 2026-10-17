//! `gen-cols`: the same commits as `gen`, emitted as columnar batches.

wit_bindgen::generate!({
    world: "gen-cols",
    path: "../../wit",
});

use cols::StrColBuilder;
use std::fmt::Write as _;
use wp::demo::cols::StrCol;

struct GenCols;

const BATCH: u32 = 4096;
const AUTHORS: [&str; 4] = ["ada", "grace", "linus", "barbara"];

fn col(b: &mut StrColBuilder) -> StrCol {
    let (data, offsets) = b.finish();
    StrCol { data, offsets }
}

impl Guest for GenCols {
    async fn run(count: Option<u64>, batch: Option<u32>) -> wit_bindgen::StreamReader<CommitBatch> {
        let batch = u64::from(batch.unwrap_or(BATCH).max(1));
        let (mut tx, rx) = wit_stream::new();
        wit_bindgen::spawn_local(async move {
            let rows = batch as usize;
            let mut sha = StrColBuilder::with_capacity(rows, rows * 16);
            let mut author = StrColBuilder::with_capacity(rows, rows * 6);
            let mut message = StrColBuilder::with_capacity(rows, rows * 16);
            let mut scratch = String::with_capacity(32);
            let mut i = 0u64;
            while count.is_none_or(|c| i < c) {
                let n = count.map_or(batch, |c| batch.min(c - i));
                let mut time = Vec::with_capacity(n as usize);
                for k in i..i + n {
                    scratch.clear();
                    write!(scratch, "{:016x}", k.wrapping_mul(0x9e37_79b9_7f4a_7c15)).unwrap();
                    sha.push(&scratch);
                    author.push(AUTHORS[(k % 4) as usize]);
                    scratch.clear();
                    write!(scratch, "change #{k}").unwrap();
                    message.push(&scratch);
                    time.push(1_760_000_000 + k as i64 * 60);
                }
                let b = CommitBatch { sha: col(&mut sha), author: col(&mut author), message: col(&mut message), time };
                if tx.write_one(b).await.is_some() {
                    eprintln!("gen-cols: reader hung up after {i} commits; stopping");
                    return;
                }
                i += n;
            }
        });
        rx
    }
}

export!(GenCols);
