//! A stage is one batch of rows held by the host between two components.
//!
//! Rows are kept in the producer's canonical-ABI layout: `fixed` holds each
//! row's fixed-size block back to back, and `heap` holds all their strings and
//! list contents, with every `(ptr, len)` rewritten to point into `heap`.
//! Lifting a batch from a producer is a few `memcpy`s; lowering it into a
//! consumer is a `memcpy` of `fixed` plus one `realloc` + `memcpy` per string
//! or list the consumer asked for. Stages are recycled, so steady-state
//! forwarding performs no host allocations.

use super::pipe::Counters;
use super::plan::{Fix, Layout, Plan, Proj};
use anyhow::{Result, anyhow, bail};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex};
use wasmtime::component::__internal::{InterfaceType, LowerContext};

/// Cached projection for the consumer: (types address, type, projection, row size).
pub(super) type LowerCache = Option<(usize, InterfaceType, Arc<Proj>, usize)>;

#[derive(Default)]
pub struct Stage {
    pub fixed: Vec<u8>,
    pub heap: Vec<u8>,
    pub rows: usize,
    pub size: usize,
    /// The producer's row layout (what `fixed` holds).
    pub src: Option<Arc<Layout>>,
    /// How to lower into the consumer, compiled from its own types.
    pub(super) lower: Mutex<LowerCache>,
    /// The owning pipe's counters.
    pub counters: Option<Arc<Counters>>,
}

impl Stage {
    pub fn clear(&mut self) {
        self.fixed.clear();
        self.heap.clear();
        self.rows = 0;
    }

    pub fn row(&self, i: usize) -> &[u8] {
        &self.fixed[i * self.size..(i + 1) * self.size]
    }

    /// Copy the row at `ptr` in `src` (the producer's memory) into this stage.
    pub fn push_row(&mut self, plan: &Plan, src: &[u8], ptr: usize) -> Result<u32> {
        self.size = plan.size;
        let at = self.fixed.len();
        let row = src.get(ptr..ptr + plan.size).ok_or_else(|| anyhow!("row out of bounds"))?;
        self.fixed.extend_from_slice(row);
        if !plan.is_flat() {
            lift(&plan.fixes, src, &mut self.fixed, &mut self.heap, false, at)?;
        }
        self.rows += 1;
        Ok((self.rows - 1) as u32)
    }

    /// Write rows `first..first + n` into the consumer's memory at `dst`,
    /// where each consumer row is `dst_size` bytes.
    pub fn lower_rows<T>(
        &self,
        cx: &mut LowerContext<'_, T>,
        proj: &Proj,
        dst_size: usize,
        first: usize,
        n: usize,
        dst: usize,
    ) -> wasmtime::Result<()> {
        let t0 = std::time::Instant::now();
        let mut reallocs = 0u64;
        let result = (|| {
            match proj {
                // Same row type: one memcpy for every row's fixed block.
                Proj::Copy(plan) => {
                    let bytes = &self.fixed[first * self.size..(first + n) * self.size];
                    cx.as_slice_mut()
                        .get_mut(dst..dst + bytes.len())
                        .ok_or_else(|| wasmtime::format_err!("read buffer out of bounds"))?
                        .copy_from_slice(bytes);
                    if !plan.is_flat() {
                        for k in 0..n {
                            lower(cx, &plan.fixes, &self.heap, self.row(first + k), dst + k * dst_size, &mut reallocs)?;
                        }
                    }
                }
                // Projected: build each consumer row field by field.
                Proj::Fields(_) => {
                    for k in 0..n {
                        lower_proj(cx, proj, &self.heap, self.row(first + k), dst + k * dst_size, &mut reallocs)?;
                    }
                }
            }
            Ok(())
        })();
        if let Some(c) = &self.counters {
            c.lower_ns.fetch_add(t0.elapsed().as_nanos() as u64, Relaxed);
            c.reallocs.fetch_add(reallocs, Relaxed);
        }
        result
    }
}

#[inline]
fn rd32(buf: &[u8], at: usize) -> usize {
    u32::from_le_bytes(buf[at..at + 4].try_into().unwrap()) as usize
}

#[inline]
fn wr32(buf: &mut [u8], at: usize, v: usize) {
    buf[at..at + 4].copy_from_slice(&(v as u32).to_le_bytes());
}

fn rd_disc(buf: &[u8], at: usize, size: usize) -> usize {
    let mut d = [0u8; 4];
    d[..size].copy_from_slice(&buf[at..at + size]);
    u32::from_le_bytes(d) as usize
}

