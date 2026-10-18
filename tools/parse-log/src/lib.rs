//! `parse-log`: bytes in, one `request` per log line out. Lines that don't
//! parse are skipped and counted on stderr.

wit_bindgen::generate!({ world: "parse-log", path: "../../wit" });

use logparse::{Lines, Parser};
use wp::demo::logs::Method;

struct ParseLog;

impl Guest for ParseLog {
    async fn run(mut input: wit_bindgen::StreamReader<u8>) -> wit_bindgen::StreamReader<Request> {
        let (mut tx, rx) = wit_stream::new();
        wit_bindgen::spawn_local(async move {
            let (mut lines, mut parser, mut skipped) = (Lines::default(), Parser::default(), 0);
            let mut buf = Vec::with_capacity(1 << 20);
            loop {
                let (_, data) = input.read(buf).await;
                let mut rows = Vec::with_capacity(data.len() / 200);
                let mut emit = |line: &str| match parser.parse(line) {
                    Some(r) => rows.push(row(r)),
                    None => skipped += 1,
                };
                match data.is_empty() {
                    true => lines.finish(&mut emit),
                    false => lines.feed(&data, &mut emit),
                }
                if !rows.is_empty() && !tx.write_all(rows).await.is_empty() {
                    return; // the reader stopped reading
                }
                if data.is_empty() {
                    break;
                }
                buf = data;
                buf.clear();
            }
            if skipped + lines.bad > 0 {
                eprintln!("parse-log: skipped {} lines", skipped + lines.bad);
            }
        });
        rx
    }
}

fn row(r: logparse::Request<'_>) -> Request {
    use logparse::Method as M;
    let method = match r.method {
        M::Get => Method::Get,
        M::Head => Method::Head,
        M::Post => Method::Post,
        M::Put => Method::Put,
        M::Delete => Method::Delete,
        M::Patch => Method::Patch,
        M::Options => Method::Options,
        M::Other => Method::Other,
    };
    Request {
        ip: r.ip.into(),
        time: r.time,
        method,
        path: r.path.into(),
        status: r.status,
        bytes: r.bytes,
        referrer: r.referrer.into(),
        agent: r.agent.into(),
    }
}

export!(ParseLog);
