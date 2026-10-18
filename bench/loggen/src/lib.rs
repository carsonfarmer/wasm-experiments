//! Generate a realistic, deterministic web access log (Combined Log Format).
//!
//! Paths follow a Zipf popularity curve over pages, static assets, blog posts
//! and API endpoints (with numeric IDs, so there are many distinct paths).
//! Some endpoints are flaky and return 5xx more often, so "top error paths"
//! has a real answer.

use chrono::{DateTime, FixedOffset};
use rand::distr::{Distribution, weighted::WeightedIndex};
use rand::{RngExt, SeedableRng, rngs::StdRng};
use rand_distr::Zipf;
use std::io::Write;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Page,
    Static,
    Blog,
    Api,
}

/// A route, its method, and how often (per mille) it fails with a 5xx.
struct Route(&'static str, Kind, &'static str, u32);

const ROUTES: &[Route] = &[
    Route("/", Kind::Page, "GET", 2),
    Route("/static/js/app.4f9c2e.js", Kind::Static, "GET", 0),
    Route("/static/css/main.81d3aa.css", Kind::Static, "GET", 0),
    Route("/api/v1/users/{id}", Kind::Api, "GET", 4),
    Route("/favicon.ico", Kind::Static, "GET", 0),
    Route("/api/v1/search?q={word}", Kind::Api, "GET", 12),
    Route("/static/img/logo.svg", Kind::Static, "GET", 0),
    Route("/pricing", Kind::Page, "GET", 2),
    Route("/api/v2/recommendations?user={id}", Kind::Api, "GET", 55),
    Route("/blog/{slug}", Kind::Blog, "GET", 3),
    Route("/docs/{slug}", Kind::Page, "GET", 1),
    Route("/api/v1/orders/{id}/items", Kind::Api, "GET", 8),
    Route("/graphql", Kind::Api, "POST", 21),
    Route("/api/v1/checkout", Kind::Api, "POST", 80),
    Route("/login", Kind::Page, "POST", 5),
    Route("/api/v1/health", Kind::Api, "GET", 1),
    Route("/robots.txt", Kind::Static, "GET", 0),
    Route("/api/v1/users/{id}", Kind::Api, "PUT", 9),
    Route("/api/v1/sessions/{id}", Kind::Api, "DELETE", 6),
    Route("/sitemap.xml", Kind::Static, "GET", 0),
    Route("/careers", Kind::Page, "GET", 2),
    Route("/api/v1/uploads", Kind::Api, "POST", 30),
    Route("/about", Kind::Page, "GET", 1),
    Route("/api/v1/users/{id}/avatar", Kind::Api, "PATCH", 7),
];

const WORDS: &[&str] = &[
    "rust",
    "wasm",
    "pipes",
    "streams",
    "components",
    "async",
    "logs",
    "latency",
    "cache",
    "kernel",
    "network",
    "storage",
    "compiler",
    "parser",
    "database",
    "index",
    "query",
    "metrics",
    "tracing",
    "deploy",
    "release",
    "security",
    "auth",
    "billing",
    "search",
    "mobile",
    "design",
    "pricing",
    "observability",
    "edge",
    "sandbox",
    "runtime",
];

const AGENTS: &[(&str, u32)] = &[
    (
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36",
        30,
    ),
    (
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15",
        14,
    ),
    (
        "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1",
        16,
    ),
    ("Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0", 6),
    (
        "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Mobile Safari/537.36",
        10,
    ),
    (
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36 Edg/129.0.0.0",
        6,
    ),
    ("Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)", 5),
    ("Mozilla/5.0 (compatible; bingbot/2.0; +http://www.bing.com/bingbot.htm)", 2),
    ("curl/8.9.1", 3),
    ("python-requests/2.32.3", 3),
    ("Go-http-client/2.0", 2),
    ("okhttp/4.12.0", 3),
];

