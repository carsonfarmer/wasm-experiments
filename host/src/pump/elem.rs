//! `Elem<SIZE, ALIGN>`: the element type the host uses to read and write
//! streams whose row type it only learns at runtime.
//!
//! Wasmtime's public stream API needs a Rust type `T: Lift + Lower` with a
//! compile-time size for every element the host touches; there is no `Val`
//! stream. `Elem` is a 16-byte handle (stage pointer + row index) whose
//! canonical-ABI size and alignment are const generics. Its lift copies a row
//! into the current [`Stage`]; its lower copies rows back out of one. The
//! actual bytes never pass through `Elem`.
//!
//! This relies on Wasmtime's `#[doc(hidden)]` lift/lower hooks
//! (`wasmtime::component::__internal`), so it is pinned to one Wasmtime version.

use super::plan::{self, Plan};
use super::stage::Stage;
use std::cell::Cell;
use std::mem::MaybeUninit;
use std::sync::Arc;
use wasmtime::ValRaw;
use wasmtime::component::__internal::wasmtime_environ::component::StringEncoding;
use wasmtime::component::__internal::{
    CanonicalAbiInfo, ComponentTypes, InstanceType, InterfaceType, LiftContext, LowerContext,
};
use wasmtime::component::{ComponentType, Lift, Lower};

#[derive(Clone, Copy, Debug)]
pub struct Elem<const S: u32, const A: u32> {
    stage: *const Stage,
    idx: u32,
}

// SAFETY: an `Elem` only points into a `Stage` owned by the same buffer that
// holds the `Elem`, and is only dereferenced on the store's thread.
unsafe impl<const S: u32, const A: u32> Send for Elem<S, A> {}
unsafe impl<const S: u32, const A: u32> Sync for Elem<S, A> {}

impl<const S: u32, const A: u32> Elem<S, A> {
    pub(super) fn new(stage: &Stage, idx: usize) -> Self {
        Elem { stage, idx: idx as u32 }
    }
}

/// Where lifted rows go: set by the pump around `Source::read`.
pub(super) struct LiftTarget<'a> {
    pub stage: &'a mut Stage,
    pub plan: &'a mut Option<(usize, InterfaceType, Arc<Plan>)>,
    pub error: Option<anyhow::Error>,
}

thread_local! {
    static LIFT: Cell<*mut LiftTarget<'static>> = const { Cell::new(std::ptr::null_mut()) };
}

/// Run `f` with lifted rows going into `target`.
pub(super) fn lifting_into<R>(target: &mut LiftTarget<'_>, f: impl FnOnce() -> R) -> R {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            LIFT.set(std::ptr::null_mut());
        }
    }
    LIFT.set(target as *mut LiftTarget<'_> as *mut LiftTarget<'static>);
    let _reset = Reset;
    f()
}

fn cached_plan(
    slot: &mut Option<(usize, InterfaceType, Arc<Plan>)>,
    types: &ComponentTypes,
    ty: InterfaceType,
) -> anyhow::Result<Arc<Plan>> {
    let key = types as *const ComponentTypes as usize;
    if let Some((k, t, p)) = slot
        && *k == key
        && *t == ty
    {
        return Ok(p.clone());
    }
    let p = Arc::new(plan::compile(types, &ty)?);
    *slot = Some((key, ty, p.clone()));
    Ok(p)
}

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
            wasmtime::bail!("layout mismatch: element is {}/{}, expected {S}/{A}", abi.size32, abi.align32);
        }
        plan::compile(types.types, ty).map_err(|e| wasmtime::format_err!("{e}"))?;
        Ok(())
    }
}

unsafe impl<const S: u32, const A: u32> Lift for Elem<S, A> {
    fn linear_lift_from_flat(
        _cx: &mut LiftContext<'_>,
        _ty: InterfaceType,
        _src: &Self::Lower,
    ) -> wasmtime::Result<Self> {
        wasmtime::bail!("stream rows are only lifted from memory")
    }

    fn linear_lift_from_memory(
        cx: &mut LiftContext<'_>,
        ty: InterfaceType,
        bytes: &[u8],
    ) -> wasmtime::Result<Self> {
        let target = LIFT.get();
        if target.is_null() {
            wasmtime::bail!("row lifted outside of a host pump");
        }
        // SAFETY: set by `lifting_into` for the duration of this read.
        let target = unsafe { &mut *target };
        let result = (|| {
            if !matches!(cx.options().string_encoding, StringEncoding::Utf8) {
                anyhow::bail!("only UTF-8 components are supported");
            }
            let plan = cached_plan(target.plan, cx.types, ty)?;
            let mem = cx.memory();
            // `bytes` is a window into `mem`; recover its offset so nested
            // pointers can be followed.
            let ptr = bytes.as_ptr() as usize - mem.as_ptr() as usize;
            target.stage.push_row(&plan, mem, ptr)
        })();
        match result {
            Ok(idx) => Ok(Elem { stage: target.stage, idx }),
            Err(e) => {
                let msg = e.to_string();
                target.error = Some(e);
                wasmtime::bail!("{msg}")
            }
        }
    }
}

unsafe impl<const S: u32, const A: u32> Lower for Elem<S, A> {
    fn linear_lower_to_flat<T>(
        &self,
        _cx: &mut LowerContext<'_, T>,
        _ty: InterfaceType,
        _dst: &mut MaybeUninit<Self::Lower>,
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

    /// Lower a whole run of rows at once: one `memcpy` for their fixed blocks,
    /// then their strings and lists.
    fn linear_store_list_to_memory<T>(
        cx: &mut LowerContext<'_, T>,
        ty: InterfaceType,
        offset: usize,
        items: &[Self],
    ) -> wasmtime::Result<()> {
        let Some(first) = items.first() else { return Ok(()) };
        if !matches!(cx.options().string_encoding, StringEncoding::Utf8) {
            wasmtime::bail!("only UTF-8 components are supported");
        }
        // SAFETY: the stage is owned by the buffer these items came from,
        // which Wasmtime keeps alive while lowering them.
        let stage = unsafe { &*first.stage };
        let start = first.idx as usize;
        debug_assert!(items.iter().enumerate().all(|(k, e)| e.idx as usize == start + k));
        let plan = {
            let mut slot = stage.lower_plan.lock().unwrap();
            cached_plan(&mut slot, cx.types, ty).map_err(|e| wasmtime::format_err!("{e}"))?
        };
        stage.lower_rows(cx, &plan, start, items.len(), offset)
    }
}
