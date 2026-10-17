//! Parsing web server access logs in Combined Log Format, shared by the log
//! components and the native baseline so both run the exact same code:
//!
//! ```text
//! 203.0.113.9 - - [16/Oct/2026:21:57:03 -0700] "GET /api/v1/users/42 HTTP/1.1" 200 5123 "https://example.com/" "Mozilla/5.0 ..."
//! ```

use chrono::DateTime;
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
    pub fn parse(s: &str) -> Method {
        match s {
            "GET" => Method::Get,
            "HEAD" => Method::Head,
            "POST" => Method::Post,
            "PUT" => Method::Put,
            "DELETE" => Method::Delete,
            "PATCH" => Method::Patch,
            "OPTIONS" => Method::Options,
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

/// `16/Oct/2026:21:57:03 -0700` → Unix seconds.
pub fn parse_time(stamp: &str) -> Option<i64> {
    DateTime::parse_from_str(stamp, "%d/%b/%Y:%H:%M:%S %z").ok().map(|t| t.timestamp())
}

/// `str::split_once` for an ASCII byte, using SIMD `memchr`.
fn split(s: &str, b: u8) -> Option<(&str, &str)> {
    memchr::memchr(b, s.as_bytes()).map(|i| (&s[..i], &s[i + 1..]))
}

/// Parses lines, remembering the last timestamp (consecutive log lines
/// usually share one, so most lines skip date parsing).
#[derive(Default)]
pub struct Parser {
    stamp: String,
    time: i64,
}

impl Parser {
    /// Parse one line (without its trailing newline).
    pub fn parse<'a>(&mut self, line: &'a str) -> Option<Request<'a>> {
        let (ip, rest) = split(line, b' ')?;
        let (stamp, rest) = split(split(rest, b'[')?.1, b']')?;
        let (request, rest) = split(rest.strip_prefix(" \"")?, b'"')?;
        let (status, rest) = split(rest.strip_prefix(' ')?, b' ')?;
        let (bytes, rest) = split(rest, b' ')?;
        let (referrer, rest) = split(rest.strip_prefix('"')?, b'"')?;
        let agent = rest.strip_prefix(" \"")?.strip_suffix('"')?;
        let (method, path) = match split(request, b' ') {
            Some((m, rest)) => (Method::parse(m), split(rest, b' ').map_or(rest, |(p, _)| p)),
            None => (Method::Other, request),
        };
        Some(Request {
            ip,
            time: self.time(stamp)?,
            method,
            path,
            status: status.parse().ok()?,
            bytes: if bytes == "-" { 0 } else { bytes.parse().ok()? },
            referrer,
            agent,
        })
    }

    fn time(&mut self, stamp: &str) -> Option<i64> {
        if stamp != self.stamp {
            self.time = parse_time(stamp)?;
            stamp.clone_into(&mut self.stamp);
        }
        Some(self.time)
    }
}

/// Splits a byte stream arriving in chunks into lines. Complete lines are
/// borrowed from the chunk; only a line split across chunks is copied.
#[derive(Default)]
pub struct Lines {
    partial: Vec<u8>,
    /// Lines that weren't valid UTF-8.
    pub bad: u64,
}

impl Lines {
    pub fn feed(&mut self, chunk: &[u8], mut f: impl FnMut(&str)) {
        let mut start = 0;
        for end in memchr::memchr_iter(b'\n', chunk) {
            if self.partial.is_empty() {
                emit(&mut self.bad, &chunk[start..end], &mut f);
            } else {
                self.partial.extend_from_slice(&chunk[start..end]);
                emit(&mut self.bad, &self.partial, &mut f);
                self.partial.clear();
            }
            start = end + 1;
        }
        self.partial.extend_from_slice(&chunk[start..]);
    }

    pub fn finish(&mut self, mut f: impl FnMut(&str)) {
        if !self.partial.is_empty() {
            emit(&mut self.bad, &self.partial, &mut f);
            self.partial.clear();
        }
    }
}

fn emit(bad: &mut u64, line: &[u8], f: &mut impl FnMut(&str)) {
    match std::str::from_utf8(line.strip_suffix(b"\r").unwrap_or(line)) {
        Ok(s) => f(s),
        Err(_) => *bad += 1,
    }
}

/// Counts occurrences of keys and reports the most frequent.
#[derive(Default)]
pub struct Top(HashMap<String, u64>);

impl Top {
    pub fn add(&mut self, key: &str) {
        match self.0.get_mut(key) {
            Some(c) => *c += 1,
            None => drop(self.0.insert(key.to_string(), 1)),
        }
    }

