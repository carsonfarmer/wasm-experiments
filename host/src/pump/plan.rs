//! Copy plans: a row type compiled into the list of places that hold pointers.
//!
//! In the canonical ABI a value is a fixed-size block of bytes, plus heap
//! data reachable through `(ptr, len)` pairs for strings and lists. A plan
//! records where those pairs are, so copying a row is: `memcpy` the fixed
//! block, then follow each pointer. A type with no pointers (all scalars,
//! enums, flags, and records of them) has an empty plan and copies as one
//! `memcpy` per batch.

use anyhow::{Result, bail};
use std::sync::Arc;
use wasmtime::component::__internal::{ComponentTypes, InterfaceType};

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
    /// A variant, option or result whose payload holds pointers in some cases.
    /// `cases[d]` is the payload plan for discriminant `d` (`None` = nothing to fix).
    Case { off: usize, disc_size: usize, payload_off: usize, cases: Vec<Option<Arc<Plan>>> },
}

impl Plan {
    pub fn is_flat(&self) -> bool {
        self.fixes.is_empty()
    }
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

/// Compile the plan for values of type `ty`.
pub fn compile(types: &ComponentTypes, ty: &InterfaceType) -> Result<Plan> {
    let abi = types.canonical_abi(ty);
    let mut fixes = Vec::new();
    collect(types, ty, 0, &mut fixes)?;
    Ok(Plan { size: abi.size32 as usize, align: abi.align32, fixes })
}

fn collect(types: &ComponentTypes, ty: &InterfaceType, base: usize, fixes: &mut Vec<Fix>) -> Result<()> {
    use InterfaceType as T;
    match ty {
        T::Bool | T::S8 | T::U8 | T::S16 | T::U16 | T::S32 | T::U32 | T::S64 | T::U64 => {}
        T::Float32 | T::Float64 | T::Char | T::Enum(_) | T::Flags(_) => {}
        T::String => fixes.push(Fix::Str { off: base }),
        T::List(i) => {
            let elem = compile(types, &types[*i].element)?;
            fixes.push(Fix::List { off: base, elem: Arc::new(elem) });
        }
        T::Record(i) => fields(types, types[*i].fields.iter().map(|f| f.ty), base, fixes)?,
        T::Tuple(i) => fields(types, types[*i].types.iter().copied(), base, fixes)?,
        T::Variant(i) => {
            let v = &types[*i];
            let payloads: Vec<Option<InterfaceType>> = v.cases.values().copied().collect();
            case(types, &payloads, v.info.payload_offset32 as usize, base, fixes)?;
        }
        T::Option(i) => {
            let o = &types[*i];
            case(types, &[None, Some(o.ty)], o.info.payload_offset32 as usize, base, fixes)?;
        }
        T::Result(i) => {
            let r = &types[*i];
            case(types, &[r.ok, r.err], r.info.payload_offset32 as usize, base, fixes)?;
        }
        T::Own(_) | T::Borrow(_) => bail!("resource handles can't be copied by the host yet"),
        T::Future(_) | T::Stream(_) | T::ErrorContext(_) => {
            bail!("futures, streams and error-contexts inside rows aren't supported")
        }
        T::Map(_) | T::FixedLengthList(_) => bail!("maps and fixed-length lists aren't supported yet"),
    }
    Ok(())
}

fn fields(
    types: &ComponentTypes,
    tys: impl Iterator<Item = InterfaceType>,
    base: usize,
    fixes: &mut Vec<Fix>,
) -> Result<()> {
    let mut off = 0usize;
    for t in tys {
        let abi = types.canonical_abi(&t);
        off = align_to(off, abi.align32);
        collect(types, &t, base + off, fixes)?;
        off += abi.size32 as usize;
    }
    Ok(())
}

fn case(
    types: &ComponentTypes,
    payloads: &[Option<InterfaceType>],
    payload_off: usize,
    base: usize,
    fixes: &mut Vec<Fix>,
) -> Result<()> {
    let mut cases = Vec::with_capacity(payloads.len());
    let mut any = false;
    for p in payloads {
        let plan = match p {
            Some(t) => {
                let plan = compile(types, t)?;
                (!plan.is_flat()).then(|| Arc::new(plan))
            }
            None => None,
        };
        any |= plan.is_some();
        cases.push(plan);
    }
    if any {
        fixes.push(Fix::Case {
            off: base,
            disc_size: discriminant_size(payloads.len()),
            payload_off: base + payload_off,
            cases,
        });
    }
    Ok(())
}
