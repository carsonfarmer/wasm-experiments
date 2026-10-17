//! `gen`: a producer that emits synthetic commits as fast as the reader takes them.

wit_bindgen::generate!({
    world: "gen",
    path: "../../wit",
});

struct Gen;

/// Default commits per stream write. Batching amortizes the per-write cost.
const BATCH: u32 = 256;

impl Guest for Gen {
    async fn run(count: Option<u64>, batch: Option<u32>) -> wit_bindgen::StreamReader<Commit> {
        let batch = u64::from(batch.unwrap_or(BATCH).max(1));
        let (mut tx, rx) = wit_stream::new();
        // Keep producing after returning the reader; the host drives this task.
        wit_bindgen::spawn_local(async move {
            let forever = count.is_none();
            let count = count.unwrap_or(0);
            let mut i = 0u64;
            while forever || i < count {
                let n = if forever { batch } else { batch.min(count - i) };
                let batch: Vec<Commit> = (i..i + n).map(commit).collect();
                let unsent = tx.write_all(batch).await;
                if !unsent.is_empty() {
                    // The reader hung up (e.g. `take` got enough): stop producing.
                    let sent = i + n - unsent.len() as u64;
                    eprintln!("gen: reader hung up after {sent} commits; stopping");
                    return;
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
        author: AUTHORS[(i % AUTHORS.len() as u64) as usize].to_string(),
        message: format!("change #{i}"),
        time: 1_760_000_000 + i as i64 * 60,
    }
}

const AUTHORS: [&str; 4] = ["ada", "grace", "linus", "barbara"];

export!(Gen);
