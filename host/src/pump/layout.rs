//! Canonical-ABI layouts of component types, built from Wasmtime's public
//! [`Type`]: sizes, alignments and field offsets, plus names for printing
//! and for matching fields by name. Equality is structural (derived).

use anyhow::{Result, bail};
use std::fmt;
use std::sync::Arc;
use wasmtime::component::Type;

#[derive(Debug, PartialEq)]
pub struct Layout {
    pub size: usize,
    pub align: usize,
    pub kind: Kind,
}

#[derive(Debug, PartialEq)]
pub enum Kind {
    /// A scalar, by its WIT name.
    Prim(&'static str),
    Str,
    List(Arc<Layout>),
    Record(Vec<Field>),
    Tuple(Vec<Field>),
    /// Variants, enums, options and results: a discriminant, then a payload.
    Case {
        of: CaseOf,
        disc: usize,
        payload: usize,
        cases: Vec<(String, Option<Arc<Layout>>)>,
    },
    Flags(Vec<String>),
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum CaseOf {
    Variant,
    Enum,
    Option,
    Result,
}

#[derive(Debug, PartialEq)]
pub struct Field {
    pub name: String,
    pub off: usize,
    pub layout: Arc<Layout>,
}

fn align_to(n: usize, align: usize) -> usize {
    n.div_ceil(align) * align
}

impl Layout {
    pub fn of(ty: &Type) -> Result<Arc<Layout>> {
        let prim = |name, n| Layout { size: n, align: n, kind: Kind::Prim(name) };
        Ok(Arc::new(match ty {
            Type::Bool => prim("bool", 1),
            Type::S8 => prim("s8", 1),
            Type::U8 => prim("u8", 1),
            Type::S16 => prim("s16", 2),
            Type::U16 => prim("u16", 2),
            Type::S32 => prim("s32", 4),
            Type::U32 => prim("u32", 4),
            Type::Char => prim("char", 4),
            Type::Float32 => prim("f32", 4),
            Type::S64 => prim("s64", 8),
            Type::U64 => prim("u64", 8),
            Type::Float64 => prim("f64", 8),
            Type::String => Layout { size: 8, align: 4, kind: Kind::Str },
            Type::List(l) => Layout { size: 8, align: 4, kind: Kind::List(Layout::of(&l.ty())?) },
            Type::Record(r) => fields(r.fields().map(|f| (f.name.to_string(), f.ty)), Kind::Record)?,
            Type::Tuple(t) => fields(t.types().enumerate().map(|(i, t)| (i.to_string(), t)), Kind::Tuple)?,
            Type::Variant(v) => case(CaseOf::Variant, v.cases().map(|c| (c.name.to_string(), c.ty)))?,
            Type::Enum(e) => case(CaseOf::Enum, e.names().map(|n| (n.to_string(), None)))?,
            Type::Option(o) => case(CaseOf::Option, [("none".into(), None), ("some".into(), Some(o.ty()))])?,
            Type::Result(r) => case(CaseOf::Result, [("ok".into(), r.ok()), ("err".into(), r.err())])?,
            Type::Flags(f) => {
                let names: Vec<String> = f.names().map(String::from).collect();
                let (size, align) = match names.len() {
                    0 => (0, 1),
                    1..=8 => (1, 1),
                    9..=16 => (2, 2),
                    n => (4 * n.div_ceil(32), 4),
                };
                Layout { size, align, kind: Kind::Flags(names) }
            }
            Type::Own(_) | Type::Borrow(_) => bail!("resource handles can't be copied by the host yet"),
            Type::Stream(_) | Type::Future(_) | Type::ErrorContext => {
                bail!("streams, futures and error-contexts inside rows aren't supported")
            }
            Type::Map(_) | Type::FixedLengthList(_) => bail!("maps and fixed-length lists aren't supported yet"),
        }))
    }

    /// No strings or lists anywhere: copying is a plain `memcpy`.
    pub fn is_flat(&self) -> bool {
        match &self.kind {
            Kind::Str | Kind::List(_) => false,
            Kind::Record(fs) | Kind::Tuple(fs) => fs.iter().all(|f| f.layout.is_flat()),
            Kind::Case { cases, .. } => cases.iter().all(|(_, p)| p.as_ref().is_none_or(|p| p.is_flat())),
            Kind::Prim(_) | Kind::Flags(_) => true,
        }
    }
}

/// Lay fields out one after another, each at its own alignment.
fn fields(items: impl Iterator<Item = (String, Type)>, kind: fn(Vec<Field>) -> Kind) -> Result<Layout> {
    let (mut off, mut align, mut out) = (0, 1, Vec::new());
    for (name, ty) in items {
        let layout = Layout::of(&ty)?;
        off = align_to(off, layout.align);
        align = align.max(layout.align);
        out.push(Field { name, off, layout: layout.clone() });
        off += layout.size;
    }
    Ok(Layout { size: align_to(off, align), align, kind: kind(out) })
}

/// A discriminant sized for the number of cases, then the largest payload.
fn case(of: CaseOf, items: impl IntoIterator<Item = (String, Option<Type>)>) -> Result<Layout> {
    let cases: Vec<(String, Option<Arc<Layout>>)> =
        items.into_iter().map(|(n, t)| Ok((n, t.as_ref().map(Layout::of).transpose()?))).collect::<Result<_>>()?;
    let disc = match cases.len() {
        0..=0xff => 1,
        0x100..=0xffff => 2,
        _ => 4,
    };
    let payloads = cases.iter().filter_map(|(_, p)| p.as_deref());
    let (size, palign) = payloads.fold((0, 1), |(s, a), p| (s.max(p.size), a.max(p.align)));
    let align = disc.max(palign);
    let payload = align_to(disc, palign);
    Ok(Layout { size: align_to(payload + size, align), align, kind: Kind::Case { of, disc, payload, cases } })
}

/// Prints the type the way WIT spells it.
impl fmt::Display for Layout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let list = |f: &mut fmt::Formatter<'_>, items: Vec<String>| write!(f, "{}", items.join(", "));
        match &self.kind {
            Kind::Prim(p) => write!(f, "{p}"),
            Kind::Str => write!(f, "string"),
            Kind::List(e) => write!(f, "list<{e}>"),
            Kind::Record(fs) => {
                write!(f, "record {{ ")?;
                list(f, fs.iter().map(|x| format!("{}: {}", x.name, x.layout)).collect())?;
                write!(f, " }}")
            }
            Kind::Tuple(fs) => {
                write!(f, "tuple<")?;
                list(f, fs.iter().map(|x| x.layout.to_string()).collect())?;
                write!(f, ">")
            }
            Kind::Flags(names) => write!(f, "flags {{ {} }}", names.join(", ")),
            Kind::Case { of, cases, .. } => {
                let ty = |i: usize| cases[i].1.as_ref().map_or("_".to_string(), |t| t.to_string());
                match of {
                    CaseOf::Option => write!(f, "option<{}>", ty(1)),
                    CaseOf::Result => write!(f, "result<{}, {}>", ty(0), ty(1)),
                    CaseOf::Enum => {
                        write!(f, "enum {{ {} }}", cases.iter().map(|c| c.0.as_str()).collect::<Vec<_>>().join(", "))
                    }
                    CaseOf::Variant => {
                        write!(f, "variant {{ ")?;
                        list(
                            f,
                            cases.iter().map(|(n, p)| p.as_ref().map_or(n.clone(), |p| format!("{n}({p})"))).collect(),
                        )?;
                        write!(f, " }}")
                    }
                }
            }
        }
    }
}
