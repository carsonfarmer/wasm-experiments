//! Moving stream elements whose type the host only learns at runtime.
//!
//! Wasmtime 49's public stream API needs a Rust type `T: Lift + Lower` for
//! every element the host reads or writes (`StreamReader<T>`, `Source<T>`).
//! There is no `Val`-typed stream yet, and `StreamAny` can only be passed
//! through untouched. So a host built-in like `take` cannot see elements of
//! a type it discovered at runtime through the public API alone.
//!
//! The workaround here: [`Elem<SIZE, ALIGN>`] is a stand-in element type whose
//! canonical-ABI size and alignment are const generics, and whose lift/lower
//! walk the *runtime* type description (`InterfaceType` + `ComponentTypes`)
//! to deep-copy values (strings, lists, nested records) out of one guest's
//! memory and into another's. It relies on Wasmtime's `#[doc(hidden)]`
//! `__internal` hooks, which is fine for a spike and exactly the gap a real
//! `StreamReader<Val>` would close upstream.

use anyhow::{Result, bail};
use std::mem::MaybeUninit;
use wasmtime::ValRaw;
use wasmtime::component::__internal::wasmtime_environ::component::StringEncoding;
use wasmtime::component::__internal::{
    CanonicalAbiInfo, ComponentTypes, InstanceType, InterfaceType, LiftContext, LowerContext,
};
use wasmtime::component::{ComponentType, Lift, Lower, Type};

/// An owned, deep copy of one canonical-ABI value.
#[derive(Clone, Debug)]
pub enum Dyn {
    /// Any fixed-size scalar (bool, ints, floats, char), stored little-endian.
    Scalar([u8; 8]),
    String(Box<str>),
    List(Vec<Dyn>),
    /// Records and tuples: fields in declaration order.
    Fields(Vec<Dyn>),
    /// Variants, options, results and enums: discriminant plus optional payload.
    Case(u32, Option<Box<Dyn>>),
    /// Flags: the raw bit bytes.
    Bits(Vec<u8>),
}

/// A stream element with canonical-ABI size `S` and alignment `A`, carrying a
/// value of any type with that layout.
#[derive(Clone, Debug)]
pub struct Elem<const S: u32, const A: u32>(pub Dyn);

unsafe impl<const S: u32, const A: u32> ComponentType for Elem<S, A> {
    // Stream elements are always passed in memory, never flattened.
    type Lower = [ValRaw; 1];

    const ABI: CanonicalAbiInfo = CanonicalAbiInfo {
        size32: S,
        align32: A,
        size64: S,
        align64: A,
        flat_count: None,
    };

    fn typecheck(ty: &InterfaceType, types: &InstanceType<'_>) -> wasmtime::Result<()> {
        let abi = types.types.canonical_abi(ty);
        if abi.size32 != S || abi.align32 != A {
            wasmtime::bail!(
                "layout mismatch: element is {}/{}, expected {S}/{A}",
                abi.size32,
                abi.align32
            );
        }
        supported(types.types, ty).map_err(|e| wasmtime::format_err!("{e}"))
    }
}

unsafe impl<const S: u32, const A: u32> Lift for Elem<S, A> {
    fn linear_lift_from_flat(
        _cx: &mut LiftContext<'_>,
        _ty: InterfaceType,
        _src: &Self::Lower,
    ) -> wasmtime::Result<Self> {
        wasmtime::bail!("Elem is only lifted from memory")
    }

    fn linear_lift_from_memory(
        cx: &mut LiftContext<'_>,
        ty: InterfaceType,
        bytes: &[u8],
    ) -> wasmtime::Result<Self> {
        if !matches!(cx.options().string_encoding, StringEncoding::Utf8) {
            wasmtime::bail!("spike only supports UTF-8 components");
        }
        let mem = cx.memory();
        // `bytes` is a window into `mem`; recover its absolute offset so that
        // nested pointers (strings, lists) can be followed.
        let ptr = bytes.as_ptr() as usize - mem.as_ptr() as usize;
        let v = load(cx.types, mem, &ty, ptr).map_err(|e| wasmtime::format_err!("{e}"))?;
        Ok(Elem(v))
    }
}

unsafe impl<const S: u32, const A: u32> Lower for Elem<S, A> {
    fn linear_lower_to_flat<T>(
        &self,
        _cx: &mut LowerContext<'_, T>,
        _ty: InterfaceType,
        _dst: &mut MaybeUninit<Self::Lower>,
    ) -> wasmtime::Result<()> {
        wasmtime::bail!("Elem is only lowered to memory")
    }

    fn linear_lower_to_memory<T>(
        &self,
        cx: &mut LowerContext<'_, T>,
        ty: InterfaceType,
        offset: usize,
    ) -> wasmtime::Result<()> {
        if !matches!(cx.options().string_encoding, StringEncoding::Utf8) {
            wasmtime::bail!("spike only supports UTF-8 components");
        }
        store(cx, &ty, offset, &self.0)
    }
}

