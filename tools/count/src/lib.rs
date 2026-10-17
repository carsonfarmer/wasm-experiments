//! `count`: a consumer that counts commits without printing (for benchmarks).

wit_bindgen::generate!({
    world: "count",
    path: "../../wit",
});

struct Count;

impl Guest for Count {
    async fn run(mut input: wit_bindgen::StreamReader<Commit>, batch: Option<u32>) -> u64 {
        let mut seen = 0u64;
        let mut buf = Vec::with_capacity(batch.unwrap_or(256).max(1) as usize);
        loop {
            let (_status, batch) = input.read(buf).await;
            if batch.is_empty() {
                break;
            }
            seen += batch.len() as u64;
            buf = batch;
            buf.clear();
        }
        seen
    }
}

export!(Count);
