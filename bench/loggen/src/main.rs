//! Generate a realistic, deterministic web access log (Combined Log Format).
//!
//!     loggen LINES OUT.log [SEED]
//!
//! Paths follow a Zipf-like popularity curve over pages, static assets, blog
//! posts and API endpoints (with numeric IDs, so there are many distinct
//! paths). Some endpoints are flaky and return 5xx more often, so "top error
//! paths" has a real answer.

use std::fmt::Write as _;
use std::io::{BufWriter, Write};

/// SplitMix64: small, fast, and deterministic across platforms.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        ((self.next() >> 11) as u128 * n as u128 >> 53) as u64
    }
    fn chance(&mut self, per_mille: u64) -> bool {
        self.below(1000) < per_mille
    }
}

/// Picks indices with probability proportional to `weights`.
struct Weighted {
    cumulative: Vec<u64>,
}

impl Weighted {
    fn new(weights: impl IntoIterator<Item = u64>) -> Self {
        let mut total = 0;
        let cumulative = weights
            .into_iter()
            .map(|w| {
                total += w;
                total
            })
            .collect();
        Weighted { cumulative }
    }
    fn zipf(n: usize, s: f64) -> Self {
        Self::new((1..=n).map(|r| (1e9 / (r as f64).powf(s)) as u64 + 1))
    }
    fn pick(&self, rng: &mut Rng) -> usize {
        let x = rng.below(*self.cumulative.last().unwrap());
        self.cumulative.partition_point(|&c| c <= x)
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Page,
    Static,
    Blog,
    Api,
}

struct Route {
    template: &'static str,
    kind: Kind,
    method: &'static str,
    /// Chance (per mille) of a 5xx response.
    fail: u64,
}

const ROUTES: &[Route] = &[
    Route { template: "/", kind: Kind::Page, method: "GET", fail: 2 },
    Route { template: "/static/js/app.4f9c2e.js", kind: Kind::Static, method: "GET", fail: 0 },
    Route { template: "/static/css/main.81d3aa.css", kind: Kind::Static, method: "GET", fail: 0 },
    Route { template: "/api/v1/users/{id}", kind: Kind::Api, method: "GET", fail: 4 },
    Route { template: "/favicon.ico", kind: Kind::Static, method: "GET", fail: 0 },
    Route { template: "/api/v1/search?q={word}", kind: Kind::Api, method: "GET", fail: 12 },
    Route { template: "/static/img/logo.svg", kind: Kind::Static, method: "GET", fail: 0 },
    Route { template: "/pricing", kind: Kind::Page, method: "GET", fail: 2 },
    Route { template: "/api/v2/recommendations?user={id}", kind: Kind::Api, method: "GET", fail: 55 },
    Route { template: "/blog/{slug}", kind: Kind::Blog, method: "GET", fail: 3 },
    Route { template: "/docs/{slug}", kind: Kind::Page, method: "GET", fail: 1 },
    Route { template: "/api/v1/orders/{id}/items", kind: Kind::Api, method: "GET", fail: 8 },
    Route { template: "/graphql", kind: Kind::Api, method: "POST", fail: 21 },
    Route { template: "/api/v1/checkout", kind: Kind::Api, method: "POST", fail: 80 },
    Route { template: "/login", kind: Kind::Page, method: "POST", fail: 5 },
    Route { template: "/api/v1/health", kind: Kind::Api, method: "GET", fail: 1 },
    Route { template: "/robots.txt", kind: Kind::Static, method: "GET", fail: 0 },
    Route { template: "/api/v1/users/{id}", kind: Kind::Api, method: "PUT", fail: 9 },
    Route { template: "/api/v1/sessions/{id}", kind: Kind::Api, method: "DELETE", fail: 6 },
    Route { template: "/sitemap.xml", kind: Kind::Static, method: "GET", fail: 0 },
    Route { template: "/careers", kind: Kind::Page, method: "GET", fail: 2 },
    Route { template: "/api/v1/uploads", kind: Kind::Api, method: "POST", fail: 30 },
    Route { template: "/about", kind: Kind::Page, method: "GET", fail: 1 },
    Route { template: "/api/v1/users/{id}/avatar", kind: Kind::Api, method: "PATCH", fail: 7 },
];

const WORDS: &[&str] = &[
    "rust", "wasm", "pipes", "streams", "components", "async", "logs", "latency", "cache",
    "kernel", "network", "storage", "compiler", "parser", "database", "index", "query",
    "metrics", "tracing", "deploy", "release", "security", "auth", "billing", "search",
    "mobile", "design", "pricing", "observability", "edge", "sandbox", "runtime",
];

const AGENTS: &[(&str, u64)] = &[
    ("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36", 30),
    ("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15", 14),
    ("Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1", 16),
    ("Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0", 6),
    ("Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Mobile Safari/537.36", 10),
    ("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36 Edg/129.0.0.0", 6),
    ("Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)", 5),
    ("Mozilla/5.0 (compatible; bingbot/2.0; +http://www.bing.com/bingbot.htm)", 2),
    ("curl/8.9.1", 3),
    ("python-requests/2.32.3", 3),
    ("Go-http-client/2.0", 2),
    ("okhttp/4.12.0", 3),
];

const REFERRERS: &[(&str, u64)] = &[
    ("-", 40),
    ("https://www.google.com/", 20),
    ("https://example.com/", 15),
    ("https://example.com/pricing", 5),
    ("https://example.com/docs/getting-started", 5),
    ("https://news.ycombinator.com/", 4),
    ("https://duckduckgo.com/", 3),
    ("https://www.bing.com/", 2),
    ("https://t.co/xyz", 3),
    ("https://example.com/blog/streams-everywhere", 3),
];

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// Civil date from days since 1970-01-01 (inverse of Hinnant's algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: loggen LINES OUT.log [SEED]");
        std::process::exit(2);
    }
    let lines: u64 = args[1].parse().expect("LINES must be a number");
    let seed: u64 = args.get(3).map(|s| s.parse().expect("SEED must be a number")).unwrap_or(42);
    let mut out = BufWriter::with_capacity(1 << 20, std::fs::File::create(&args[2])?);
    let mut rng = Rng(seed);

    let routes = Weighted::zipf(ROUTES.len(), 1.1);
    let ips: Vec<String> = (0..20_000)
        .map(|_| {
            let mut r = || rng.below(254) + 1;
            format!("{}.{}.{}.{}", [23, 34, 52, 66, 73, 98, 104, 142, 172, 185, 203][(r() % 11) as usize], r(), r(), r())
        })
        .collect();
    let ip_pick = Weighted::zipf(ips.len(), 0.8);
    let agents = Weighted::new(AGENTS.iter().map(|a| a.1));
    let referrers = Weighted::new(REFERRERS.iter().map(|r| r.1));
    let slugs: Vec<String> = (0..300)
        .map(|i| format!("{}-{}-{}", WORDS[i % WORDS.len()], WORDS[(i * 7 + 3) % WORDS.len()], i))
        .collect();
    let slug_pick = Weighted::zipf(slugs.len(), 1.0);
    let word_pick = Weighted::zipf(WORDS.len(), 0.9);

    // 2026-10-01T00:00:00Z, served from a -0700 timezone.
    let mut t_ms: i64 = 1_790_812_800_000;
    let tz = -7 * 3600;
    let mut line = String::with_capacity(512);
    let mut path = String::with_capacity(128);
    for _ in 0..lines {
        t_ms += rng.below(12) as i64;
        let route = &ROUTES[routes.pick(&mut rng)];

        path.clear();
        let t = route.template;
        if let Some(i) = t.find('{') {
            let j = t.find('}').unwrap();
            path.push_str(&t[..i]);
            match &t[i + 1..j] {
                "id" => write!(path, "{}", 1 + rng.below(200_000)).unwrap(),
                "word" => path.push_str(WORDS[word_pick.pick(&mut rng)]),
                _ => path.push_str(&slugs[slug_pick.pick(&mut rng)]),
            }
            path.push_str(&t[j + 1..]);
        } else {
            path.push_str(t);
        }

        let mut method = route.method;
        if rng.chance(2) {
            method = ["HEAD", "OPTIONS"][rng.below(2) as usize];
        }
        let status: u16 = if rng.chance(route.fail) {
            [500, 500, 502, 503, 503, 504][rng.below(6) as usize]
        } else if rng.chance(30) {
            path.push_str(["/old", "/undefined", ".bak", "/wp-login.php"][rng.below(4) as usize]);
            404
        } else if matches!(route.kind, Kind::Static) && rng.chance(250) {
            304
        } else if matches!(route.kind, Kind::Api) && rng.chance(12) {
            [401, 403, 429][rng.below(3) as usize]
        } else if matches!(route.kind, Kind::Page) && rng.chance(10) {
            301
        } else {
            200
        };
        let bytes = match (status, route.kind) {
            (304, _) | (301, _) => 0,
            (s, _) if s >= 400 => 120 + rng.below(400),
            (_, Kind::Static) => 2_000 + rng.below(180_000),
            (_, Kind::Page) | (_, Kind::Blog) => 8_000 + rng.below(60_000),
            (_, Kind::Api) => 200 + rng.below(12_000),
        };

        let local = t_ms.div_euclid(1000) + tz;
        let (y, mo, d) = civil(local.div_euclid(86400));
        let secs = local.rem_euclid(86400);
        line.clear();
        write!(
            line,
            "{} - - [{:02}/{}/{}:{:02}:{:02}:{:02} -0700] \"{} {} HTTP/1.1\" {} {} \"{}\" \"{}\"\n",
            ips[ip_pick.pick(&mut rng)],
            d,
            MONTHS[mo as usize - 1],
            y,
            secs / 3600,
            secs / 60 % 60,
            secs % 60,
            method,
            path,
            status,
            if bytes == 0 && rng.chance(500) { "-".to_string() } else { bytes.to_string() },
            REFERRERS[referrers.pick(&mut rng)].0,
            AGENTS[agents.pick(&mut rng)].0,
        )
        .unwrap();
        out.write_all(line.as_bytes())?;
    }
    out.flush()
}