// ---------------------------------------------------------------------------
// Canonical ABI walking (memory32, UTF-8 strings).

fn align_to(n: usize, align: u32) -> usize {
    let a = align as usize;
    n.div_ceil(a) * a
}

fn u32_at(mem: &[u8], ptr: usize) -> Result<u32> {
    let b = mem.get(ptr..ptr + 4).ok_or_else(|| anyhow::anyhow!("out of bounds"))?;
    Ok(u32::from_le_bytes(b.try_into().unwrap()))
}

fn scalar_size(ty: &InterfaceType) -> Option<usize> {
    use InterfaceType::*;
    Some(match ty {
        Bool | S8 | U8 => 1,
        S16 | U16 => 2,
        S32 | U32 | Float32 | Char => 4,
        S64 | U64 | Float64 => 8,
        _ => return None,
    })
}

fn discriminant_size(cases: usize) -> usize {
    match cases {
        0..=0xff => 1,
        0x100..=0xffff => 2,
        _ => 4,
    }
}

/// Is every part of `ty` something this spike can deep-copy?
fn supported(types: &ComponentTypes, ty: &InterfaceType) -> Result<()> {
    use InterfaceType::*;
    match ty {
        _ if scalar_size(ty).is_some() => Ok(()),
        String | Enum(_) | Flags(_) => Ok(()),
        List(i) => supported(types, &types[*i].element),
        Record(i) => types[*i].fields.iter().try_for_each(|f| supported(types, &f.ty)),
        Tuple(i) => types[*i].types.iter().try_for_each(|t| supported(types, t)),
        Variant(i) => types[*i].cases.values().flatten().try_for_each(|t| supported(types, t)),
        Option(i) => supported(types, &types[*i].ty),
        Result(i) => {
            let r = &types[*i];
            r.ok.iter().chain(r.err.iter()).try_for_each(|t| supported(types, t))
        }
        Own(_) | Borrow(_) => bail!("resources can't be moved by the host yet"),
        Future(_) | Stream(_) | ErrorContext(_) => bail!("nested futures/streams not supported"),
        Map(_) | FixedLengthList(_) => bail!("maps and fixed-length lists not supported yet"),
        _ => bail!("unsupported type"),
    }
}

/// Discriminant and payload offset for a variant-like type.
fn case_layout(types: &ComponentTypes, ty: &InterfaceType) -> (usize, usize) {
    use InterfaceType::*;
    let (n, off) = match ty {
        Variant(i) => (types[*i].cases.len(), types[*i].info.payload_offset32),
        Option(i) => (2, types[*i].info.payload_offset32),
        Result(i) => (2, types[*i].info.payload_offset32),
        Enum(i) => (types[*i].names.len(), types[*i].info.payload_offset32),
        _ => unreachable!(),
    };
    (discriminant_size(n), off as usize)
}

fn case_payload(types: &ComponentTypes, ty: &InterfaceType, disc: u32) -> Result<Option<InterfaceType>> {
    use InterfaceType::*;
    Ok(match ty {
        Variant(i) => match types[*i].cases.get_index(disc as usize) {
            Some((_, t)) => *t,
            None => bail!("invalid variant discriminant {disc}"),
        },
        Option(i) => match disc {
            0 => None,
            1 => Some(types[*i].ty),
            _ => bail!("invalid option discriminant {disc}"),
        },
        Result(i) => match disc {
            0 => types[*i].ok,
            1 => types[*i].err,
            _ => bail!("invalid result discriminant {disc}"),
        },
        Enum(_) => None,
        _ => unreachable!(),
    })
}

/// Deep-copy the value of type `ty` at `ptr` out of guest memory.
fn load(types: &ComponentTypes, mem: &[u8], ty: &InterfaceType, ptr: usize) -> Result<Dyn> {
    use InterfaceType::*;
    if let Some(n) = scalar_size(ty) {
        let mut b = [0u8; 8];
        b[..n].copy_from_slice(mem.get(ptr..ptr + n).ok_or_else(|| anyhow::anyhow!("out of bounds"))?);
        return Ok(Dyn::Scalar(b));
    }
    Ok(match ty {
        String => {
            let (p, len) = (u32_at(mem, ptr)? as usize, u32_at(mem, ptr + 4)? as usize);
            let bytes = mem.get(p..p + len).ok_or_else(|| anyhow::anyhow!("string out of bounds"))?;
            Dyn::String(std::str::from_utf8(bytes)?.into())
        }
        List(i) => {
            let elem = types[*i].element;
            let size = types.canonical_abi(&elem).size32 as usize;
            let (p, len) = (u32_at(mem, ptr)? as usize, u32_at(mem, ptr + 4)? as usize);
            Dyn::List(
                (0..len)
                    .map(|k| load(types, mem, &elem, p + k * size))
                    .collect::<anyhow::Result<_>>()?,
            )
        }
        Record(i) => Dyn::Fields(fields(types, types[*i].fields.iter().map(|f| f.ty), |t, off| {
            load(types, mem, &t, ptr + off)
        })?),
        Tuple(i) => Dyn::Fields(fields(types, types[*i].types.iter().copied(), |t, off| {
            load(types, mem, &t, ptr + off)
        })?),
        Variant(_) | Option(_) | Result(_) | Enum(_) => {
            let (dsize, payload_off) = case_layout(types, ty);
            let mut d = [0u8; 4];
            d[..dsize].copy_from_slice(mem.get(ptr..ptr + dsize).ok_or_else(|| anyhow::anyhow!("out of bounds"))?);
            let disc = u32::from_le_bytes(d);
            let payload = match case_payload(types, ty, disc)? {
                Some(t) => Some(Box::new(load(types, mem, &t, ptr + payload_off)?)),
                None => None,
            };
            Dyn::Case(disc, payload)
        }
        Flags(_) => {
            let n = types.canonical_abi(ty).size32 as usize;
            Dyn::Bits(mem.get(ptr..ptr + n).ok_or_else(|| anyhow::anyhow!("out of bounds"))?.to_vec())
        }
        _ => bail!("unsupported type"),
    })
}