/// Follow the pointers of one value whose fixed block is at `base` in either
/// `fixed` or `heap` (still holding the producer's pointers), copying their
/// targets from `src` into `heap` and rewriting the pointers to heap offsets.
fn lift(
    fixes: &[Fix],
    src: &[u8],
    fixed: &mut Vec<u8>,
    heap: &mut Vec<u8>,
    in_heap: bool,
    base: usize,
) -> Result<()> {
    for fix in fixes {
        match fix {
            Fix::Str { off } => {
                let at = base + off;
                let buf: &[u8] = if in_heap { &heap[..] } else { &fixed[..] };
                let (p, len) = (rd32(buf, at), rd32(buf, at + 4));
                let bytes = src
                    .get(p..p.checked_add(len).ok_or_else(|| anyhow!("string length overflow"))?)
                    .ok_or_else(|| anyhow!("string out of bounds"))?;
                std::str::from_utf8(bytes).map_err(|_| anyhow!("invalid UTF-8 in string"))?;
                let h = heap.len();
                heap.extend_from_slice(bytes);
                let buf: &mut [u8] = if in_heap { &mut heap[..] } else { &mut fixed[..] };
                wr32(buf, at, h);
            }
            Fix::List { off, elem } => {
                let at = base + off;
                let buf: &[u8] = if in_heap { &heap[..] } else { &fixed[..] };
                let (p, n) = (rd32(buf, at), rd32(buf, at + 4));
                let bytes = n.checked_mul(elem.size).ok_or_else(|| anyhow!("list length overflow"))?;
                if p % elem.align as usize != 0 {
                    bail!("list pointer not aligned");
                }
                let data = src
                    .get(p..p.checked_add(bytes).ok_or_else(|| anyhow!("list length overflow"))?)
                    .ok_or_else(|| anyhow!("list out of bounds"))?;
                let h = heap.len();
                heap.extend_from_slice(data);
                let buf: &mut [u8] = if in_heap { &mut heap[..] } else { &mut fixed[..] };
                wr32(buf, at, h);
                if !elem.is_flat() {
                    for k in 0..n {
                        lift(&elem.fixes, src, fixed, heap, true, h + k * elem.size)?;
                    }
                }
            }
            Fix::Case { off, disc_size, payload_off, cases } => {
                let buf: &[u8] = if in_heap { &heap[..] } else { &fixed[..] };
                let d = rd_disc(buf, base + off, *disc_size);
                match cases.get(d) {
                    Some(Some(p)) => lift(&p.fixes, src, fixed, heap, in_heap, base + payload_off)?,
                    Some(None) => {}
                    None => bail!("invalid discriminant {d}"),
                }
            }
        }
    }
    Ok(())
}

/// Build one projected value at `at` in the consumer from the staged value `src`.
fn lower_proj<T>(
    cx: &mut LowerContext<'_, T>,
    proj: &Proj,
    heap: &[u8],
    src: &[u8],
    at: usize,
    reallocs: &mut u64,
) -> wasmtime::Result<()> {
    match proj {
        Proj::Copy(plan) => {
            cx.as_slice_mut()
                .get_mut(at..at + plan.size)
                .ok_or_else(|| wasmtime::format_err!("read buffer out of bounds"))?
                .copy_from_slice(&src[..plan.size]);
            lower(cx, &plan.fixes, heap, src, at, reallocs)
        }
        Proj::Fields(fields) => {
            for (d, s, p) in fields {
                lower_proj(cx, p, heap, &src[*s..], at + d, reallocs)?;
            }
            Ok(())
        }
    }
}

/// Follow the pointers of one value already copied into the consumer's memory
/// at `at`. `src` is the same value in the stage (holding heap offsets), so
/// pointers are read from host memory; each string or list is allocated in
/// the consumer and its bytes copied over from `heap`.
fn lower<T>(
    cx: &mut LowerContext<'_, T>,
    fixes: &[Fix],
    heap: &[u8],
    src: &[u8],
    at: usize,
    reallocs: &mut u64,
) -> wasmtime::Result<()> {
    for fix in fixes {
        match fix {
            Fix::Str { off } => {
                let (h, len) = (rd32(src, *off), rd32(src, off + 4));
                let p = cx.realloc(0, 0, 1, len)?;
                *reallocs += 1;
                let mem = cx.as_slice_mut();
                mem[p..p + len].copy_from_slice(&heap[h..h + len]);
                wr32(mem, at + off, p);
            }
            Fix::List { off, elem } => {
                let (h, n) = (rd32(src, *off), rd32(src, off + 4));
                let bytes = n * elem.size;
                let p = cx.realloc(0, 0, elem.align, bytes)?;
                *reallocs += 1;
                let mem = cx.as_slice_mut();
                mem[p..p + bytes].copy_from_slice(&heap[h..h + bytes]);
                wr32(mem, at + off, p);
                if !elem.is_flat() {
                    for k in 0..n {
                        let e = h + k * elem.size;
                        lower(cx, &elem.fixes, heap, &heap[e..e + elem.size], p + k * elem.size, reallocs)?;
                    }
                }
            }
            Fix::Case { off, disc_size, payload_off, cases } => {
                let d = rd_disc(src, *off, *disc_size);
                if let Some(Some(p)) = cases.get(d) {
                    lower(cx, &p.fixes, heap, &src[*payload_off..], at + payload_off, reallocs)?;
                }
            }
        }
    }
    Ok(())
}
