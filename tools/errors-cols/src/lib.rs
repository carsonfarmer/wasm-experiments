//! `errors-cols`: columnar `errors` — keeps rows with status >= `min-status`.

wit_bindgen::generate!({
    world: "errors-cols",
    path: "../../wit",
});

use cols::StrColBuilder;
use wp::demo::cols::StrCol;

struct ErrorsCols;

/// Copy the selected rows of a string column into a new column.
fn select(col: &StrCol, keep: &[usize]) -> StrCol {
    let mut b = StrColBuilder::with_capacity(keep.len(), keep.len() * 32);
    for &i in keep {
        b.push(cols::get(&col.data, &col.offsets, i));
    }
    let (data, offsets) = b.finish();
    StrCol { data, offsets }
}

fn pick<T: Copy>(v: &[T], keep: &[usize]) -> Vec<T> {
    keep.iter().map(|&i| v[i]).collect()
}

impl Guest for ErrorsCols {
    async fn run(
        mut input: wit_bindgen::StreamReader<RequestBatch>,
        min_status: Option<u16>,
    ) -> wit_bindgen::StreamReader<RequestBatch> {
        let min = min_status.unwrap_or(500);
        let (mut tx, rx) = wit_stream::new();
        wit_bindgen::spawn_local(async move {
            let mut keep = Vec::new();
            while let Some(b) = input.next().await {
                keep.clear();
                keep.extend(b.status.iter().enumerate().filter(|(_, s)| **s >= min).map(|(i, _)| i));
                if keep.is_empty() {
                    continue;
                }
                let out = if keep.len() == b.status.len() {
                    b
                } else {
                    RequestBatch {
                        ip: select(&b.ip, &keep),
                        time: pick(&b.time, &keep),
                        method: keep.iter().map(|&i| b.method[i]).collect(),
                        path: select(&b.path, &keep),
                        status: pick(&b.status, &keep),
                        bytes: pick(&b.bytes, &keep),
                        referrer: select(&b.referrer, &keep),
                        agent: select(&b.agent, &keep),
                    }
                };
                if tx.write_one(out).await.is_some() {
                    return;
                }
            }
        });
        rx
    }
}

export!(ErrorsCols);
