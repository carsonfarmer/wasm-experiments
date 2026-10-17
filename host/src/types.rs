//! Facts about component types the shell needs before running anything:
//! how to print them, their canonical-ABI layout, whether they contain
//! pointers, and whether they look like a columnar batch.

use crate::pump::{BatchShape, Col, Need};
use anyhow::{Result, bail};
use wasmtime::component::Type;

fn align_to(n: usize, align: u32) -> usize {
    let a = align as usize;
    n.div_ceil(a) * a
}

fn discriminant_size(cases: usize) -> usize {
    crate::pump::plan::discriminant_size(cases)
}

/// Canonical-ABI (size, align) of `ty` in a 32-bit memory.
pub fn layout(ty: &Type) -> Result<(u32, u32)> {
    fn record(tys: impl Iterator<Item = Type>) -> Result<(u32, u32)> {
        let (mut size, mut align) = (0u32, 1u32);
        for t in tys {
            let (s, a) = layout(&t)?;
            size = align_to(size as usize, a) as u32 + s;
            align = align.max(a);
        }
        Ok((align_to(size as usize, align) as u32, align))
    }
    fn variant(n: usize, payloads: impl Iterator<Item = Option<Type>>) -> Result<(u32, u32)> {
        let d = discriminant_size(n) as u32;
        let (mut size, mut align) = (0u32, d);
        for p in payloads.flatten() {
            let (s, a) = layout(&p)?;
            size = size.max(s);
            align = align.max(a);
        }
        let payload_off = align_to(d as usize, align) as u32;
        Ok((align_to((payload_off + size) as usize, align) as u32, align))
    }
    Ok(match ty {
        Type::Bool | Type::S8 | Type::U8 => (1, 1),
        Type::S16 | Type::U16 => (2, 2),
        Type::S32 | Type::U32 | Type::Float32 | Type::Char => (4, 4),
        Type::S64 | Type::U64 | Type::Float64 => (8, 8),
        Type::String | Type::List(_) => (8, 4),
        Type::Record(r) => record(r.fields().map(|f| f.ty))?,
        Type::Tuple(t) => record(t.types())?,
        Type::Variant(v) => variant(v.cases().len(), v.cases().map(|c| c.ty))?,
        Type::Enum(e) => variant(e.names().len(), std::iter::empty())?,
        Type::Option(o) => variant(2, [None, Some(o.ty())].into_iter())?,
        Type::Result(r) => variant(2, [r.ok(), r.err()].into_iter())?,
        Type::Flags(f) => match f.names().len() {
            0 => (0, 1),
            1..=8 => (1, 1),
            9..=16 => (2, 2),
            n => (4 * n.div_ceil(32) as u32, 4),
        },
        _ => bail!("the host can't forward {} values yet", describe(ty)),
    })
}

/// Render a component type the way WIT would spell it.
pub fn describe(ty: &Type) -> String {
    match ty {
        Type::Bool => "bool".into(),
        Type::S8 => "s8".into(),
        Type::U8 => "u8".into(),
        Type::S16 => "s16".into(),
        Type::U16 => "u16".into(),
        Type::S32 => "s32".into(),
        Type::U32 => "u32".into(),
        Type::S64 => "s64".into(),
        Type::U64 => "u64".into(),
        Type::Float32 => "f32".into(),
        Type::Float64 => "f64".into(),
        Type::Char => "char".into(),
        Type::String => "string".into(),
        Type::List(l) => format!("list<{}>", describe(&l.ty())),
        Type::Record(r) => {
            let fs: Vec<_> = r.fields().map(|f| format!("{}: {}", f.name, describe(&f.ty))).collect();
            format!("record {{ {} }}", fs.join(", "))
        }
        Type::Tuple(t) => format!("tuple<{}>", t.types().map(|t| describe(&t)).collect::<Vec<_>>().join(", ")),
        Type::Variant(v) => {
            let cs: Vec<_> = v
                .cases()
                .map(|c| match c.ty {
                    Some(t) => format!("{}({})", c.name, describe(&t)),
                    None => c.name.to_string(),
                })
                .collect();
            format!("variant {{ {} }}", cs.join(", "))
        }
        Type::Enum(e) => format!("enum {{ {} }}", e.names().collect::<Vec<_>>().join(", ")),
        Type::Option(o) => format!("option<{}>", describe(&o.ty())),
        Type::Result(r) => {
            let part = |t: Option<Type>| t.map(|t| describe(&t)).unwrap_or_else(|| "_".into());
            format!("result<{}, {}>", part(r.ok()), part(r.err()))
        }
        Type::Flags(f) => format!("flags {{ {} }}", f.names().collect::<Vec<_>>().join(", ")),
        Type::Stream(s) => match s.ty() {
            Some(t) => format!("stream<{}>", describe(&t)),
            None => "stream".into(),
        },
        Type::Future(f) => match f.ty() {
            Some(t) => format!("future<{}>", describe(&t)),
            None => "future".into(),
        },
        Type::Own(_) => "own<resource>".into(),
        Type::Borrow(_) => "borrow<resource>".into(),
        Type::ErrorContext => "error-context".into(),
        Type::Map(m) => format!("map<{}, {}>", describe(&m.key()), describe(&m.value())),
        Type::FixedLengthList(l) => format!("list<{}, {}>", describe(&l.ty()), l.len()),
    }
}


