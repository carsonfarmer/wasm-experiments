//! `fruit`: a producer with a differently shaped row (lists, options, enums),
//! to show the host's built-ins aren't tied to any one type.

wit_bindgen::generate!({
    world: "fruit",
    path: "../../wit",
});

use wp::demo::produce::Ripeness;

struct FruitStand;

impl Guest for FruitStand {
    async fn run(count: Option<u32>) -> wit_bindgen::StreamReader<Fruit> {
        let (mut tx, rx) = wit_stream::new();
        wit_bindgen::spawn_local(async move {
            let mut i = 0u32;
            while count.is_none_or(|c| i < c) {
                let (name, tags) = KINDS[i as usize % KINDS.len()];
                let fruit = Fruit {
                    name: format!("{name}-{i}"),
                    tags: tags.iter().map(|t| t.to_string()).collect(),
                    grams: (!i.is_multiple_of(3)).then(|| 80.0 + (i as f64 * 7.5) % 120.0),
                    ripeness: [Ripeness::Green, Ripeness::Ripe, Ripeness::Overripe][i as usize % 3],
                };
                if tx.write_one(fruit).await.is_some() {
                    eprintln!("fruit: reader hung up after {i} fruit; stopping");
                    return;
                }
                i += 1;
            }
        });
        rx
    }
}

const KINDS: [(&str, &[&str]); 3] =
    [("apple", &["crisp", "red"]), ("banana", &["soft"]), ("kiwi", &["fuzzy", "green", "tart"])];

export!(FruitStand);
