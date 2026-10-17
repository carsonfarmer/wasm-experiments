//! The host pump: carries rows between components itself instead of
//! leaving it to Wasmtime's built-in guest-to-guest copy.
//!
//! Why: Wasmtime copies a stream element that contains pointers by lifting it
//! into a dynamic `Val` tree and lowering that tree again. That allocates for
//! every field name, string and list element of every row (a `list<u8>`
//! becomes one heap-allocated `Val` per byte). The pump instead compiles the
//! row type into a copy [`plan`] once, stages rows as raw canonical-ABI bytes
//! (see [`stage::Stage`]), and lowers them with `memcpy` plus one `realloc` per
//! string or list. Because it sits between the two components, it can also
//! hand the consumer a narrower row type than the producer wrote (a
//! [`plan::Proj`]), stop after N rows, and bridge components living in
//! different Stores (and threads).

mod elem;
pub mod pipe;
pub mod plan;
mod stage;

use anyhow::{Result, bail};
use pipe::{Counters, PumpIn, PumpOut, TypedIn, TypedOut};
use std::sync::Arc;
use wasmtime::StoreContextMut;
use wasmtime::component::{StreamAny, StreamReader};

pub use pipe::{BatchShape, Col, Finished, Spec};
pub use plan::Need;

// Wasmtime needs each element's size at compile time, so each layout gets its
// own monomorphized copy of the (small) pump glue.
macro_rules! layouts {
    ($mac:ident) => {
        $mac!(
            (1, 1), (2, 1), (3, 1), (4, 1), (5, 1), (6, 1), (7, 1), (8, 1), (2, 2), (4, 2),
            (6, 2), (8, 2), (10, 2), (12, 2), (14, 2), (16, 2), (4, 4), (8, 4), (12, 4),
            (16, 4), (20, 4), (24, 4), (28, 4), (32, 4), (36, 4), (40, 4), (44, 4), (48, 4),
            (52, 4), (56, 4), (60, 4), (64, 4), (68, 4), (72, 4), (76, 4), (80, 4), (84, 4),
            (88, 4), (92, 4), (96, 4), (100, 4), (104, 4), (108, 4), (112, 4), (116, 4),
            (120, 4), (124, 4), (128, 4), (132, 4), (136, 4), (140, 4), (144, 4), (148, 4),
            (152, 4), (156, 4), (160, 4), (164, 4), (168, 4), (172, 4), (176, 4), (180, 4),
            (184, 4), (188, 4), (192, 4), (196, 4), (200, 4), (204, 4), (208, 4), (212, 4),
            (216, 4), (220, 4), (224, 4), (228, 4), (232, 4), (236, 4), (240, 4), (244, 4),
            (248, 4), (252, 4), (256, 4), (8, 8), (16, 8), (24, 8), (32, 8), (40, 8), (48, 8),
            (56, 8), (64, 8), (72, 8), (80, 8), (88, 8), (96, 8), (104, 8), (112, 8), (120, 8),
            (128, 8), (136, 8), (144, 8), (152, 8), (160, 8), (168, 8), (176, 8), (184, 8),
            (192, 8), (200, 8), (208, 8), (216, 8), (224, 8), (232, 8), (240, 8), (248, 8),
            (256, 8),
        )
    };
}

fn attach<const S: u32, const A: u32, D: 'static>(
    mut store: StoreContextMut<'_, D>,
    input: StreamAny,
    pin: PumpIn,
) -> Result<()> {
    let reader = input.try_into_stream_reader::<elem::Elem<S, A>>()?;
    reader.pipe(&mut store, TypedIn::<S, A>(pin))?;
    Ok(())
}

fn open<const S: u32, const A: u32, D: 'static>(mut store: StoreContextMut<'_, D>, pout: PumpOut) -> Result<StreamAny> {
    let out = StreamReader::new(&mut store, TypedOut::<S, A>(pout))?;
    Ok(StreamAny::try_from_stream_reader(&mut store, out)?)
}

/// Feed the producer stream `input` (elements of canonical-ABI `(size, align)`)
/// into a pipe's input half.
pub fn attach_input<D: 'static>(
    store: StoreContextMut<'_, D>,
    input: StreamAny,
    (size, align): (u32, u32),
    pin: PumpIn,
) -> Result<()> {
    macro_rules! dispatch {
        ($( ($s:literal, $a:literal) ),* $(,)?) => {
            match (size, align) {
                $( ($s, $a) => attach::<$s, $a, D>(store, input, pin), )*
                _ => bail!("rows of {size} bytes (align {align}) are too big for the host pump"),
            }
        };
    }
    layouts!(dispatch)
}

/// Create the stream a consumer reads from a pipe's output half, for consumer
/// elements of canonical-ABI `(size, align)`.
pub fn open_output<D: 'static>(store: StoreContextMut<'_, D>, (size, align): (u32, u32), pout: PumpOut) -> Result<StreamAny> {
    macro_rules! dispatch {
        ($( ($s:literal, $a:literal) ),* $(,)?) => {
            match (size, align) {
                $( ($s, $a) => open::<$s, $a, D>(store, pout), )*
                _ => bail!("rows of {size} bytes (align {align}) are too big for the host pump"),
            }
        };
    }
    layouts!(dispatch)
}

/// Interpose a pump between `input` and a consumer in the same Store.
pub fn pump<D: 'static>(
    mut store: StoreContextMut<'_, D>,
    input: StreamAny,
    spec: Spec,
    in_layout: (u32, u32),
    out_layout: (u32, u32),
    counters: Arc<Counters>,
) -> Result<StreamAny> {
    let (pin, pout, _finished) = pipe::new(spec, counters);
    attach_input(store.as_context_mut(), input, in_layout, pin)?;
    open_output(store, out_layout, pout)
}

use wasmtime::AsContextMut;
