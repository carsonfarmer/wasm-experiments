//! Copy plans and projections, compiled from [`Layout`]s.
//!
//! A value is a fixed-size block plus heap data behind `(ptr, len)` pairs
//! (strings and lists). A [`Plan`] lists where those pairs are, so copying a
//! row is a `memcpy` of the block plus following each pointer. A [`Proj`]
//! maps a consumer's row type onto the producer's: equal types are copied
//! whole, and a consumer record may take a subset of the producer record's
//! fields (by name, at any depth), so fields nobody reads are never copied.

use super::layout::{Kind, Layout};
use anyhow::{Result, bail};
use std::sync::Arc;

#[derive(Debug)]
pub struct Plan {
    pub size: usize,
    pub align: usize,
    pub fixes: Vec<Fix>,
}

#[derive(Debug)]
pub enum Fix {
    /// A UTF-8 `string`: `(ptr, byte_len)` at `off`.
    Str { off: usize },
    /// A `list<T>`: `(ptr, len)` at `off`; `elem` describes each element.
    List { off: usize, elem: Arc<Plan> },
    /// A variant whose payload holds pointers in some cases: `cases[d]` is
    /// the plan for discriminant `d` (`None` = nothing to follow).
    Case { off: usize, disc: usize, payload: usize, cases: Vec<Option<Arc<Plan>>> },
}

impl Plan {
    pub fn of(l: &Layout) -> Plan {
        let mut fixes = Vec::new();
        collect(l, 0, &mut fixes);
        Plan { size: l.size, align: l.align, fixes }
    }

    /// The plan for staging producer rows: only the pointers `proj` reads.
    pub fn lift(have: &Layout, proj: &Proj) -> Plan {
        let mut fixes = Vec::new();
        collect_read(have, proj, 0, &mut fixes);
        Plan { size: have.size, align: have.align, fixes }
    }

    pub fn is_flat(&self) -> bool {
        self.fixes.is_empty()
    }
}

fn collect(l: &Layout, base: usize, fixes: &mut Vec<Fix>) {
    match &l.kind {
        Kind::Prim(_) | Kind::Flags(_) => {}
        Kind::Str => fixes.push(Fix::Str { off: base }),
        Kind::List(e) => fixes.push(Fix::List { off: base, elem: Arc::new(Plan::of(e)) }),
        Kind::Record(fs) | Kind::Tuple(fs) => fs.iter().for_each(|f| collect(&f.layout, base + f.off, fixes)),
        Kind::Case { disc, payload, cases, .. } => {
            let plans: Vec<_> =
                cases.iter().map(|(_, p)| p.as_deref().map(Plan::of).filter(|p| !p.is_flat()).map(Arc::new)).collect();
            if plans.iter().any(Option::is_some) {
                fixes.push(Fix::Case { off: base, disc: *disc, payload: base + payload, cases: plans });
            }
        }
    }
}

fn collect_read(l: &Layout, proj: &Proj, base: usize, fixes: &mut Vec<Fix>) {
    match (proj, &l.kind) {
        (Proj::Fields(picks), Kind::Record(fs)) => {
            for p in picks {
                collect_read(&fs[p.field].layout, &p.proj, base + p.src, fixes);
            }
        }
        _ => collect(l, base, fixes),
    }
}

/// How to build a consumer's value from a staged producer value.
#[derive(Debug)]
pub enum Proj {
    /// Same type: copy `plan.size` bytes, then follow `plan`'s pointers.
    Copy(Arc<Plan>),
    /// The consumer's record takes some of the producer record's fields.
    Fields(Vec<Pick>),
}

#[derive(Debug)]
pub struct Pick {
    /// Offset in the consumer's record.
    pub dst: usize,
    /// Offset and index of the field in the producer's record.
    pub src: usize,
    pub field: usize,
    pub proj: Proj,
}

/// Map consumer layout `want` onto producer layout `have`, or fail if the
/// consumer asks for something the producer doesn't have.
pub fn project(want: &Layout, have: &Layout) -> Result<Proj> {
    if want == have {
        return Ok(Proj::Copy(Arc::new(Plan::of(have))));
    }
    let (Kind::Record(want), Kind::Record(have)) = (&want.kind, &have.kind) else { bail!("types differ") };
    let picks = want.iter().map(|w| {
        let Some(field) = have.iter().position(|h| h.name == w.name) else { bail!("no field `{}`", w.name) };
        let proj = project(&w.layout, &have[field].layout)?;
        Ok(Pick { dst: w.off, src: have[field].off, field, proj })
    });
    Ok(Proj::Fields(picks.collect::<Result<_>>()?))
}
