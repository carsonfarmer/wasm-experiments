//! `show`: a consumer that prints each commit, one per line.

wit_bindgen::generate!({
    world: "show",
    path: "../../wit",
});

use std::io::Write;

struct Show;

impl Guest for Show {
    async fn run(mut input: wit_bindgen::StreamReader<Commit>) -> u64 {
        let mut seen = 0u64;
        let mut out = std::io::stdout().lock();
        loop {
            let (_status, batch) = input.read(Vec::with_capacity(256)).await;
            if batch.is_empty() {
                break;
            }
            for c in &batch {
                let _ = writeln!(out, "{}  {:<8} {}", &c.sha[..10], c.author, c.message);
            }
            seen += batch.len() as u64;
        }
        let _ = out.flush();
        seen
    }
}

export!(Show);
