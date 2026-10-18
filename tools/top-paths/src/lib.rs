//! `top-paths`: prints the `n` most requested paths (default 10).

wit_bindgen::generate!({ world: "top-paths", path: "../../wit" });

struct TopPaths;

impl Guest for TopPaths {
    async fn run(mut input: wit_bindgen::StreamReader<Request>, n: Option<u32>) {
        let mut top = logparse::Top::default();
        loop {
            let (_, rows) = input.read(Vec::with_capacity(8192)).await;
            if rows.is_empty() {
                break;
            }
            rows.into_iter().for_each(|r| top.add_owned(r.path));
        }
        print!("{}", top.report(n.unwrap_or(10) as usize));
    }
}

export!(TopPaths);
