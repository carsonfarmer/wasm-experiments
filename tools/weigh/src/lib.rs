//! `weigh`: a consumer that prints each fruit.

wit_bindgen::generate!({
    world: "weigh",
    path: "../../wit",
});

use std::io::Write;
use wp::demo::produce::Ripeness;

struct Weigh;

impl Guest for Weigh {
    async fn run(mut input: wit_bindgen::StreamReader<Fruit>) -> u64 {
        let mut seen = 0u64;
        let mut out = std::io::stdout().lock();
        while let Some(f) = input.next().await {
            let grams = f.grams.map(|g| format!("{g:.1} g")).unwrap_or_else(|| "?".into());
            let _ = writeln!(
                out,
                "{:<10} {:>8}  {:<9} [{}]",
                f.name,
                grams,
                match f.ripeness {
                    Ripeness::Green => "green",
                    Ripeness::Ripe => "ripe",
                    Ripeness::Overripe => "overripe",
                },
                f.tags.join(", ")
            );
            seen += 1;
        }
        let _ = out.flush();
        seen
    }
}

export!(Weigh);
