//! A stage is one batch of rows held by the host between two components.
//!
//! Rows are kept in the producer's canonical-ABI layout: `fixed` holds each
//! row's fixed-size block back to back, and `heap` the strings and lists the
//! consumer will read, with their `(ptr, len)` rewritten to point into
//! `heap`. Lifting is a few `memcpy`s; lowering is a `memcpy` per batch (or
//! per projected field) plus one `realloc` + `memcpy` per string or list.

use super::pipe::Counters;
use super::plan::{Fix, Plan, Proj};
use anyhow::{Result, anyhow, ensure};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use wasmtime::component::__internal::LowerContext;

#[derive(Default)]
pub struct Stage {
    pub fixed: Vec<u8>,
    pub heap: Vec<u8>,
    pub rows: usize,
    /// Producer row size.
    pub size: usize,
    /// How consumer rows are built from staged ones, and their size.
    pub proj: Option<Arc<Proj>>,
    pub dst_size: usize,
    pub counters: Arc<Counters>,
}

fn rd32(buf: &[u8], at: usize) -> usize {
    u32::from_le_bytes(buf[at..at + 4].try_into().unwrap()) as usize
}

fn wr32(buf: &mut [u8], at: usize, v: usize) {
    buf[at..at + 4].copy_from_slice(&(v as u32).to_le_bytes());
}

fn rd_disc(buf: &[u8], at: usize, size: usize) -> usize {
    let mut d = [0u8; 4];
    d[..size].copy_from_slice(&buf[at..at + size]);
    u32::from_le_bytes(d) as usize
}

/// `src[ptr..ptr + len]`, or an error if a guest handed us a bad pointer.
fn slice(src: &[u8], ptr: usize, len: usize) -> Result<&[u8]> {
    ptr.checked_add(len).and_then(|end| src.get(ptr..end)).ok_or_else(|| anyhow!("pointer out of bounds"))
}

impl Stage {
    pub fn row(&self, i: usize) -> &[u8] {
        &self.fixed[i * self.size..(i + 1) * self.size]
    }

    /// Copy the row at `ptr` in the producer's memory `src` into this stage.
    pub fn push_row(&mut self, plan: &Plan, src: &[u8], ptr: usize) -> Result<u32> {
        let at = self.fixed.len();
        self.fixed.extend_from_slice(slice(src, ptr, plan.size)?);
        lift(&plan.fixes, src, &mut self.fixed, &mut self.heap, false, at)?;
        self.rows += 1;
        Ok(self.rows as u32 - 1)
    }

    /// Write rows `first..first + n` into the consumer's memory at `dst`.
    pub fn lower_rows<T>(
        &self,
        cx: &mut LowerContext<'_, T>,
        first: usize,
        n: usize,
        dst: usize,
    ) -> wasmtime::Result<()> {
        let t0 = std::time::Instant::now();
        let mut reallocs = 0;
        let result = match self.proj.as_deref() {
            // Same row type: one memcpy for every row's fixed block.
            Some(Proj::Copy(plan)) => {
                write(cx, dst, &self.fixed[first * self.size..(first + n) * self.size])?;
                (0..n).try_for_each(|k| {
                    lower(cx, &plan.fixes, &self.heap, self.row(first + k), dst + k * self.dst_size, &mut reallocs)
                })
            }
            Some(proj) => (0..n).try_for_each(|k| {
                lower_proj(cx, proj, &self.heap, self.row(first + k), dst + k * self.dst_size, &mut reallocs)
            }),
            None => Err(wasmtime::format_err!("stage has no projection")),
        };
        self.counters.lower_ns.fetch_add(t0.elapsed().as_nanos() as u64, Relaxed);
        self.counters.reallocs.fetch_add(reallocs, Relaxed);
        result
    }
}