/// Visit record/tuple fields with their canonical-ABI offsets.
fn fields(
    types: &ComponentTypes,
    tys: impl Iterator<Item = InterfaceType>,
    mut f: impl FnMut(InterfaceType, usize) -> Result<Dyn>,
) -> Result<Vec<Dyn>> {
    let mut off = 0usize;
    tys.map(|t| {
        let abi = types.canonical_abi(&t);
        off = align_to(off, abi.align32);
        let v = f(t, off)?;
        off += abi.size32 as usize;
        Ok(v)
    })
    .collect()
}

fn write<T>(cx: &mut LowerContext<'_, T>, ptr: usize, bytes: &[u8]) -> wasmtime::Result<()> {
    let mem = cx.as_slice_mut();
    let dst = mem
        .get_mut(ptr..ptr + bytes.len())
        .ok_or_else(|| wasmtime::format_err!("out of bounds"))?;
    dst.copy_from_slice(bytes);
    Ok(())
}

/// Deep-copy `val` (of type `ty`) into guest memory at `ptr`, allocating
/// strings and lists with the guest's `realloc`.
fn store<T>(cx: &mut LowerContext<'_, T>, ty: &InterfaceType, ptr: usize, val: &Dyn) -> wasmtime::Result<()> {
    use InterfaceType::*;
    if let (Some(n), Dyn::Scalar(b)) = (scalar_size(ty), val) {
        return write(cx, ptr, &b[..n]);
    }
    match (ty, val) {
        (String, Dyn::String(s)) => {
            let p = cx.realloc(0, 0, 1, s.len())?;
            write(cx, p, s.as_bytes())?;
            write(cx, ptr, &(p as u32).to_le_bytes())?;
            write(cx, ptr + 4, &(s.len() as u32).to_le_bytes())
        }
        (List(i), Dyn::List(items)) => {
            let elem = cx.types[*i].element;
            let abi = cx.types.canonical_abi(&elem).clone();
            let p = cx.realloc(0, 0, abi.align32, abi.size32 as usize * items.len())?;
            for (k, v) in items.iter().enumerate() {
                store(cx, &elem, p + k * abi.size32 as usize, v)?;
            }
            write(cx, ptr, &(p as u32).to_le_bytes())?;
            write(cx, ptr + 4, &(items.len() as u32).to_le_bytes())
        }
        (Record(_) | Tuple(_), Dyn::Fields(vals)) => {
            let tys: Vec<InterfaceType> = match ty {
                Record(i) => cx.types[*i].fields.iter().map(|f| f.ty).collect(),
                Tuple(i) => cx.types[*i].types.to_vec(),
                _ => unreachable!(),
            };
            if tys.len() != vals.len() {
                wasmtime::bail!("field count mismatch");
            }
            let mut off = 0usize;
            for (t, v) in tys.iter().zip(vals) {
                let abi = cx.types.canonical_abi(t).clone();
                off = align_to(off, abi.align32);
                store(cx, t, ptr + off, v)?;
                off += abi.size32 as usize;
            }
            Ok(())
        }
        (Variant(_) | Option(_) | Result(_) | Enum(_), Dyn::Case(disc, payload)) => {
            let (dsize, payload_off) = case_layout(cx.types, ty);
            write(cx, ptr, &disc.to_le_bytes()[..dsize])?;
            match (case_payload(cx.types, ty, *disc).map_err(|e| wasmtime::format_err!("{e}"))?, payload) {
                (Some(t), Some(v)) => store(cx, &t, ptr + payload_off, v),
                (None, None) => Ok(()),
                _ => wasmtime::bail!("variant payload mismatch"),
            }
        }
        (Flags(_), Dyn::Bits(b)) => write(cx, ptr, b),
        _ => wasmtime::bail!("value does not match type"),
    }
}

// ---------------------------------------------------------------------------
// Layout of a public `Type`, used to pick the right `Elem<S, A>`.

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
