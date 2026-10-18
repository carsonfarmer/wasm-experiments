//! `zoo`: emits `count` deterministic rows that exercise every kind of WIT
//! value (see `zoo-common/rows.rs`), in odd-sized batches.

wit_bindgen::generate!({ world: "zoo", path: "../../wit", additional_derives: [PartialEq] });

include!("../../zoo-common/rows.rs");

struct Zoo;

impl Guest for Zoo {
    async fn run(count: u64, seed: u64) -> wit_bindgen::StreamReader<Row> {
        let (mut tx, rx) = wit_stream::new();
        wit_bindgen::spawn_local(async move {
            let mut i = 0;
            while i < count {
                let n = (37 + i % 50).min(count - i);
                if !tx.write_all((i..i + n).map(|k| row(seed, k)).collect()).await.is_empty() {
                    return;
                }
                i += n;
            }
        });
        rx
    }
}

export!(Zoo);
