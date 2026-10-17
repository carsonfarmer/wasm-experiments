//! `errors`: keep only requests with status >= `min-status` (default 500).

wit_bindgen::generate!({
    world: "errors",
    path: "../../wit",
});

struct Errors;

impl Guest for Errors {
    async fn run(
        mut input: wit_bindgen::StreamReader<Request>,
        min_status: Option<u16>,
    ) -> wit_bindgen::StreamReader<Request> {
        let min = min_status.unwrap_or(500);
        let (mut tx, rx) = wit_stream::new();
        wit_bindgen::spawn_local(async move {
            let mut buf = Vec::with_capacity(8192);
            loop {
                let (_status, mut batch) = input.read(buf).await;
                if batch.is_empty() {
                    break;
                }
                batch.retain(|r| r.status >= min);
                let cap = batch.capacity();
                if !batch.is_empty() && !tx.write_all(batch).await.is_empty() {
                    return;
                }
                buf = Vec::with_capacity(cap);
            }
        });
        rx
    }
}

export!(Errors);
