//! `top-paths-cols`: columnar `top-paths`. Declares `stream<path-batch>`, so
//! the host hands it only the `path` column of each batch.

wit_bindgen::generate!({
    world: "top-paths-cols",
    path: "../../wit",
});

use logparse::Top;

struct TopPathsCols;

impl Guest for TopPathsCols {
    async fn run(mut input: wit_bindgen::StreamReader<PathBatch>, n: Option<u32>) -> u64 {
        let mut top = Top::default();
        let mut rows = 0u64;
        while let Some(b) = input.next().await {
            let paths = &b.path;
            for i in 0..cols::rows(&paths.offsets) {
                top.add(cols::get(&paths.data, &paths.offsets, i));
            }
            rows += cols::rows(&paths.offsets) as u64;
        }
        print!("{}", top.report(n.unwrap_or(10) as usize));
        rows
    }
}

export!(TopPathsCols);
