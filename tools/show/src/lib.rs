//! `show`: prints commits, one per line, stopping after `limit` if given.
//! Stopping early drops the stream, which cancels the producer.

wit_bindgen::generate!({ world: "show", path: "../../wit" });

use std::io::Write;

struct Show;

impl Guest for Show {
    async fn run(mut input: wit_bindgen::StreamReader<Commit>, limit: Option<u64>) {
        let mut out = std::io::BufWriter::new(std::io::stdout().lock());
        let mut left = limit.unwrap_or(u64::MAX);
        while left > 0
            && let Some(c) = input.next().await
        {
            let _ = writeln!(out, "{}  {:<8} {}", &c.sha[..10], c.author, c.message);
            left -= 1;
        }
        let _ = out.flush();
    }
}

export!(Show);
