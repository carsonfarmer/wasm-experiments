//! `count-flat`: counts ticks, reading `batch` at a time.

wit_bindgen::generate!({ world: "count-flat", path: "../../wit" });

struct CountFlat;

impl Guest for CountFlat {
    async fn run(mut input: wit_bindgen::StreamReader<Tick>, batch: Option<u32>) -> u64 {
        let mut seen = 0;
        let mut buf = Vec::with_capacity(batch.unwrap_or(256).max(1) as usize);
        loop {
            let (_, rows) = input.read(buf).await;
            if rows.is_empty() {
                return seen;
            }
            seen += rows.len() as u64;
            buf = rows;
            buf.clear();
        }
    }
}

export!(CountFlat);
