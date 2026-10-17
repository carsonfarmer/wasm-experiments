//! Process-level measurements: host heap allocations, peak RSS, and the
//! linear memory the components grew to.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

pub static ALLOCS: AtomicU64 = AtomicU64::new(0);
pub static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

/// A global allocator that counts host heap allocations. Install it in a
/// binary with `#[global_allocator]`.
pub struct CountingAlloc;

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        ALLOC_BYTES.fetch_add(layout.size() as u64, Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        ALLOC_BYTES.fetch_add(layout.size() as u64, Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        ALLOC_BYTES.fetch_add(new_size.saturating_sub(layout.size()) as u64, Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// Host allocation counters at a point in time.
#[derive(Clone, Copy, Debug, Default)]
pub struct Allocs {
    pub count: u64,
    pub bytes: u64,
}

impl Allocs {
    pub fn now() -> Allocs {
        Allocs { count: ALLOCS.load(Relaxed), bytes: ALLOC_BYTES.load(Relaxed) }
    }
    pub fn since(self, earlier: Allocs) -> Allocs {
        Allocs { count: self.count - earlier.count, bytes: self.bytes - earlier.bytes }
    }
}

/// Peak resident set size of this process, in bytes (Linux only).
pub fn peak_rss() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

/// Tracks how far the components' linear memories grew.
#[derive(Default)]
pub struct MemoryTracker {
    pub linear_bytes: u64,
}

impl wasmtime::ResourceLimiter for MemoryTracker {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        self.linear_bytes += (desired - current) as u64;
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(true)
    }
}
