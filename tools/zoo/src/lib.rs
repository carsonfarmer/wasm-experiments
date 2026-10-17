//! `zoo`: emits `count` deterministic rows exercising every value kind the
//! host pump copies (see `zoo-common/rows.rs`).

wit_bindgen::generate!({
    world: "zoo",
    path: "../../wit",
    additional_derives: [PartialEq],
});

include!("../../zoo-common/rows.rs");

struct Zoo;

impl Guest for Zoo {
    async fn run(count: u64, seed: u64) -> wit_bindgen::StreamReader<Row> {
        let (mut tx, rx) = wit_stream::new();
        wit_bindgen::spawn_local(async move {
            let mut i = 0;
            while i < count {
                // Odd batch sizes so rows straddle every kind of boundary.
                let n = (37 + i % 50).min(count - i);
                let batch: Vec<Row> = (i..i + n).map(|k| row(seed, k)).collect();
                if !tx.write_all(batch).await.is_empty() {
                    return;
                }
                i += n;
            }
        });
        rx
    }
}

export!(Zoo);
