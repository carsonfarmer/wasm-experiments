//! A host-carried pipe: reads batches from the upstream component into
//! [`Stage`]s and hands them to the downstream component, optionally
//! stopping after N rows (`take`).

use super::elem::{Elem, LiftTarget, lifting_into};
use super::plan::Plan;
use super::stage::Stage;
use std::collections::VecDeque;
use std::mem::MaybeUninit;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use wasmtime::StoreContextMut;
use wasmtime::component::__internal::InterfaceType;
use wasmtime::component::{
    Destination, ReadBuffer, Source, StreamConsumer, StreamProducer, StreamResult, WriteBuffer,
};
use wasmtime::AsContextMut;

/// Stages buffered between the two sides before the producer is paused.
const MAX_READY: usize = 4;

/// Counters for one pipe, readable after the run.
#[derive(Default, Debug)]
pub struct Counters {
    /// Stream elements carried (rows, or batches for columnar streams).
    pub elems: AtomicU64,
    /// Logical rows carried (equal to `elems` for row streams).
    pub rows: AtomicU64,
    /// Producer writes received.
    pub batches: AtomicU64,
    /// Bytes staged (fixed blocks plus heap data).
    pub bytes: AtomicU64,
    /// `realloc` calls made into the consumer.
    pub reallocs: AtomicU64,
    /// Time spent copying rows out of the producer into stages.
    pub lift_ns: AtomicU64,
    /// Time spent copying rows from stages into the consumer (incl. `realloc`).
    pub lower_ns: AtomicU64,
}

/// How to find and cut the rows inside a columnar batch element.
#[derive(Clone, Debug)]
pub struct BatchShape {
    pub cols: Vec<Col>,
}

#[derive(Clone, Debug)]
pub enum Col {
    /// `list<T>` of a fixed-size `T`, at byte offset `off` of the batch.
    Prim { off: usize },
    /// `record { data: string, offsets: list<u32> }` with `offsets` at
    /// `offsets_off` and `data` at `data_off`; `offsets` has rows + 1 entries.
    Str { data_off: usize, offsets_off: usize },
}

fn rd32(b: &[u8], at: usize) -> usize {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) as usize
}

fn wr32(b: &mut [u8], at: usize, v: usize) {
    b[at..at + 4].copy_from_slice(&(v as u32).to_le_bytes());
}

impl BatchShape {
    /// Rows in batch `i` of `stage`.
    fn rows(&self, stage: &Stage, i: usize) -> usize {
        let row = stage.row(i);
        match self.cols.first() {
            Some(Col::Prim { off }) => rd32(row, off + 4),
            Some(Col::Str { offsets_off, .. }) => rd32(row, offsets_off + 4).saturating_sub(1),
            None => 0,
        }
    }

    /// Cut batch `i` of `stage` down to its first `k` rows.
    fn truncate(&self, stage: &mut Stage, i: usize, k: usize) {
        let base = i * stage.size;
        for col in &self.cols {
            match *col {
                Col::Prim { off } => wr32(&mut stage.fixed, base + off + 4, k),
                Col::Str { data_off, offsets_off } => {
                    let h = rd32(&stage.fixed, base + offsets_off);
                    let end = rd32(&stage.heap, h + 4 * k);
                    wr32(&mut stage.fixed, base + offsets_off + 4, k + 1);
                    wr32(&mut stage.fixed, base + data_off + 4, end);
                }
            }
        }
    }
}

pub enum Limit {
    /// Forward everything.
    None,
    /// Stop after this many stream elements.
    Elems(u64),
    /// Stop after this many rows, cutting the last columnar batch short.
    Rows(u64, BatchShape),
}

struct State {
    ready: VecDeque<Box<Stage>>,
    free: Vec<Box<Stage>>,
    /// Rows (or elements) still to accept from upstream.
    want: u64,
    batch: Option<BatchShape>,
    upstream_done: bool,
    downstream_done: bool,
    reader: Option<Waker>,
    writer: Option<Waker>,
    lift_plan: Option<(usize, InterfaceType, Arc<Plan>)>,
    counters: Arc<Counters>,
}

type Shared = Arc<Mutex<State>>;

pub struct PumpIn(Shared);
pub struct PumpOut(Shared);

pub fn new(limit: Limit, counters: Arc<Counters>) -> (PumpIn, PumpOut) {
    let (want, batch) = match limit {
        Limit::None => (u64::MAX, None),
        Limit::Elems(n) => (n, None),
        Limit::Rows(n, shape) => (n, Some(shape)),
    };
    let st = Arc::new(Mutex::new(State {
        ready: VecDeque::new(),
        free: Vec::new(),
        want,
        batch,
        upstream_done: false,
        downstream_done: false,
        reader: None,
        writer: None,
        lift_plan: None,
        counters,
    }));
    (PumpIn(st.clone()), PumpOut(st))
}

/// A read buffer that only counts: the rows themselves go into the stage.
struct Counting {
    cap: usize,
    got: usize,
}

impl<T: Send + Sync + 'static> ReadBuffer<T> for Counting {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        self.got += iter.into_iter().count();
    }
    fn remaining_capacity(&self) -> usize {
        self.cap - self.got
    }
    fn move_from(&mut self, input: &mut dyn WriteBuffer<T>, count: usize) {
        input.skip(count);
        self.got += count;
    }
}

