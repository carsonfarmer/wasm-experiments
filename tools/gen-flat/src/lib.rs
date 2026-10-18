//! `gen-flat`: emits synthetic ticks (plain numbers, no strings), `batch` per write.

wit_bindgen::generate!({ world: "gen-flat", path: "../../wit" });

struct GenFlat;

impl Guest for GenFlat {
    async fn run(count: Option<u64>, batch: Option<u32>) -> wit_bindgen::StreamReader<Tick> {
        let batch = u64::from(batch.unwrap_or(256).max(1));
        let (mut tx, rx) = wit_stream::new();
        wit_bindgen::spawn_local(async move {
            let mut i = 0;
            while count.is_none_or(|c| i < c) {
                let n = count.map_or(batch, |c| batch.min(c - i));
                let ticks = (i..i + n).map(|i| Tick {
                    seq: i,
                    time: 1_760_000_000 + i as i64,
                    value: i as f64 * 0.25,
                    mask: i as u32,
                });
                if !tx.write_all(ticks.collect()).await.is_empty() {
                    return;
                }
                i += n;
            }
        });
        rx
    }
}

export!(GenFlat);
