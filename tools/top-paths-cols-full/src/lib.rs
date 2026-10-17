//! `top-paths-cols-full`: columnar `top-paths`, declaring whole batches (for comparison).

wit_bindgen::generate!({
    world: "top-paths-cols-full",
    path: "../../wit",
});

use logparse::Top;

struct TopPathsColsFull;

impl Guest for TopPathsColsFull {
    async fn run(mut input: wit_bindgen::StreamReader<RequestBatch>, n: Option<u32>) -> u64 {
        let mut top = Top::default();
        let mut rows = 0u64;
        while let Some(b) = input.next().await {
            let paths = &b.path;
            for i in 0..cols::rows(&paths.offsets) {
                top.add(cols::get(&paths.data, &paths.offsets, i));
            }
            rows += b.status.len() as u64;
        }
        print!("{}", top.report(n.unwrap_or(10) as usize));
        rows
    }
}

export!(TopPathsColsFull);
