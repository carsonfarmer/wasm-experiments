//! `take N`: a host built-in that sits on the pipe between two stages,
//! forwards the first N elements, then hangs up on the producer.
//!
//! It works for any element type the host can deep-copy (see `dynval`):
//! the shell picks the matching `Elem<SIZE, ALIGN>` at runtime.

use crate::dynval::Elem;
use anyhow::{Result, bail};
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use wasmtime::StoreContextMut;
use wasmtime::component::{
    Destination, Lift, Lower, Source, StreamAny, StreamConsumer, StreamProducer, StreamReader,
    StreamResult, VecBuffer,
};

/// Items buffered between the two sides before the producer is paused.
const CAPACITY: usize = 1024;

struct State<E> {
    queue: VecDeque<E>,
    /// Items still to accept from upstream.
    want: u64,
    /// Items still to deliver downstream.
    left: u64,
    upstream_done: bool,
    downstream_done: bool,
    /// Downstream reader waiting for items.
    reader: Option<Waker>,
    /// Upstream writer waiting for space.
    writer: Option<Waker>,
}

type Shared<E> = Arc<Mutex<State<E>>>;

/// Reads from the upstream stage.
struct TakeIn<E>(Shared<E>);

/// Feeds the downstream stage.
struct TakeOut<E>(Shared<E>);

impl<D: 'static, E: Lift + Send + Sync + 'static> StreamConsumer<D> for TakeIn<E> {
    type Item = E;

    fn poll_consume(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        store: StoreContextMut<D>,
        mut source: Source<'_, E>,
        finish: bool,
    ) -> Poll<wasmtime::Result<StreamResult>> {
        let mut st = self.0.lock().unwrap();
        if st.downstream_done || st.want == 0 {
            // Hanging up here is what cancels the producer.
            return Poll::Ready(Ok(StreamResult::Dropped));
        }
        let space = CAPACITY.saturating_sub(st.queue.len());
        if space == 0 {
            if finish {
                return Poll::Ready(Ok(StreamResult::Cancelled));
            }
            st.writer = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let mut buf = Vec::with_capacity(space.min(st.want as usize));
        source.read(store, &mut buf)?;
        st.want -= buf.len() as u64;
        st.queue.extend(buf);
        if let Some(w) = st.reader.take() {
            w.wake();
        }
        if st.want == 0 {
            Poll::Ready(Ok(StreamResult::Dropped))
        } else {
            Poll::Ready(Ok(StreamResult::Completed))
        }
    }
}

impl<E> Drop for TakeIn<E> {
    fn drop(&mut self) {
        let mut st = self.0.lock().unwrap();
        st.upstream_done = true;
        if let Some(w) = st.reader.take() {
            w.wake();
        }
    }
}

impl<D: 'static, E: Lower + Send + Sync + 'static> StreamProducer<D> for TakeOut<E> {
    type Item = E;
    type Buffer = VecBuffer<E>;

    fn poll_produce<'a>(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        store: StoreContextMut<'a, D>,
        mut dst: Destination<'a, E, VecBuffer<E>>,
        finish: bool,
    ) -> Poll<wasmtime::Result<StreamResult>> {
        let mut st = self.0.lock().unwrap();
        if st.left == 0 {
            return Poll::Ready(Ok(StreamResult::Dropped));
        }
        if st.queue.is_empty() {
            if st.upstream_done {
                return Poll::Ready(Ok(StreamResult::Dropped));
            }
            if finish {
                return Poll::Ready(Ok(StreamResult::Cancelled));
            }
            st.reader = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let room = dst.remaining(store).unwrap_or(usize::MAX);
        if room == 0 {
            // Zero-length read: the reader is only asking whether we're ready.
            return Poll::Ready(Ok(StreamResult::Completed));
        }
        let n = room.min(st.queue.len());
        let items: Vec<E> = st.queue.drain(..n).collect();
        st.left -= n as u64;
        dst.set_buffer(VecBuffer::from(items));
        if let Some(w) = st.writer.take() {
            w.wake();
        }
        if st.left == 0 {
            Poll::Ready(Ok(StreamResult::Dropped))
        } else {
            Poll::Ready(Ok(StreamResult::Completed))
        }
    }
}

impl<E> Drop for TakeOut<E> {
    fn drop(&mut self) {
        let mut st = self.0.lock().unwrap();
        st.downstream_done = true;
        if let Some(w) = st.writer.take() {
            w.wake();
        }
    }
}

fn take_typed<const S: u32, const A: u32, D: 'static>(
    mut store: StoreContextMut<'_, D>,
    input: StreamAny,
    n: u64,
) -> Result<StreamAny> {
    let reader = input.try_into_stream_reader::<Elem<S, A>>()?;
    let shared = Arc::new(Mutex::new(State {
        queue: VecDeque::new(),
        want: n,
        left: n,
        upstream_done: false,
        downstream_done: false,
        reader: None,
        writer: None,
    }));
    reader.pipe(&mut store, TakeIn(shared.clone()))?;
    let out = StreamReader::new(&mut store, TakeOut(shared))?;
    Ok(StreamAny::try_from_stream_reader(&mut store, out)?)
}

/// Wrap `input` in a `take n`, choosing the element shim by runtime layout.
pub fn take<D: 'static>(
    store: StoreContextMut<'_, D>,
    input: StreamAny,
    n: u64,
    (size, align): (u32, u32),
) -> Result<StreamAny> {
    // Each arm is a monomorphized copy of the forwarding code for one layout.
    macro_rules! dispatch {
        ($( ($s:literal, $a:literal) ),* $(,)?) => {
            match (size, align) {
                $( ($s, $a) => take_typed::<$s, $a, D>(store, input, n), )*
                _ => bail!(
                    "no element shim for a {size}-byte, {align}-aligned type \
                     (the spike's dispatch table covers common layouts only)"
                ),
            }
        };
    }
    dispatch!(
        (1, 1), (2, 1), (2, 2), (3, 1), (4, 1), (4, 2), (4, 4), (6, 2), (8, 2),
        (8, 4), (12, 4), (16, 4), (20, 4), (24, 4), (28, 4), (32, 4), (36, 4),
        (40, 4), (44, 4), (48, 4), (52, 4), (56, 4), (60, 4), (64, 4), (72, 4),
        (80, 4), (96, 4), (128, 4),
        (8, 8), (16, 8), (24, 8), (32, 8), (40, 8), (48, 8), (56, 8), (64, 8),
        (72, 8), (80, 8), (96, 8), (112, 8), (128, 8),
    )
}