impl<D: 'static, const S: u32, const A: u32> StreamConsumer<D> for TypedIn<S, A> {
    type Item = Elem<S, A>;

    fn poll_consume(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut store: StoreContextMut<D>,
        mut source: Source<'_, Elem<S, A>>,
        finish: bool,
    ) -> Poll<wasmtime::Result<StreamResult>> {
        let mut guard = self.0.0.lock().unwrap();
        let st = &mut *guard;
        if st.downstream_done || st.want == 0 {
            // Hanging up here is what cancels the producer.
            return Poll::Ready(Ok(StreamResult::Dropped));
        }
        if st.ready.len() >= MAX_READY {
            if finish {
                return Poll::Ready(Ok(StreamResult::Cancelled));
            }
            st.writer = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let offered = source.remaining(store.as_context_mut());
        // Row streams stop reading at the limit; columnar ones are cut below.
        let cap = if st.batch.is_some() { offered } else { offered.min(st.want as usize) };
        let mut stage = st.free.pop().unwrap_or_default();
        stage.clear();
        stage.counters = Some(st.counters.clone());
        let mut target = LiftTarget { stage: &mut stage, plan: &mut st.lift_plan, error: None };
        let mut buf = Counting { cap, got: 0 };
        let t0 = std::time::Instant::now();
        let read = lifting_into(&mut target, || source.read(store, &mut buf));
        st.counters.lift_ns.fetch_add(t0.elapsed().as_nanos() as u64, Relaxed);
        if let Some(e) = target.error.take() {
            return Poll::Ready(Err(wasmtime::format_err!("{e:#}")));
        }
        read?;

        let elems = stage.rows;
        let mut rows = elems as u64;
        if let Some(shape) = &st.batch {
            rows = 0;
            for i in 0..elems {
                let r = shape.rows(&stage, i) as u64;
                if rows + r >= st.want {
                    shape.truncate(&mut stage, i, (st.want - rows) as usize);
                    stage.rows = i + 1;
                    rows = st.want;
                    break;
                }
                rows += r;
            }
        }
        st.want -= rows;
        let c = &st.counters;
        c.elems.fetch_add(stage.rows as u64, Relaxed);
        c.rows.fetch_add(rows, Relaxed);
        c.batches.fetch_add(1, Relaxed);
        c.bytes.fetch_add((stage.fixed.len() + stage.heap.len()) as u64, Relaxed);
        st.ready.push_back(stage);
        if st.want == 0 {
            // Nothing more will be read; let the consumer finish once drained.
            st.upstream_done = true;
        }
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

/// The staged rows currently being delivered to the consumer.
pub struct Delivery<const S: u32, const A: u32> {
    stage: Option<Box<Stage>>,
    elems: Vec<Elem<S, A>>,
    pos: usize,
}

impl<const S: u32, const A: u32> Default for Delivery<S, A> {
    fn default() -> Self {
        Delivery { stage: None, elems: Vec::new(), pos: 0 }
    }
}

unsafe impl<const S: u32, const A: u32> WriteBuffer<Elem<S, A>> for Delivery<S, A> {
    fn remaining(&self) -> &[Elem<S, A>] {
        &self.elems[self.pos..]
    }
    fn skip(&mut self, count: usize) {
        self.pos += count;
    }
    fn take(&mut self, count: usize, fun: &mut dyn FnMut(&[MaybeUninit<Elem<S, A>>])) {
        let items = &self.elems[self.pos..self.pos + count];
        // SAFETY: `Elem` is `Copy`, so viewing initialized items as
        // `MaybeUninit` and letting the callee copy them out is sound.
        fun(unsafe { &*(items as *const [Elem<S, A>] as *const [MaybeUninit<Elem<S, A>>]) });
        self.pos += count;
    }
}

impl<D: 'static, const S: u32, const A: u32> StreamProducer<D> for TypedOut<S, A> {
    type Item = Elem<S, A>;
    type Buffer = Delivery<S, A>;

    fn poll_produce<'a>(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut store: StoreContextMut<'a, D>,
        mut dst: Destination<'a, Elem<S, A>, Delivery<S, A>>,
        finish: bool,
    ) -> Poll<wasmtime::Result<StreamResult>> {
        let mut guard = self.0.0.lock().unwrap();
        let st = &mut *guard;
        // The previous delivery has been fully lowered: recycle its stage.
        let mut out = dst.take_buffer();
        if let Some(stage) = out.stage.take() {
            st.free.push(stage);
        }
        if st.ready.is_empty() {
            if st.upstream_done {
                return Poll::Ready(Ok(StreamResult::Dropped));
            }
            if finish {
                return Poll::Ready(Ok(StreamResult::Cancelled));
            }
            st.reader = Some(cx.waker().clone());
            dst.set_buffer(out);
            return Poll::Pending;
        }
        if dst.remaining(store.as_context_mut()) == Some(0) {
            // Zero-length read: the reader is only asking whether we're ready.
            dst.set_buffer(out);
            return Poll::Ready(Ok(StreamResult::Completed));
        }
        let stage = st.ready.pop_front().unwrap();
        out.elems.clear();
        Extend::extend(&mut out.elems, (0..stage.rows).map(|i| Elem::new(&*stage, i)));
        out.pos = 0;
        out.stage = Some(stage);
        dst.set_buffer(out);
        if let Some(w) = st.writer.take() {
            w.wake();
        }
        Poll::Ready(Ok(StreamResult::Completed))
    }
}

/// `PumpIn`/`PumpOut` monomorphized for one element layout.
pub struct TypedIn<const S: u32, const A: u32>(pub PumpIn);
pub struct TypedOut<const S: u32, const A: u32>(pub PumpOut);

impl Drop for PumpIn {
    fn drop(&mut self) {
        let mut st = self.0.lock().unwrap();
        st.upstream_done = true;
        if let Some(w) = st.reader.take() {
            w.wake();
        }
    }
}

impl Drop for PumpOut {
    fn drop(&mut self) {
        let mut st = self.0.lock().unwrap();
        st.downstream_done = true;
        if let Some(w) = st.writer.take() {
            w.wake();
        }
    }
}
