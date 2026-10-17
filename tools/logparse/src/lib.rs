//! Parsing web server access logs in Combined Log Format, shared by the
//! log components and the native baseline so both run the exact same code:
//!
//! ```text
//! 203.0.113.9 - - [16/Oct/2026:21:57:03 -0700] "GET /api/v1/users/42 HTTP/1.1" 200 5123 "https://example.com/" "Mozilla/5.0 ..."
//! ```

use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Post,
    Put,
    Delete,
    Patch,
    Options,
    Other,
}

impl Method {
    pub fn parse(s: &[u8]) -> Method {
        match s {
            b"GET" => Method::Get,
            b"HEAD" => Method::Head,
            b"POST" => Method::Post,
            b"PUT" => Method::Put,
            b"DELETE" => Method::Delete,
            b"PATCH" => Method::Patch,
            b"OPTIONS" => Method::Options,
            _ => Method::Other,
        }
    }
}

/// One parsed line, borrowing its strings from the input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request<'a> {
    pub ip: &'a str,
    /// Unix seconds.
    pub time: i64,
    pub method: Method,
    pub path: &'a str,
    pub status: u16,
    pub bytes: u64,
    pub referrer: &'a str,
    pub agent: &'a str,
}

fn find(hay: &[u8], from: usize, b: u8) -> Option<usize> {
    memchr::memchr(b, hay.get(from..)?).map(|i| from + i)
}

fn num(s: &[u8]) -> Option<u64> {
    if s.is_empty() || s.len() > 19 {
        return None;
    }
    let mut n = 0u64;
    for &c in s {
        if !c.is_ascii_digit() {
            return None;
        }
        n = n * 10 + (c - b'0') as u64;
    }
    Some(n)
}

fn month(m: &[u8]) -> Option<u32> {
    Some(match m {
        b"Jan" => 1,
        b"Feb" => 2,
        b"Mar" => 3,
        b"Apr" => 4,
        b"May" => 5,
        b"Jun" => 6,
        b"Jul" => 7,
        b"Aug" => 8,
        b"Sep" => 9,
        b"Oct" => 10,
        b"Nov" => 11,
        b"Dec" => 12,
        _ => return None,
    })
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// `16/Oct/2026:21:57:03 -0700` → Unix seconds.
pub fn parse_time(t: &[u8]) -> Option<i64> {
    if t.len() != 26 || t[2] != b'/' || t[6] != b'/' || t[11] != b':' || t[20] != b' ' {
        return None;
    }
    let d = num(&t[0..2])? as u32;
    let m = month(&t[3..6])?;
    let y = num(&t[7..11])? as i64;
    let hh = num(&t[12..14])? as i64;
    let mm = num(&t[15..17])? as i64;
    let ss = num(&t[18..20])? as i64;
    let sign = match t[21] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let oh = num(&t[22..24])? as i64;
    let om = num(&t[24..26])? as i64;
    let local = days_from_civil(y, m, d) * 86400 + hh * 3600 + mm * 60 + ss;
    Some(local - sign * (oh * 3600 + om * 60))
}

/// End of a double-quoted field starting just after its opening quote.
fn quoted_end(line: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    loop {
        let q = find(line, i, b'"')?;
        if q > start && line[q - 1] == b'\\' {
            i = q + 1;
            continue;
        }
        return Some(q);
    }
}

/// Parse one line (without its trailing newline).
pub fn parse(line: &str) -> Option<Request<'_>> {
    let b = line.as_bytes();
    let sp = find(b, 0, b' ')?;
    let ip = &line[..sp];
    let lb = find(b, sp, b'[')?;
    let rb = find(b, lb, b']')?;
    let time = parse_time(&b[lb + 1..rb])?;
    // ` "METHOD PATH PROTO"`
    if b.get(rb + 1..rb + 3)? != b" \"" {
        return None;
    }
    let rq = rb + 3;
    let rq_end = quoted_end(b, rq)?;
    let request = &b[rq..rq_end];
    let (method, path) = match memchr::memchr(b' ', request) {
        Some(s1) => {
            let rest = &line[rq + s1 + 1..rq_end];
            let path = match memchr::memchr(b' ', rest.as_bytes()) {
                Some(s2) => &rest[..s2],
                None => rest,
            };
            (Method::parse(&request[..s1]), path)
        }
        None => (Method::Other, &line[rq..rq_end]),
    };
    // ` STATUS BYTES `
    let s0 = rq_end + 2;
    let s1 = find(b, s0, b' ')?;
    let status = num(&b[s0..s1])? as u16;
    let b1 = find(b, s1 + 1, b' ')?;
    let bytes = match &b[s1 + 1..b1] {
        b"-" => 0,
        n => num(n)?,
    };
    // `"REFERRER" "AGENT"`
    if b.get(b1 + 1) != Some(&b'"') {
        return None;
    }
    let r0 = b1 + 2;
    let r1 = quoted_end(b, r0)?;
    let referrer = &line[r0..r1];
    if b.get(r1 + 1..r1 + 3)? != b" \"" {
        return None;
    }
    let a0 = r1 + 3;
    let a1 = quoted_end(b, a0)?;
    let agent = &line[a0..a1];
    Some(Request { ip, time, method, path, status, bytes, referrer, agent })
}

/// Splits a byte stream arriving in chunks into lines, without copying
/// complete lines (only a line split across two chunks is copied once).
#[derive(Default)]
pub struct Lines {
    partial: Vec<u8>,
    /// Lines that weren't valid UTF-8.
    pub bad: u64,
}

