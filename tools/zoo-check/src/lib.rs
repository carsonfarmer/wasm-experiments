//! `zoo-check`: regenerates the rows `zoo` should have sent and returns how
//! many arrived intact, reporting the first difference on stderr.

wit_bindgen::generate!({ world: "zoo-check", path: "../../wit", additional_derives: [PartialEq] });

include!("../../zoo-common/rows.rs");

struct ZooCheck;

impl Guest for ZooCheck {
    async fn run(mut input: wit_bindgen::StreamReader<Row>, count: u64, seed: u64) -> u64 {
        let (mut i, mut intact) = (0, 0);
        while let Some(got) = input.next().await {
            let want = row(seed, i);
            if got == want {
                intact += 1;
            } else if intact == i {
                eprintln!("zoo-check: row {i} differs\n  got:  {got:?}\n  want: {want:?}");
            }
            i += 1;
        }
        if i != count {
            eprintln!("zoo-check: got {i} rows, expected {count}");
        }
        intact
    }
}

export!(ZooCheck);
