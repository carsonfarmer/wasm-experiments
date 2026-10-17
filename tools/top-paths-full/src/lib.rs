//! `top-paths-full`: `top-paths`, but declaring whole `request` rows, so the
//! host copies every field (for comparison with the projected `top-paths`).

wit_bindgen::generate!({
    world: "top-paths-full",
    path: "../../wit",
});

use logparse::Top;

struct TopPathsFull;

impl Guest for TopPathsFull {
    async fn run(mut input: wit_bindgen::StreamReader<Request>, n: Option<u32>) -> u64 {
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

export!(TopPathsFull);
