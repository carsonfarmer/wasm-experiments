//! `parse-log-cols`: bytes in, columnar `request-batch`es out (one per chunk
//! read, ~5k rows), with no per-row allocation.

wit_bindgen::generate!({
    world: "parse-log-cols",
    path: "../../wit",
});

use cols::StrColBuilder;
use logparse::Lines;
use wp::demo::cols::StrCol;
use wp::demo::logs::Method;

struct ParseLogCols;

const CHUNK: usize = 1 << 20;

fn method(m: logparse::Method) -> Method {
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

#[derive(Default)]
struct Builder {
    ip: StrColBuilder,
    time: Vec<i64>,
    method: Vec<Method>,
    path: StrColBuilder,
    status: Vec<u16>,
    bytes: Vec<u64>,
    referrer: StrColBuilder,
    agent: StrColBuilder,
}

fn col(b: &mut StrColBuilder) -> StrCol {
    let (data, offsets) = b.finish();
    StrCol { data, offsets }
}

impl Builder {
    fn push(&mut self, r: logparse::Request<'_>) {
        self.ip.push(r.ip);
        self.time.push(r.time);
        self.method.push(method(r.method));
        self.path.push(r.path);
        self.status.push(r.status);
        self.bytes.push(r.bytes);
        self.referrer.push(r.referrer);
        self.agent.push(r.agent);
    }

    fn rows(&self) -> usize {
        self.time.len()
    }

    fn finish(&mut self) -> RequestBatch {
        let rows = self.rows();
        RequestBatch {
            ip: col(&mut self.ip),
            time: std::mem::replace(&mut self.time, Vec::with_capacity(rows)),
            method: std::mem::replace(&mut self.method, Vec::with_capacity(rows)),
            path: col(&mut self.path),
            status: std::mem::replace(&mut self.status, Vec::with_capacity(rows)),
            bytes: std::mem::replace(&mut self.bytes, Vec::with_capacity(rows)),
            referrer: col(&mut self.referrer),
            agent: col(&mut self.agent),
        }
    }
}

impl Guest for ParseLogCols {
    async fn run(mut input: wit_bindgen::StreamReader<u8>) -> wit_bindgen::StreamReader<RequestBatch> {
        let (mut tx, rx) = wit_stream::new();
        wit_bindgen::spawn_local(async move {
            let mut lines = Lines::default();
            let mut b = Builder::default();
            let mut buf = Vec::with_capacity(CHUNK);
            let mut skipped = 0u64;
            loop {
                let (_status, data) = input.read(buf).await;
                if data.is_empty() {
                    break;
                }
                lines.feed(&data, |l| match logparse::parse(l) {
                    Some(r) => b.push(r),
                    None => skipped += 1,
                });
                if b.rows() > 0 && tx.write_one(b.finish()).await.is_some() {
                    return;
                }
                buf = data;
                buf.clear();
            }
            lines.finish(|l| match logparse::parse(l) {
                Some(r) => b.push(r),
                None => skipped += 1,
            });
            if b.rows() > 0 {
                tx.write_one(b.finish()).await;
            }
            if skipped + lines.bad > 0 {
                eprintln!("parse-log-cols: skipped {} unparseable lines", skipped + lines.bad);
            }
        });
        rx
    }
}

export!(ParseLogCols);
