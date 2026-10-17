// Deterministic rows for the type zoo, shared (via `include!`) by the
// producer and both checkers so they agree on every value. Every kind of
// value the host pump copies appears: scalars, unicode strings and chars,
// nested lists, lists of records with optional strings, variants with and
// without payloads, results, enums, flags, tuples and empty lists.

use fastrand::Rng;
use wp::demo::menagerie::{Color, Perms, Point, Shape};

const WORDS: [&str; 9] = [
    "alpha", "βeta", "gamma 🦀", "", "delta-delta", "épsilon", "zeta\nline", "η",
    "a much longer string that spans several dozen bytes of utf-8 text ✓",
];
const LETTERS: [char; 6] = ['a', 'é', '🦀', 'Z', '\u{10FFFF}', '中'];

fn word(r: &mut Rng) -> String {
    WORDS[r.usize(..WORDS.len())].to_string()
}

/// Up to `max` values from `f`.
fn some<T>(r: &mut Rng, max: usize, mut f: impl FnMut(&mut Rng) -> T) -> Vec<T> {
    (0..r.usize(..=max)).map(|_| f(r)).collect()
}

fn point(r: &mut Rng) -> Point {
    Point { x: r.i32(..), y: -r.i32(0..1000), tag: r.bool().then(|| word(r)) }
}

pub fn row(seed: u64, i: u64) -> Row {
    let r = &mut Rng::with_seed(seed ^ i.wrapping_mul(0x2545_f491_4f6c_dd1d));
    let shape = match i % 5 {
        0 => Shape::Circle(i as f64 * 1.5),
        1 => Shape::Rect((r.u32(..), r.u32(..7))),
        2 => Shape::Label(word(r)),
        3 => Shape::Points(some(r, 3, point)),
        _ => Shape::Empty,
    };
    let perms = [(2, Perms::READ), (3, Perms::WRITE), (7, Perms::EXEC)]
        .into_iter()
        .filter(|(k, _)| i.is_multiple_of(*k))
        .fold(Perms::empty(), |p, (_, f)| p | f);
    Row {
        id: i,
        flag: i % 2 == 1,
        tiny: i as u8 as i8,
        small: (i * 7) as u16,
        medium: -(i as i32) * 3,
        single: i as f32 * 0.5 - 3.25,
        double: r.u64(..) as f64 / 7.0,
        letter: LETTERS[(i % 6) as usize],
        name: format!("row-{i}-{}", word(r)),
        tags: some(r, 4, word),
        nums: some(r, 5, |r| r.u32(..)),
        nested: some(r, 2, |r| some(r, 3, word)),
        points: some(r, 3, point),
        maybe: (!i.is_multiple_of(3)).then(|| word(r)),
        outcome: if i.is_multiple_of(4) { Err(i as u32) } else { Ok(word(r)) },
        check: if i.is_multiple_of(5) { Err(word(r)) } else { Ok(()) },
        shape,
        color: [Color::Red, Color::Green, Color::Blue][(i % 3) as usize],
        perms,
        pair: (word(r), some(r, 8, |r| r.u8(..))),
        bytes: some(r, 39, |r| r.u8(..)),
        nothing: Vec::new(),
    }
}
