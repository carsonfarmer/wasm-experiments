//! A host-carried pipe. The input half reads batches from the producer into
//! [`Stage`]s; the output half hands them to the consumer. The halves only
//! share a queue, so they can live in different Stores on different threads.
//! A pipe can also stop after N rows (`take`), counting and cutting rows
//! inside columnar batches.

use super::elem::{Elem, LiftTarget, lifting_into};
use super::layout::{Kind, Layout};
use super::plan::{Plan, Proj, project};
use super::stage::Stage;
use anyhow::Result;
use std::collections::VecDeque;
use std::future::Future;
use std::mem::MaybeUninit;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use wasmtime::component::{Destination, Source, StreamConsumer, StreamProducer, StreamResult, WriteBuffer};
use wasmtime::{AsContextMut, StoreContextMut};

/// Stages buffered between the halves before the producer is paused.
const MAX_READY: usize = 4;

/// What one pipe carried, readable after the run.
#[derive(Default, Debug)]
pub struct Counters {
    /// Stream elements (rows, or batches for columnar streams).
    pub elems: AtomicU64,
    /// Logical rows.
    pub rows: AtomicU64,
    /// Producer writes received.
    pub batches: AtomicU64,
    /// Bytes staged.
    pub bytes: AtomicU64,
    /// `realloc` calls into the consumer.
    pub reallocs: AtomicU64,
    /// Time spent copying into stages, and out of them into the consumer.
    pub lift_ns: AtomicU64,
    pub lower_ns: AtomicU64,
}

/// A pipe from producer rows of layout `have` to consumer rows of layout
/// `want` (equal, or a projection), stopping after `limit` rows.
pub struct Spec {
    pub have: Arc<Layout>,
    pub want: Arc<Layout>,
    pub limit: Option<u64>,
}

/// Where the columns of a columnar batch are: a batch is a record whose read
/// fields are all `list<T>` of a flat `T`, or Arrow-style string columns
/// `record { data: string, offsets: list<u32> }`.
#[derive(Clone, Debug)]
struct BatchShape(Vec<Col>);

#[derive(Clone, Copy, Debug)]
enum Col {
    /// A `list<T>` at this offset.
    Prim(usize),
    /// A string column's `data` and `offsets` lists.
    Str { data: usize, offsets: usize },
}

fn rd32(b: &[u8], at: usize) -> usize {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) as usize
}

fn wr32(b: &mut [u8], at: usize, v: usize) {
    b[at..at + 4].copy_from_slice(&(v as u32).to_le_bytes());
}

impl BatchShape {
    fn of(have: &Layout, proj: &Proj) -> Option<BatchShape> {
        let Kind::Record(fields) = &have.kind else { return None };
        let read: Vec<usize> = match proj {
            Proj::Copy(_) => (0..fields.len()).collect(),
            Proj::Fields(picks) => picks.iter().map(|p| p.field).collect(),
        };
        let col = |i: usize| {
            let f = &fields[i];
            match &f.layout.kind {
                Kind::List(e) if e.is_flat() => Some(Col::Prim(f.off)),
                Kind::Record(sub) if sub.len() == 2 => {
                    let find = |name| sub.iter().find(|s| s.name == name);
                    let (d, o) = (find("data")?, find("offsets")?);
                    let data_ok = matches!(&d.layout.kind, Kind::Str)
                        || matches!(&d.layout.kind, Kind::List(e) if matches!(e.kind, Kind::Prim("u8")));
                    let offsets_ok = matches!(&o.layout.kind, Kind::List(e) if matches!(e.kind, Kind::Prim("u32")));
                    (data_ok && offsets_ok).then_some(Col::Str { data: f.off + d.off, offsets: f.off + o.off })
                }
                _ => None,
            }
        };
        let cols: Vec<Col> = read.into_iter().map(col).collect::<Option<_>>()?;
        (!cols.is_empty()).then_some(BatchShape(cols))
    }

    /// Rows in batch `i` of `stage`.
    fn rows(&self, stage: &Stage, i: usize) -> usize {
        match self.0[0] {
            Col::Prim(off) => rd32(stage.row(i), off + 4),
            Col::Str { offsets, .. } => rd32(stage.row(i), offsets + 4).saturating_sub(1),
        }
    }

    /// Cut batch `i` of `stage` down to its first `k` rows.
    fn truncate(&self, stage: &mut Stage, i: usize, k: usize) {
        let base = i * stage.size;
        for &col in &self.0 {
            match col {
                Col::Prim(off) => wr32(&mut stage.fixed, base + off + 4, k),
                Col::Str { data, offsets } => {
                    let end = rd32(&stage.heap, rd32(&stage.fixed, base + offsets) + 4 * k);
                    wr32(&mut stage.fixed, base + offsets + 4, k + 1);
                    wr32(&mut stage.fixed, base + data + 4, end);
                }
            }
        }
    }
}

// Stages are boxed so they never move: delivered `Elem`s point into them.
#[allow(clippy::vec_box)]
struct State {
    ready: VecDeque<Box<Stage>>,
    free: Vec<Box<Stage>>,
    /// Rows still to accept from the producer.
    want: u64,
    shape: Option<BatchShape>,
    lift: Plan,
    proj: Arc<Proj>,
    dst_size: usize,
    upstream_done: bool,
    downstream_done: bool,
    reader: Option<Waker>,
    writer: Option<Waker>,
    done: Option<Waker>,
    counters: Arc<Counters>,
}

fn wake(w: &mut Option<Waker>) {
    if let Some(w) = w.take() {
        w.wake();
    }
}

type Shared = Arc<Mutex<State>>;

