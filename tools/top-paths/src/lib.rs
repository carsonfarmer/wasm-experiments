//! `top-paths`: print the `n` most requested paths; returns rows seen.
//!
//! It declares its input as `stream<path-only>` (just `{ path }`), so when it
//! reads `request` rows the host copies only the path of each row.

wit_bindgen::generate!({
    world: "top-paths",
    path: "../../wit",
});

use logparse::Top;

struct TopPaths;

impl Guest for TopPaths {
    async fn run(mut input: wit_bindgen::StreamReader<PathOnly>, n: Option<u32>) -> u64 {
        let mut top = Top::default();
        let mut rows = 0u64;
        let mut buf = Vec::with_capacity(8192);
        loop {
            let (_status, mut batch) = input.read(buf).await;
            if batch.is_empty() {
                break;
            }
            rows += batch.len() as u64;
            for r in batch.drain(..) {
                top.add_owned(r.path);
            }
            buf = batch;
        }
        print!("{}", top.report(n.unwrap_or(10) as usize));
        rows
    }
}

export!(TopPaths);
