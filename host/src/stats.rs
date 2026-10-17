//! Process-level measurements: host allocations (via `stats_alloc`), peak
//! RSS, and how far the components' linear memories grew.

pub use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

/// Peak resident set size of this process, in bytes.
pub fn peak_rss() -> u64 {
    // SAFETY: `getrusage` only writes the struct it's given.
    let usage = unsafe {
        let mut usage = std::mem::zeroed::<libc::rusage>();
        libc::getrusage(libc::RUSAGE_SELF, &mut usage);
        usage
    };
    let max = usage.ru_maxrss as u64;
    if cfg!(target_os = "macos") { max } else { max * 1024 }
}

/// Tracks how far the components' linear memories grew.
#[derive(Default)]
pub struct MemoryTracker {
    pub linear_bytes: u64,
}

impl wasmtime::ResourceLimiter for MemoryTracker {
    fn memory_growing(&mut self, current: usize, desired: usize, _: Option<usize>) -> wasmtime::Result<bool> {
        self.linear_bytes += (desired - current) as u64;
        Ok(true)
    }

    fn table_growing(&mut self, _: usize, _: usize, _: Option<usize>) -> wasmtime::Result<bool> {
        Ok(true)
    }
}
