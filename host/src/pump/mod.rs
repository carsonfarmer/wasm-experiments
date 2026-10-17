//! The host pump: carries rows between components itself instead of
//! leaving it to Wasmtime's built-in guest-to-guest copy.
//!
//! Wasmtime copies a stream element that holds pointers by lifting it into a
//! dynamic `Val` tree and lowering that again, allocating for every field
//! name, string and list element of every row. The pump compiles the row
//! type into a copy [`plan`] once, stages rows as raw canonical-ABI bytes
//! ([`stage`]), and lowers them with `memcpy` plus one `realloc` per string
//! or list. Sitting between the components, it can also hand the consumer a
//! narrower row type than the producer wrote, stop after N rows, and bridge
//! components in different Stores (and threads).

mod elem;
pub mod layout;
pub mod pipe;
pub mod plan;
mod stage;

pub use layout::Layout;
pub use pipe::{Counters, Finished, PumpIn, PumpOut, Spec};

use anyhow::{Result, bail};
use pipe::{TypedIn, TypedOut};
use seq_macro::seq;
use wasmtime::component::{StreamAny, StreamReader};
use wasmtime::{AsContextMut, StoreContextMut};

/// Call `$go!(SIZE, ALIGN)` for the element layout `$abi`. Wasmtime needs
/// element sizes at compile time, so each layout is its own instantiation.
macro_rules! by_layout {
    ($abi:expr, $go:ident) => {{
        let abi: (u32, u32) = $abi;
        seq!(N in 1..=8 { if abi == (N, 1) { $go!(N, 1) } });
        seq!(N in 1..=8 { if abi == (N * 2, 2) { $go!({ N * 2 }, 2) } });
        seq!(N in 1..=64 { if abi == (N * 4, 4) { $go!({ N * 4 }, 4) } });
        seq!(N in 1..=32 { if abi == (N * 8, 8) { $go!({ N * 8 }, 8) } });
        bail!("rows of {} bytes (align {}) are too big for the host pump", abi.0, abi.1)
    }};
}

/// Feed the producer stream `input` into a pipe's input half.
pub fn attach_input<D: 'static>(mut store: StoreContextMut<'_, D>, input: StreamAny, pin: PumpIn) -> Result<()> {
    macro_rules! go {
        ($s:tt, $a:tt) => {
            return Ok(input.try_into_stream_reader::<elem::Elem<$s, $a>>()?.pipe(&mut store, TypedIn::<$s, $a>(pin))?)
        };
    }
    by_layout!(pin.1, go)
}

/// The stream a consumer reads from a pipe's output half.
pub fn open_output<D: 'static>(mut store: StoreContextMut<'_, D>, pout: PumpOut) -> Result<StreamAny> {
    macro_rules! go {
        ($s:tt, $a:tt) => {{
            let out = StreamReader::new(&mut store, TypedOut::<$s, $a>(pout))?;
            return Ok(StreamAny::try_from_stream_reader(&mut store, out)?);
        }};
    }
    by_layout!(pout.1, go)
}

/// A pipe between a producer and a consumer in the same Store.
pub fn pump<D: 'static>(
    mut store: StoreContextMut<'_, D>,
    input: StreamAny,
    pin: PumpIn,
    pout: PumpOut,
) -> Result<StreamAny> {
    attach_input(store.as_context_mut(), input, pin)?;
    open_output(store, pout)
}
