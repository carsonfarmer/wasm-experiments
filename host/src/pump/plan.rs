//! Row layouts, copy plans and projections.
//!
//! In the canonical ABI a value is a fixed-size block of bytes, plus heap
//! data reachable through `(ptr, len)` pairs for strings and lists. A
//! [`Layout`] is a host-owned description of that block (field names,
//! offsets, kinds), built once from a component's type. From it:
//!
//! - a [`Plan`] lists where the `(ptr, len)` pairs are, so copying a row is a
//!   `memcpy` of the block plus following each pointer (types with no
//!   pointers copy as one `memcpy` per batch);
//! - a [`Proj`] maps a consumer's row type onto a producer's when the
//!   consumer asks for a subset of a record's fields, so fields nobody reads
//!   are never copied.

use anyhow::{Result, bail};
use std::sync::Arc;
use wasmtime::component::__internal::{ComponentTypes, InterfaceType};

/// A host-owned description of a canonical-ABI value.
#[derive(Debug)]
pub struct Layout {
    pub size: usize,
    pub align: u32,
    pub kind: Kind,
}

#[derive(Debug)]
pub enum Kind {
    /// A scalar, identified by its WIT name (`u8`, `s64`, `f32`, `char`, ...).
    Prim(&'static str),
    Str,
    List(Arc<Layout>),
    Record(Vec<Field>),
    Tuple(Vec<(usize, Arc<Layout>)>),
    /// Variants, options, results and enums: a discriminant, then a payload.
    Case { disc_size: usize, payload_off: usize, cases: Vec<(String, Option<Arc<Layout>>)> },
    Flags(Vec<String>),
}

#[derive(Debug)]
pub struct Field {
    pub name: String,
    pub off: usize,
    pub layout: Arc<Layout>,
}

fn align_to(n: usize, align: u32) -> usize {
    let a = align as usize;
    n.div_ceil(a) * a
}

pub fn discriminant_size(cases: usize) -> usize {
    match cases {
        0..=0xff => 1,
        0x100..=0xffff => 2,
        _ => 4,
    }
}

/// Describe values of type `ty`.
pub fn layout(types: &ComponentTypes, ty: &InterfaceType) -> Result<Arc<Layout>> {
    use InterfaceType as T;
    let abi = types.canonical_abi(ty);
    let prim = |name| Kind::Prim(name);
    let kind = match ty {
        T::Bool => prim("bool"),
        T::S8 => prim("s8"),
        T::U8 => prim("u8"),
        T::S16 => prim("s16"),
        T::U16 => prim("u16"),
        T::S32 => prim("s32"),
        T::U32 => prim("u32"),
        T::S64 => prim("s64"),
        T::U64 => prim("u64"),
        T::Float32 => prim("f32"),
        T::Float64 => prim("f64"),
        T::Char => prim("char"),
        T::String => Kind::Str,
        T::List(i) => Kind::List(layout(types, &types[*i].element)?),
        T::Record(i) => {
            let mut off = 0;
            let mut fields = Vec::new();
            for f in types[*i].fields.iter() {
                let l = layout(types, &f.ty)?;
                off = align_to(off, l.align);
                fields.push(Field { name: f.name.clone(), off, layout: l.clone() });
                off += l.size;
            }
            Kind::Record(fields)
        }
        T::Tuple(i) => {
            let mut off = 0;
            let mut items = Vec::new();
            for t in types[*i].types.iter() {
                let l = layout(types, t)?;
                off = align_to(off, l.align);
                items.push((off, l.clone()));
                off += l.size;
            }
            Kind::Tuple(items)
        }
        T::Variant(i) => {
            let v = &types[*i];
            let cases = v
                .cases
                .iter()
                .map(|(name, t)| Ok((name.clone(), t.map(|t| layout(types, &t)).transpose()?)))
                .collect::<Result<Vec<_>>>()?;
            case(cases, v.info.payload_offset32)
        }
        T::Option(i) => {
            let o = &types[*i];
            case(vec![("none".into(), None), ("some".into(), Some(layout(types, &o.ty)?))], o.info.payload_offset32)
        }
        T::Result(i) => {
            let r = &types[*i];
            let ok = r.ok.map(|t| layout(types, &t)).transpose()?;
            let err = r.err.map(|t| layout(types, &t)).transpose()?;
            case(vec![("ok".into(), ok), ("err".into(), err)], r.info.payload_offset32)
        }
        T::Enum(i) => {
            let e = &types[*i];
            case(e.names.iter().map(|n| (n.clone(), None)).collect(), e.info.payload_offset32)
        }
        T::Flags(i) => Kind::Flags(types[*i].names.iter().cloned().collect()),
        T::Own(_) | T::Borrow(_) => bail!("resource handles can't be copied by the host yet"),
        T::Future(_) | T::Stream(_) | T::ErrorContext(_) => {
            bail!("futures, streams and error-contexts inside rows aren't supported")
        }
        T::Map(_) | T::FixedLengthList(_) => bail!("maps and fixed-length lists aren't supported yet"),
    };
    Ok(Arc::new(Layout { size: abi.size32 as usize, align: abi.align32, kind }))
}

fn case(cases: Vec<(String, Option<Arc<Layout>>)>, payload_off: u32) -> Kind {
    Kind::Case { disc_size: discriminant_size(cases.len()), payload_off: payload_off as usize, cases }
}

/// Structural equality, including field and case names.
pub fn same(a: &Layout, b: &Layout) -> bool {
    if a.size != b.size || a.align != b.align {
        return false;
    }
    match (&a.kind, &b.kind) {
        (Kind::Prim(x), Kind::Prim(y)) => x == y,
        (Kind::Str, Kind::Str) => true,
        (Kind::List(x), Kind::List(y)) => same(x, y),
        (Kind::Record(x), Kind::Record(y)) => {
            x.len() == y.len()
                && x.iter().zip(y).all(|(f, g)| f.name == g.name && f.off == g.off && same(&f.layout, &g.layout))
        }
        (Kind::Tuple(x), Kind::Tuple(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|((o, f), (p, g))| o == p && same(f, g))
        }
        (Kind::Case { cases: x, .. }, Kind::Case { cases: y, .. }) => {
            x.len() == y.len()
                && x.iter().zip(y).all(|((n, f), (m, g))| {
                    n == m
                        && match (f, g) {
                            (None, None) => true,
                            (Some(f), Some(g)) => same(f, g),
                            _ => false,
                        }
                })
        }
        (Kind::Flags(x), Kind::Flags(y)) => x == y,
        _ => false,
    }
}

/// The pointer-bearing locations of one value, relative to its start.
#[derive(Debug)]
pub struct Plan {
    pub size: usize,
    pub align: u32,
    pub fixes: Vec<Fix>,
}

#[derive(Debug)]
pub enum Fix {
    /// A UTF-8 `string`: `(ptr, byte_len)` at `off`.
    Str { off: usize },
    /// A `list<T>`: `(ptr, len)` at `off`; `elem` describes each element.
    List { off: usize, elem: Arc<Plan> },
    /// A variant/option/result whose payload holds pointers in some cases.
    /// `cases[d]` is the payload plan for discriminant `d` (`None` = nothing to fix).
    Case { off: usize, disc_size: usize, payload_off: usize, cases: Vec<Option<Arc<Plan>>> },
}

impl Plan {
    pub fn is_flat(&self) -> bool {
        self.fixes.is_empty()
    }
}

/// Which parts of a value the consumer will read.
#[derive(Clone, Debug, PartialEq)]
pub enum Need {
    All,
    /// Only these record fields (each with its own need).
    Fields(Vec<(String, Need)>),
}

/// Compile the copy plan for values described by `l`.
pub fn plan(l: &Layout) -> Plan {
    plan_for(l, &Need::All)
}

/// Compile a copy plan that only follows the pointers `need` asks for;
/// the rest of the fixed block is copied but never dereferenced.
pub fn plan_for(l: &Layout, need: &Need) -> Plan {
    let mut fixes = Vec::new();
    collect(l, need, 0, &mut fixes);
    Plan { size: l.size, align: l.align, fixes }
}

fn collect(l: &Layout, need: &Need, base: usize, fixes: &mut Vec<Fix>) {
    match &l.kind {
        Kind::Prim(_) | Kind::Flags(_) => {}
        Kind::Str => fixes.push(Fix::Str { off: base }),
        Kind::List(e) => fixes.push(Fix::List { off: base, elem: Arc::new(plan(e)) }),
        Kind::Record(fields) => {
            for f in fields {
                let sub = match need {
                    Need::All => Some(&Need::All),
                    Need::Fields(want) => want.iter().find(|(n, _)| *n == f.name).map(|(_, n)| n),
                };
                if let Some(sub) = sub {
                    collect(&f.layout, sub, base + f.off, fixes);
                }
            }
        }
        Kind::Tuple(items) => {
            for (off, l) in items {
                collect(l, &Need::All, base + off, fixes);
            }
        }
        Kind::Case { disc_size, payload_off, cases } => {
            let plans: Vec<Option<Arc<Plan>>> = cases
                .iter()
                .map(|(_, p)| p.as_ref().map(|l| plan(l)).filter(|p| !p.is_flat()).map(Arc::new))
                .collect();
            if plans.iter().any(Option::is_some) {
                fixes.push(Fix::Case { off: base, disc_size: *disc_size, payload_off: base + payload_off, cases: plans });
            }
        }
    }
}

/// How to build a consumer's value from a staged producer value.
#[derive(Debug)]
pub enum Proj {
    /// Same shape: copy `plan.size` bytes, then follow `plan`'s pointers.
    Copy(Arc<Plan>),
    /// The consumer's record takes some of the producer record's fields:
    /// `(consumer offset, producer offset, how)` for each.
    Fields(Vec<(usize, usize, Proj)>),
}

/// Map consumer layout `dst` onto producer layout `src`. Records may be
/// narrowed to a subset of their fields (matched by name, at any depth);
/// everything else must match exactly.
pub fn project(dst: &Layout, src: &Layout) -> Result<Proj> {
    if same(dst, src) {
        return Ok(Proj::Copy(Arc::new(plan(src))));
    }
    match (&dst.kind, &src.kind) {
        (Kind::Record(want), Kind::Record(have)) => {
            let mut fields = Vec::with_capacity(want.len());
            for w in want {
                let Some(h) = have.iter().find(|h| h.name == w.name) else {
                    bail!("the producer has no field `{}`", w.name);
                };
                let p = project(&w.layout, &h.layout).map_err(|e| anyhow::anyhow!("field `{}`: {e}", w.name))?;
                fields.push((w.off, h.off, p));
            }
            Ok(Proj::Fields(fields))
        }
        _ => bail!("types differ"),
    }
}
