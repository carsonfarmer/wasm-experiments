//! `count-cols`: counts rows in columnar commit batches without printing.

wit_bindgen::generate!({
    world: "count-cols",
    path: "../../wit",
});

struct CountCols;

impl Guest for CountCols {
    async fn run(mut input: wit_bindgen::StreamReader<CommitBatch>) -> u64 {
        let mut seen = 0u64;
        while let Some(b) = input.next().await {
            seen += b.time.len() as u64;
        }
        seen
    }
}

export!(CountCols);