/// The producer-facing half, for producer elements of `.1` = (size, align).
pub struct PumpIn(Shared, pub (u32, u32));
/// The consumer-facing half, for consumer elements of `.1` = (size, align).
pub struct PumpOut(Shared, pub (u32, u32));
/// Resolves once the producer side has nothing left to do.
pub struct Finished(Shared);

pub fn new(spec: Spec, counters: Arc<Counters>) -> Result<(PumpIn, PumpOut, Finished)> {
    let proj = project(&spec.want, &spec.have)?;
    let st = Arc::new(Mutex::new(State {
        ready: VecDeque::new(),
        free: Vec::new(),
        want: spec.limit.unwrap_or(u64::MAX),
        shape: BatchShape::of(&spec.have, &proj),
        lift: Plan::lift(&spec.have, &proj),
        proj: Arc::new(proj),
        dst_size: spec.want.size,
        upstream_done: false,
        downstream_done: false,
        reader: None,
        writer: None,
        done: None,
        counters,
    }));
    let abi = |l: &Layout| (l.size as u32, l.align as u32);
    Ok((PumpIn(st.clone(), abi(&spec.have)), PumpOut(st.clone(), abi(&spec.want)), Finished(st)))
}

impl Future for Finished {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut st = self.0.lock().unwrap();
        if st.upstream_done || st.downstream_done {
            return Poll::Ready(());
        }
        st.done = Some(cx.waker().clone());
        Poll::Pending
    }
}

/// A read buffer that only counts: the rows themselves go into the stage.
struct Counting(usize);

// (Implemented by path: importing `ReadBuffer` would shadow `Vec::extend`.)
impl<T: Send + Sync + 'static> wasmtime::component::ReadBuffer<T> for Counting {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        self.0 -= iter.into_iter().count();
    }
    fn remaining_capacity(&self) -> usize {
        self.0
    }
    fn move_from(&mut self, input: &mut dyn WriteBuffer<T>, count: usize) {
        input.skip(count);
        self.0 -= count;
    }
}

/// `PumpIn` / `PumpOut` for one element layout.
pub struct TypedIn<const S: u32, const A: u32>(pub PumpIn);
pub struct TypedOut<const S: u32, const A: u32>(pub PumpOut);

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
            return Poll::Ready(Ok(StreamResult::Dropped)); // hanging up cancels the producer
        }
        if st.ready.len() >= MAX_READY {
            if finish {
                return Poll::Ready(Ok(StreamResult::Cancelled));
            }
            st.writer = Some(cx.waker().clone());
            return Poll::Pending;
        }
        // Row streams stop reading at the limit; columnar batches are cut below.
        let offered = source.remaining(store.as_context_mut());
        let cap = if st.shape.is_some() { offered } else { offered.min(st.want as usize) };

        let mut stage = st.free.pop().unwrap_or_default();
        stage.fixed.clear();
        stage.heap.clear();
        stage.rows = 0;
        stage.size = st.lift.size;
        stage.proj = Some(st.proj.clone());
        stage.dst_size = st.dst_size;
        stage.counters = st.counters.clone();
        let t0 = std::time::Instant::now();
        let mut target = LiftTarget { stage: &mut stage, plan: &st.lift, error: None };
        let read = lifting_into(&mut target, || source.read(store, &mut Counting(cap)));
        if let Some(e) = target.error.take() {
            return Poll::Ready(Err(wasmtime::format_err!("{e:#}")));
        }
        read?;
        st.counters.lift_ns.fetch_add(t0.elapsed().as_nanos() as u64, Relaxed);

        let mut rows = stage.rows as u64;
        if let Some(shape) = &st.shape {
            rows = 0;
            for i in 0..stage.rows {
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
        if st.want != u64::MAX {
            st.want -= rows;
        }
        let c = &st.counters;
        c.elems.fetch_add(stage.rows as u64, Relaxed);
        c.rows.fetch_add(rows, Relaxed);
        c.batches.fetch_add(1, Relaxed);
        c.bytes.fetch_add((stage.fixed.len() + stage.heap.len()) as u64, Relaxed);
        st.ready.push_back(stage);
        wake(&mut st.reader);
        if st.want > 0 {
            return Poll::Ready(Ok(StreamResult::Completed));
        }
        st.upstream_done = true; // the consumer finishes once the queue drains
        wake(&mut st.done);
        Poll::Ready(Ok(StreamResult::Dropped))
    }
}

/// The staged rows being delivered to the consumer.
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
        // SAFETY: `Elem` is `Copy`, so the callee may copy initialized items out.
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
        st.free.extend(out.stage.take());
        let Some(stage) = st.ready.front() else {
            if st.upstream_done {
                return Poll::Ready(Ok(StreamResult::Dropped));
            }
            if finish {
                return Poll::Ready(Ok(StreamResult::Cancelled));
            }
            st.reader = Some(cx.waker().clone());
            return Poll::Pending;
        };
        // A zero-length read only asks whether we're ready.
        if dst.remaining(store.as_context_mut()) != Some(0) {
            out.elems.clear();
            out.elems.extend((0..stage.rows).map(|i| Elem::new(stage, i)));
            out.pos = 0;
            out.stage = st.ready.pop_front();
            dst.set_buffer(out);
            wake(&mut st.writer);
        }
        Poll::Ready(Ok(StreamResult::Completed))
    }
}

impl Drop for PumpIn {
    fn drop(&mut self) {
        let mut st = self.0.lock().unwrap();
        st.upstream_done = true;
        wake(&mut st.done);
        wake(&mut st.reader);
    }
}

impl Drop for PumpOut {
    fn drop(&mut self) {
        let mut st = self.0.lock().unwrap();
        st.downstream_done = true;
        wake(&mut st.done);
        wake(&mut st.writer);
    }
}