/// True if values of `ty` contain no pointers or handles, so Wasmtime can
/// move them between components with a single `memcpy`.
pub fn is_flat(ty: &Type) -> bool {
    match ty {
        Type::String | Type::List(_) | Type::Map(_) => false,
        Type::Own(_) | Type::Borrow(_) | Type::Stream(_) | Type::Future(_) | Type::ErrorContext => false,
        Type::Record(r) => r.fields().all(|f| is_flat(&f.ty)),
        Type::Tuple(t) => t.types().all(|t| is_flat(&t)),
        Type::Variant(v) => v.cases().all(|c| c.ty.as_ref().is_none_or(is_flat)),
        Type::Option(o) => is_flat(&o.ty()),
        Type::Result(r) => r.ok().as_ref().is_none_or(is_flat) && r.err().as_ref().is_none_or(is_flat),
        Type::FixedLengthList(l) => is_flat(&l.ty()),
        _ => true,
    }
}

/// Recognize a columnar batch: a record whose every field is a column, either
/// `list<T>` of a flat `T`, or a string column
/// `record { data: string, offsets: list<u32> }` (Arrow's UTF-8 layout).
pub fn batch_shape(ty: &Type) -> Option<BatchShape> {
    let Type::Record(r) = ty else { return None };
    let mut cols = Vec::new();
    let mut names = Vec::new();
    let mut off = 0usize;
    for f in r.fields() {
        names.push(f.name.to_string());
        let (size, align) = layout(&f.ty).ok()?;
        off = align_to(off, align);
        match &f.ty {
            Type::List(l) if is_flat(&l.ty()) => cols.push(Col::Prim { off }),
            Type::Record(sr) => {
                let mut data = None;
                let mut offsets = None;
                let mut sub = 0usize;
                for sf in sr.fields() {
                    let (s, a) = layout(&sf.ty).ok()?;
                    sub = align_to(sub, a);
                    match (sf.name, &sf.ty) {
                        ("data", Type::String) => data = Some(off + sub),
                        ("data", Type::List(l)) if matches!(l.ty(), Type::U8) => data = Some(off + sub),
                        ("offsets", Type::List(l)) if matches!(l.ty(), Type::U32) => offsets = Some(off + sub),
                        _ => return None,
                    }
                    sub += s as usize;
                }
                cols.push(Col::Str { data_off: data?, offsets_off: offsets? });
            }
            _ => return None,
        }
        off += size as usize;
    }
    (!cols.is_empty()).then_some(BatchShape { cols, names })
}

/// Can a consumer that declares `want` read a stream of `have`? Yes if the
/// types match, or if `want` is a record that takes a subset of `have`'s
/// fields (by name, at any depth); the host pump then copies only those.
pub fn accepts(want: &Type, have: &Type) -> bool {
    match (want, have) {
        (Type::Record(w), Type::Record(h)) => w.fields().all(|wf| {
            h.fields().any(|hf| hf.name == wf.name && accepts(&wf.ty, &hf.ty))
        }),
        _ => describe(want) == describe(have),
    }
}

/// What a consumer declaring `want` reads out of `have` rows.
pub fn need(want: &Type, have: &Type) -> Need {
    match (want, have) {
        (Type::Record(w), Type::Record(h)) if describe(want) != describe(have) => Need::Fields(
            w.fields()
                .filter_map(|wf| h.fields().find(|hf| hf.name == wf.name).map(|hf| (wf.name.to_string(), need(&wf.ty, &hf.ty))))
                .collect(),
        ),
        _ => Need::All,
    }
}

/// Field names (in declaration order) of a record type.
pub fn field_names(ty: &Type) -> Vec<String> {
    match ty {
        Type::Record(r) => r.fields().map(|f| f.name.to_string()).collect(),
        _ => Vec::new(),
    }
}