const REFERRERS: &[(&str, u32)] = &[
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

/// Picks one of `n` items, the k-th most popular with probability ∝ 1/kˢ.
struct Popular(Zipf<f64>);

impl Popular {
    fn new(n: usize, s: f64) -> Self {
        Popular(Zipf::new(n as f64, s).unwrap())
    }
    fn pick(&self, rng: &mut StdRng) -> usize {
        self.0.sample(rng) as usize - 1
    }
}

/// Write `lines` log lines generated from `seed` to `out`.
pub fn generate(lines: u64, seed: u64, mut out: impl Write) -> std::io::Result<()> {
    let mut rng = StdRng::seed_from_u64(seed);
    let ips: Vec<String> = (0..20_000)
        .map(|_| {
            let net = [23, 34, 52, 66, 73, 98, 104, 142, 172, 185, 203][rng.random_range(0..11)];
            format!("{net}.{}.{}.{}", rng.random_range(1..255), rng.random_range(1..255), rng.random_range(1..255))
        })
        .collect();
    let slugs: Vec<String> =
        (0..300).map(|i| format!("{}-{}-{i}", WORDS[i % WORDS.len()], WORDS[(i * 7 + 3) % WORDS.len()])).collect();
    let (route, ip, slug, word) = (
        Popular::new(ROUTES.len(), 1.1),
        Popular::new(ips.len(), 0.8),
        Popular::new(slugs.len(), 1.0),
        Popular::new(WORDS.len(), 0.9),
    );
    let agent = WeightedIndex::new(AGENTS.iter().map(|a| a.1)).unwrap();
    let referrer = WeightedIndex::new(REFERRERS.iter().map(|r| r.1)).unwrap();

    // Starting 2026-10-01T00:00:00Z, served from a -0700 timezone.
    let tz = FixedOffset::west_opt(7 * 3600).unwrap();
    let mut t_ms: i64 = 1_790_812_800_000;
    let mut stamp = (0, String::new()); // the formatted current second
    for _ in 0..lines {
        t_ms += rng.random_range(0..12);
        let Route(template, kind, method, fail) = &ROUTES[route.pick(&mut rng)];
        let mut path = match template.split_once('{') {
            Some((pre, rest)) => {
                let (key, post) = rest.split_once('}').unwrap();
                let mid = match key {
                    "id" => rng.random_range(1..=200_000).to_string(),
                    "word" => WORDS[word.pick(&mut rng)].to_string(),
                    _ => slugs[slug.pick(&mut rng)].clone(),
                };
                format!("{pre}{mid}{post}")
            }
            None => template.to_string(),
        };
        let method = if rng.random_ratio(2, 1000) { ["HEAD", "OPTIONS"][rng.random_range(0..2)] } else { method };
        let status = if rng.random_ratio(*fail, 1000) {
            [500, 500, 502, 503, 503, 504][rng.random_range(0..6)]
        } else if rng.random_ratio(30, 1000) {
            path += ["/old", "/undefined", ".bak", "/wp-login.php"][rng.random_range(0..4)];
            404
        } else if *kind == Kind::Static && rng.random_ratio(250, 1000) {
            304
        } else if *kind == Kind::Api && rng.random_ratio(12, 1000) {
            [401, 403, 429][rng.random_range(0..3)]
        } else if *kind == Kind::Page && rng.random_ratio(10, 1000) {
            301
        } else {
            200
        };
        let bytes = match (status, kind) {
            (301 | 304, _) => 0,
            (400.., _) => rng.random_range(120..520),
            (_, Kind::Static) => rng.random_range(2_000..182_000),
            (_, Kind::Page | Kind::Blog) => rng.random_range(8_000..68_000),
            (_, Kind::Api) => rng.random_range(200..12_200),
        };
        let bytes = if bytes == 0 && rng.random_bool(0.5) { "-".into() } else { bytes.to_string() };

        let secs = t_ms / 1000;
        if stamp.0 != secs {
            let t = DateTime::from_timestamp(secs, 0).unwrap().with_timezone(&tz);
            stamp = (secs, t.format("%d/%b/%Y:%H:%M:%S %z").to_string());
        }
        writeln!(
            out,
            "{} - - [{}] \"{method} {path} HTTP/1.1\" {status} {bytes} \"{}\" \"{}\"",
            ips[ip.pick(&mut rng)],
            stamp.1,
            REFERRERS[referrer.sample(&mut rng)].0,
            AGENTS[agent.sample(&mut rng)].0,
        )?;
    }
    out.flush()
}