    /// Add an owned key (no copy when it's new).
    pub fn add_owned(&mut self, key: String) {
        *self.0.entry(key).or_default() += 1;
    }

    /// The `n` most frequent keys, by count (descending) then key.
    pub fn top(&self, n: usize) -> Vec<(u64, &str)> {
        let mut v: Vec<_> = self.0.iter().map(|(k, c)| (*c, k.as_str())).collect();
        v.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
        v.truncate(n);
        v
    }

    /// The report every implementation prints: `count path`, one per line.
    pub fn report(&self, n: usize) -> String {
        self.top(n).iter().map(|(c, k)| format!("{c:>9} {k}\n")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: &str = r#"203.0.113.9 - - [16/Oct/2026:21:57:03 -0700] "GET /api/v1/users/42?x=1 HTTP/1.1" 200 5123 "https://example.com/" "Mozilla/5.0 (X11; Linux x86_64) \"quoted\"""#;

    #[test]
    fn parses_a_line() {
        let r = Parser::default().parse(LINE).unwrap();
        assert_eq!(r.ip, "203.0.113.9");
        assert_eq!(r.method, Method::Get);
        assert_eq!(r.path, "/api/v1/users/42?x=1");
        assert_eq!(r.status, 200);
        assert_eq!(r.bytes, 5123);
        assert_eq!(r.referrer, "https://example.com/");
        assert_eq!(r.agent, r#"Mozilla/5.0 (X11; Linux x86_64) \"quoted\""#);
        assert_eq!(r.time, 1_792_213_023); // 2026-10-17 04:57:03 UTC
    }

    #[test]
    fn time_matches_known_epochs() {
        assert_eq!(parse_time("01/Jan/1970:00:00:00 +0000"), Some(0));
        assert_eq!(parse_time("29/Feb/2024:12:00:00 +0000"), Some(1_709_208_000));
        assert_eq!(parse_time("01/Jan/2000:00:00:00 +0130"), Some(946_684_800 - 5400));
        assert_eq!(parse_time("01/Foo/2000:00:00:00 +0000"), None);
    }

    #[test]
    fn cached_timestamps_follow_changes() {
        let mut p = Parser::default();
        let at = |p: &mut Parser, t: &str| p.parse(&LINE.replace("16/Oct/2026:21:57:03 -0700", t)).unwrap().time;
        assert_eq!(at(&mut p, "01/Jan/1970:00:00:00 +0000"), 0);
        assert_eq!(at(&mut p, "01/Jan/1970:00:00:00 +0000"), 0);
        assert_eq!(at(&mut p, "01/Jan/1970:00:00:01 +0000"), 1);
        assert!(p.parse(&LINE.replace("16/Oct/2026", "99/Xyz/2026")).is_none());
    }

    #[test]
    fn handles_dashes_and_odd_requests() {
        let mut p = Parser::default();
        let r = p.parse(r#"10.0.0.1 - frank [01/Jan/2026:00:00:00 +0000] "BREW /pot" 418 - "-" "-""#).unwrap();
        assert_eq!((r.method, r.path, r.bytes, r.referrer), (Method::Other, "/pot", 0, "-"));
        let r = p.parse(r#"10.0.0.1 - - [01/Jan/2026:00:00:00 +0000] "-" 400 0 "-" "-""#).unwrap();
        assert_eq!(r.path, "-");
    }

    #[test]
    fn rejects_garbage() {
        let mut p = Parser::default();
        for bad in [
            "",
            "nonsense",
            r#"1.2.3.4 - - [bad] "GET / HTTP/1.1" 200 1 "-" "-""#,
            r#"1.2.3.4 - - [01/Jan/2026:00:00:00 +0000] "GET / HTTP/1.1" abc 1 "-" "-""#,
            r#"1.2.3.4 - - [01/Jan/2026:00:00:00 +0000] "GET / HTTP/1.1" 200 1 "-""#,
        ] {
            assert!(p.parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn lines_survive_any_chunking() {
        let text = "one\ntwo\r\n\nthree\nfour";
        for size in 1..=text.len() {
            let (mut lines, mut got) = (Lines::default(), Vec::new());
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
        ["/b", "/a", "/b", "/c", "/a", "/b"].into_iter().for_each(|k| t.add(k));
        t.add_owned("/c".into());
        assert_eq!(t.top(2), vec![(3, "/b"), (2, "/a")]);
        assert_eq!(t.report(3), "        3 /b\n        2 /a\n        2 /c\n");
    }
}
