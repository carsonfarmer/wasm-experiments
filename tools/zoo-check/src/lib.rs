//! `zoo-check`: regenerates the rows `zoo` should have sent and compares each
//! one received; returns how many matched (and reports the first mismatch).

wit_bindgen::generate!({
    world: "zoo-check",
    path: "../../wit",
    additional_derives: [PartialEq],
});

include!("../../zoo-common/rows.rs");

struct ZooCheck;

impl Guest for ZooCheck {
    async fn run(mut input: wit_bindgen::StreamReader<Row>, count: u64, seed: u64) -> u64 {
        let mut i = 0u64;
        let mut matched = 0u64;
        let mut reported = false;
        while let Some(got) = input.next().await {
            let want = row(seed, i);
            if got == want {
                matched += 1;
            } else if !reported {
                eprintln!("zoo-check: row {i} differs\n  got:  {got:?}\n  want: {want:?}");
                reported = true;
            }
            i += 1;
        }
        if i != count {
            eprintln!("zoo-check: received {i} rows, expected {count}");
        }
        matched
    }
}

export!(ZooCheck);
