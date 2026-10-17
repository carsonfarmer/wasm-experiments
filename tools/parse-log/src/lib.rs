//! `parse-log`: bytes in, one typed `request` row per log line out.

wit_bindgen::generate!({
    world: "parse-log",
    path: "../../wit",
});

use logparse::{Lines, Parser};
use wp::demo::logs::Method;

struct ParseLog;

/// Bytes requested per read; the host's `cat` fills this directly.
const CHUNK: usize = 1 << 20;

pub fn method(m: logparse::Method) -> Method {
    match m {
        logparse::Method::Get => Method::Get,
        logparse::Method::Head => Method::Head,
        logparse::Method::Post => Method::Post,
        logparse::Method::Put => Method::Put,
        logparse::Method::Delete => Method::Delete,
        logparse::Method::Patch => Method::Patch,
        logparse::Method::Options => Method::Options,
        logparse::Method::Other => Method::Other,
    }
}

fn row(r: logparse::Request<'_>) -> Request {
    Request {
        ip: r.ip.to_string(),
        time: r.time,
        method: method(r.method),
        path: r.path.to_string(),
        status: r.status,
        bytes: r.bytes,
        referrer: r.referrer.to_string(),
        agent: r.agent.to_string(),
    }
}

impl Guest for ParseLog {
    async fn run(mut input: wit_bindgen::StreamReader<u8>) -> wit_bindgen::StreamReader<Request> {
        let (mut tx, rx) = wit_stream::new();
        wit_bindgen::spawn_local(async move {
            let mut lines = Lines::default();
            let mut parser = Parser::default();
            let mut buf = Vec::with_capacity(CHUNK);
            let mut skipped = 0u64;
            loop {
                let (_status, data) = input.read(buf).await;
                if data.is_empty() {
                    break;
                }
                let mut out = Vec::with_capacity(data.len() / 200);
                lines.feed(&data, |l| match parser.parse(l) {
                    Some(r) => out.push(row(r)),
                    None => skipped += 1,
                });
                if !out.is_empty() && !tx.write_all(out).await.is_empty() {
                    return; // reader hung up
                }
                buf = data;
                buf.clear();
            }
            let mut out = Vec::new();
            lines.finish(|l| match parser.parse(l) {
                Some(r) => out.push(row(r)),
                None => skipped += 1,
            });
            if !out.is_empty() {
                tx.write_all(out).await;
            }
            if skipped + lines.bad > 0 {
                eprintln!("parse-log: skipped {} unparseable lines", skipped + lines.bad);
            }
        });
        rx
    }
}

export!(ParseLog);
