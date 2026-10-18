//! `count`: counts commits, reading `batch` at a time.

wit_bindgen::generate!({ world: "count", path: "../../wit" });

struct Count;

impl Guest for Count {
    async fn run(mut input: wit_bindgen::StreamReader<Commit>, batch: Option<u32>) -> u64 {
        let mut seen = 0;
        let mut buf = Vec::with_capacity(batch.unwrap_or(256).max(1) as usize);
        loop {
            let (_, rows) = input.read(buf).await;
            if rows.is_empty() {
                // A read with room to spare waits for at least one row, so an
                // empty one means the writer is done.
                return seen;
            }
            seen += rows.len() as u64;
            buf = rows;
            buf.clear();
        }
    }
}

export!(Count);
