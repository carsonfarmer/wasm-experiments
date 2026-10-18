//! `errors`: keeps requests whose status is at least `min-status` (default 500).

wit_bindgen::generate!({ world: "errors", path: "../../wit" });

struct Errors;

impl Guest for Errors {
    async fn run(
        mut input: wit_bindgen::StreamReader<Request>,
        min_status: Option<u16>,
    ) -> wit_bindgen::StreamReader<Request> {
        let min = min_status.unwrap_or(500);
        let (mut tx, rx) = wit_stream::new();
        wit_bindgen::spawn_local(async move {
            loop {
                let (_, mut rows) = input.read(Vec::with_capacity(8192)).await;
                if rows.is_empty() {
                    return; // the writer is done
                }
                rows.retain(|r| r.status >= min);
                if !rows.is_empty() && !tx.write_all(rows).await.is_empty() {
                    return; // the reader stopped reading
                }
            }
        });
        rx
    }
}

export!(Errors);
