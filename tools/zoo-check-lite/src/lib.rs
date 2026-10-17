//! `zoo-check-lite`: like `zoo-check`, but declares only some of each row's
//! fields (in a different order), so the host must project them.

wit_bindgen::generate!({
    world: "zoo-check-lite",
    path: "../../wit",
    additional_derives: [PartialEq],
});

// The full row type, only to regenerate expectations.
use wp::demo::menagerie::Row;
include!("../../zoo-common/rows.rs");

struct ZooCheckLite;

impl Guest for ZooCheckLite {
    async fn run(mut input: wit_bindgen::StreamReader<RowLite>, count: u64, seed: u64) -> u64 {
        let mut i = 0u64;
        let mut matched = 0u64;
        let mut reported = false;
        while let Some(got) = input.next().await {
            let full = row(seed, i);
            let want = RowLite { shape: full.shape, name: full.name, id: full.id, points: full.points };
            if got == want {
                matched += 1;
            } else if !reported {
                eprintln!("zoo-check-lite: row {i} differs\n  got:  {got:?}\n  want: {want:?}");
                reported = true;
            }
            i += 1;
        }
        if i != count {
            eprintln!("zoo-check-lite: received {i} rows, expected {count}");
        }
        matched
    }
}

export!(ZooCheckLite);
