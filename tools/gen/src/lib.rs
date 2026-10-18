//! `gen`: emits synthetic commits, `batch` per write.

wit_bindgen::generate!({ world: "gen", path: "../../wit" });

struct Gen;

impl Guest for Gen {
    async fn run(count: Option<u64>, batch: Option<u32>) -> wit_bindgen::StreamReader<Commit> {
        let batch = u64::from(batch.unwrap_or(256).max(1));
        let (mut tx, rx) = wit_stream::new();
        // Keep producing after returning the stream; the host drives this task.
        wit_bindgen::spawn_local(async move {
            let mut i = 0;
            while count.is_none_or(|c| i < c) {
                let n = count.map_or(batch, |c| batch.min(c - i));
                if !tx.write_all((i..i + n).map(commit).collect()).await.is_empty() {
                    return; // the reader stopped reading
                }
                i += n;
            }
        });
        rx
    }
}

fn commit(i: u64) -> Commit {
    Commit {
        sha: format!("{:016x}", i.wrapping_mul(0x9e37_79b9_7f4a_7c15)),
        author: ["ada", "grace", "linus", "barbara"][i as usize % 4].to_string(),
        message: format!("change #{i}"),
        time: 1_760_000_000 + i as i64 * 60,
    }
}

export!(Gen);
