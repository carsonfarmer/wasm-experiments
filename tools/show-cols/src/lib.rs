//! `show-cols`: prints columnar commit batches exactly the way `show` prints rows.

wit_bindgen::generate!({
    world: "show-cols",
    path: "../../wit",
});

use std::io::Write;

struct ShowCols;

impl Guest for ShowCols {
    async fn run(mut input: wit_bindgen::StreamReader<CommitBatch>) -> u64 {
        let mut seen = 0u64;
        let mut out = std::io::BufWriter::new(std::io::stdout().lock());
        while let Some(b) = input.next().await {
            for i in 0..b.time.len() {
                let sha = cols::get(&b.sha.data, &b.sha.offsets, i);
                let author = cols::get(&b.author.data, &b.author.offsets, i);
                let message = cols::get(&b.message.data, &b.message.offsets, i);
                let _ = writeln!(out, "{}  {:<8} {}", &sha[..10.min(sha.len())], author, message);
            }
            seen += b.time.len() as u64;
        }
        let _ = out.flush();
        seen
    }
}

export!(ShowCols);
