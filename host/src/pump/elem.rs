//! `Elem<SIZE, ALIGN>`: the element type the host uses to read and write
//! streams whose row type it only learns at runtime.
//!
//! Wasmtime's public stream API needs a Rust type `T: Lift + Lower` with a
//! compile-time size for every element the host touches; there is no `Val`
//! stream. `Elem` is a small handle (stage pointer + row index) whose
//! canonical-ABI size and alignment are const generics. Lifting copies a row
//! into the current [`Stage`]; lowering copies rows back out of one. This
//! uses Wasmtime's `#[doc(hidden)]` lift/lower hooks, so it is pinned to one
//! Wasmtime version.

use super::plan::Plan;
use super::stage::Stage;
use std::cell::Cell;
use std::mem::MaybeUninit;
use wasmtime::ValRaw;
use wasmtime::component::__internal::wasmtime_environ::component::StringEncoding;
use wasmtime::component::__internal::{CanonicalAbiInfo, InstanceType, InterfaceType, LiftContext, LowerContext};
use wasmtime::component::{ComponentType, Lift, Lower};

#[derive(Clone, Copy, Debug)]
pub struct Elem<const S: u32, const A: u32> {
    stage: *const Stage,
    idx: u32,
}

// SAFETY: an `Elem` only points into a `Stage` owned by the same buffer that
// holds the `Elem`, and is only dereferenced on the Store's thread.
unsafe impl<const S: u32, const A: u32> Send for Elem<S, A> {}
unsafe impl<const S: u32, const A: u32> Sync for Elem<S, A> {}

impl<const S: u32, const A: u32> Elem<S, A> {
    pub fn new(stage: &Stage, idx: usize) -> Self {
        Elem { stage, idx: idx as u32 }
    }
}

/// Where lifted rows go: set by the pump around `Source::read`.
pub struct LiftTarget<'a> {
    pub stage: &'a mut Stage,
    pub plan: &'a Plan,
    pub error: Option<anyhow::Error>,
}

thread_local! {
    static LIFT: Cell<*mut LiftTarget<'static>> = const { Cell::new(std::ptr::null_mut()) };
}

/// Run `f` with lifted rows going into `target`.
pub fn lifting_into<R>(target: &mut LiftTarget<'_>, f: impl FnOnce() -> R) -> R {
    LIFT.set(std::ptr::from_mut(target).cast());
    let r = f();
    LIFT.set(std::ptr::null_mut());
    r
}

fn utf8(encoding: StringEncoding) -> wasmtime::Result<()> {
    match encoding {
        StringEncoding::Utf8 => Ok(()),
        _ => wasmtime::bail!("only UTF-8 components are supported"),
    }
}

unsafe impl<const S: u32, const A: u32> ComponentType for Elem<S, A> {
    // Stream elements always live in memory; they're never flattened.
    type Lower = [ValRaw; 1];

    const ABI: CanonicalAbiInfo = CanonicalAbiInfo { size32: S, align32: A, size64: S, align64: A, flat_count: None };

    fn typecheck(ty: &InterfaceType, types: &InstanceType<'_>) -> wasmtime::Result<()> {
        let abi = types.types.canonical_abi(ty);
        if (abi.size32, abi.align32) != (S, A) {
            wasmtime::bail!("layout mismatch: element is {}/{}, expected {S}/{A}", abi.size32, abi.align32);
        }
        Ok(())
    }
}

unsafe impl<const S: u32, const A: u32> Lift for Elem<S, A> {
    fn linear_lift_from_flat(_: &mut LiftContext<'_>, _: InterfaceType, _: &Self::Lower) -> wasmtime::Result<Self> {
        wasmtime::bail!("stream rows are only lifted from memory")
    }

    fn linear_lift_from_memory(cx: &mut LiftContext<'_>, _: InterfaceType, bytes: &[u8]) -> wasmtime::Result<Self> {
        utf8(cx.options().string_encoding)?;
        // SAFETY: set by `lifting_into` for the duration of this read.
        let Some(target) = (unsafe { LIFT.get().as_mut() }) else {
            wasmtime::bail!("row lifted outside of a host pump");
        };
        let mem = cx.memory();
        // `bytes` is a window into `mem`; its offset lets us follow pointers.
        let ptr = bytes.as_ptr() as usize - mem.as_ptr() as usize;
        match target.stage.push_row(target.plan, mem, ptr) {
            Ok(idx) => Ok(Elem { stage: target.stage, idx }),
            Err(e) => {
                let msg = format!("{e:#}");
                target.error = Some(e);
                wasmtime::bail!("{msg}")
            }
        }
    }
}

unsafe impl<const S: u32, const A: u32> Lower for Elem<S, A> {
    fn linear_lower_to_flat<T>(
        &self,
        _: &mut LowerContext<'_, T>,
        _: InterfaceType,
        _: &mut MaybeUninit<Self::Lower>,
    ) -> wasmtime::Result<()> {
        wasmtime::bail!("stream rows are only lowered to memory")
    }

    fn linear_lower_to_memory<T>(
        &self,
        cx: &mut LowerContext<'_, T>,
        ty: InterfaceType,
        offset: usize,
    ) -> wasmtime::Result<()> {
        Self::linear_store_list_to_memory(cx, ty, offset, std::slice::from_ref(self))
    }

    /// Lower a run of consecutive rows from one stage at once.
    fn linear_store_list_to_memory<T>(
        cx: &mut LowerContext<'_, T>,
        _: InterfaceType,
        offset: usize,
        items: &[Self],
    ) -> wasmtime::Result<()> {
        let Some(first) = items.first() else { return Ok(()) };
        utf8(cx.options().string_encoding)?;
        // SAFETY: the stage is owned by the buffer these items came from,
        // which Wasmtime keeps alive while lowering them.
        let stage = unsafe { &*first.stage };
        stage.lower_rows(cx, first.idx as usize, items.len(), offset)
    }
}