/// Follow the pointers of the value at `base` in `fixed` (or in `heap`),
/// which still hold the producer's pointers: copy their targets out of `src`
/// into `heap` and point them there instead.
fn lift(fixes: &[Fix], src: &[u8], fixed: &mut Vec<u8>, heap: &mut Vec<u8>, in_heap: bool, base: usize) -> Result<()> {
    for fix in fixes {
        match fix {
            Fix::Str { off } | Fix::List { off, .. } => {
                let at = base + off;
                let (elem_size, align) = match fix {
                    Fix::List { elem, .. } => (elem.size, elem.align),
                    _ => (1, 1),
                };
                let buf: &[u8] = if in_heap { heap } else { fixed };
                let (p, n) = (rd32(buf, at), rd32(buf, at + 4));
                ensure!(p % align == 0, "list pointer not aligned");
                let data = slice(src, p, n.checked_mul(elem_size).ok_or_else(|| anyhow!("length overflow"))?)?;
                if matches!(fix, Fix::Str { .. }) {
                    std::str::from_utf8(data).map_err(|_| anyhow!("invalid UTF-8 in string"))?;
                }
                let h = heap.len();
                heap.extend_from_slice(data);
                wr32(if in_heap { heap } else { fixed }, at, h);
                if let Fix::List { elem, .. } = fix
                    && !elem.is_flat()
                {
                    (0..n).try_for_each(|k| lift(&elem.fixes, src, fixed, heap, true, h + k * elem.size))?;
                }
            }
            Fix::Case { off, disc, payload, cases } => {
                let d = rd_disc(if in_heap { heap } else { fixed }, base + off, *disc);
                match cases.get(d) {
                    Some(Some(p)) => lift(&p.fixes, src, fixed, heap, in_heap, base + payload)?,
                    Some(None) => {}
                    None => anyhow::bail!("invalid discriminant {d}"),
                }
            }
        }
    }
    Ok(())
}

fn write<T>(cx: &mut LowerContext<'_, T>, at: usize, bytes: &[u8]) -> wasmtime::Result<()> {
    cx.as_slice_mut()
        .get_mut(at..at + bytes.len())
        .ok_or_else(|| wasmtime::format_err!("read buffer out of bounds"))?
        .copy_from_slice(bytes);
    Ok(())
}

/// Build one projected value at `at` in the consumer from the staged `src`.
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
            write(cx, at, &src[..plan.size])?;
            lower(cx, &plan.fixes, heap, src, at, reallocs)
        }
        Proj::Fields(picks) => {
            picks.iter().try_for_each(|p| lower_proj(cx, &p.proj, heap, &src[p.src..], at + p.dst, reallocs))
        }
    }
}

/// Follow the pointers of a value already copied into the consumer at `at`.
/// `src` is the same value in the stage, so pointers are read from host
/// memory; each string or list is allocated in the consumer and filled from `heap`.
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
            Fix::Str { off } | Fix::List { off, .. } => {
                let (elem_size, align) = match fix {
                    Fix::List { elem, .. } => (elem.size, elem.align),
                    _ => (1, 1),
                };
                let (h, n) = (rd32(src, *off), rd32(src, off + 4));
                let bytes = n * elem_size;
                let p = cx.realloc(0, 0, align as u32, bytes)?;
                *reallocs += 1;
                write(cx, p, &heap[h..h + bytes])?;
                wr32(cx.as_slice_mut(), at + off, p);
                if let Fix::List { elem, .. } = fix
                    && !elem.is_flat()
                {
                    for k in 0..n {
                        let e = h + k * elem.size;
                        lower(cx, &elem.fixes, heap, &heap[e..e + elem.size], p + k * elem.size, reallocs)?;
                    }
                }
            }
            Fix::Case { off, disc, payload, cases } => {
                if let Some(Some(p)) = cases.get(rd_disc(src, *off, *disc)) {
                    lower(cx, &p.fixes, heap, &src[*payload..], at + payload, reallocs)?;
                }
            }
        }
    }
    Ok(())
}