impl Lines {
    pub fn feed(&mut self, mut chunk: &[u8], mut f: impl FnMut(&str)) {
        if !self.partial.is_empty() {
            match memchr::memchr(b'\n', chunk) {
                Some(nl) => {
                    self.partial.extend_from_slice(&chunk[..nl]);
                    let line = std::mem::take(&mut self.partial);
                    self.emit(&line, &mut f);
                    self.partial = line;
                    self.partial.clear();
                    chunk = &chunk[nl + 1..];
                }
                None => {
                    self.partial.extend_from_slice(chunk);
                    return;
                }
            }
        }
        let mut start = 0;
        for nl in memchr::memchr_iter(b'\n', chunk) {
            self.emit(&chunk[start..nl], &mut f);
            start = nl + 1;
        }
        self.partial.extend_from_slice(&chunk[start..]);
    }

    pub fn finish(&mut self, mut f: impl FnMut(&str)) {
        if !self.partial.is_empty() {
            let line = std::mem::take(&mut self.partial);
            self.emit(&line, &mut f);
        }
    }

    fn emit(&mut self, line: &[u8], f: &mut impl FnMut(&str)) {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        match std::str::from_utf8(line) {
            Ok(s) => f(s),
            Err(_) => self.bad += 1,
        }
    }
}

/// Counts occurrences of keys and reports the most frequent.
#[derive(Default)]
pub struct Top {
    counts: HashMap<String, u64>,
}

impl Top {
    #[inline]
    pub fn add(&mut self, key: &str) {
        match self.counts.get_mut(key) {
            Some(c) => *c += 1,
            None => {
                self.counts.insert(key.to_string(), 1);
            }
        }
    }

    /// Add an owned key without re-allocating it when it's new.
    #[inline]
    pub fn add_owned(&mut self, key: String) {
        *self.counts.entry(key).or_insert(0) += 1;
    }

    /// The `n` most frequent keys, by count (descending) then key.
    pub fn top(&self, n: usize) -> Vec<(u64, &str)> {
        let mut v: Vec<(u64, &str)> = self.counts.iter().map(|(k, c)| (*c, k.as_str())).collect();
        v.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
        v.truncate(n);
        v
    }

    /// The report every implementation prints: `count path`, one per line.
    pub fn report(&self, n: usize) -> String {
        let mut out = String::new();
        for (c, k) in self.top(n) {
            out.push_str(&format!("{c:>9} {k}\n"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: &str = r#"203.0.113.9 - - [16/Oct/2026:21:57:03 -0700] "GET /api/v1/users/42?x=1 HTTP/1.1" 200 5123 "https://example.com/" "Mozilla/5.0 (X11; Linux x86_64) \"quoted\"""#;

    #[test]
    fn parses_a_line() {
        let r = parse(LINE).unwrap();
        assert_eq!(r.ip, "203.0.113.9");
        assert_eq!(r.method, Method::Get);
        assert_eq!(r.path, "/api/v1/users/42?x=1");
        assert_eq!(r.status, 200);
        assert_eq!(r.bytes, 5123);
        assert_eq!(r.referrer, "https://example.com/");
        assert_eq!(r.agent, r#"Mozilla/5.0 (X11; Linux x86_64) \"quoted\""#);
        // 2026-10-17 04:57:03 UTC
        assert_eq!(r.time, 1_792_213_023);
    }

    #[test]
    fn time_matches_known_epochs() {
        assert_eq!(parse_time(b"01/Jan/1970:00:00:00 +0000"), Some(0));
        assert_eq!(parse_time(b"29/Feb/2024:12:00:00 +0000"), Some(1_709_208_000));
        assert_eq!(parse_time(b"01/Jan/2000:00:00:00 +0130"), Some(946_684_800 - 5400));
        assert_eq!(parse_time(b"01/Foo/2000:00:00:00 +0000"), None);
    }

    #[test]
    fn handles_dashes_and_odd_requests() {
        let r = parse(r#"10.0.0.1 - frank [01/Jan/2026:00:00:00 +0000] "BREW /pot" 418 - "-" "-""#).unwrap();
        assert_eq!(r.method, Method::Other);
        assert_eq!(r.path, "/pot");
        assert_eq!(r.bytes, 0);
        assert_eq!(r.referrer, "-");
        let r = parse(r#"10.0.0.1 - - [01/Jan/2026:00:00:00 +0000] "-" 400 0 "-" "-""#).unwrap();
        assert_eq!(r.path, "-");
    }

    #[test]
    fn rejects_garbage() {
        for bad in ["", "nonsense", "1.2.3.4 - - [bad] \"GET / HTTP/1.1\" 200 1 \"-\" \"-\"",
                    r#"1.2.3.4 - - [01/Jan/2026:00:00:00 +0000] "GET / HTTP/1.1" abc 1 "-" "-""#,
                    r#"1.2.3.4 - - [01/Jan/2026:00:00:00 +0000] "GET / HTTP/1.1" 200 1 "-""#] {
            assert!(parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn lines_survive_any_chunking() {
        let text = "one\ntwo\r\n\nthree\nfour";
        for size in 1..=text.len() {
            let mut lines = Lines::default();
            let mut got = Vec::new();
            for chunk in text.as_bytes().chunks(size) {
                lines.feed(chunk, |l| got.push(l.to_string()));
            }
            lines.finish(|l| got.push(l.to_string()));
            assert_eq!(got, ["one", "two", "", "three", "four"], "chunk size {size}");
        }
    }

    #[test]
    fn top_orders_by_count_then_key() {
        let mut t = Top::default();
        for k in ["/b", "/a", "/b", "/c", "/a", "/b"] {
            t.add(k);
        }
        t.add_owned("/c".to_string());
        assert_eq!(t.top(2), vec![(3, "/b"), (2, "/a")]);
        assert_eq!(t.report(3), "        3 /b\n        2 /a\n        2 /c\n");
    }
}
