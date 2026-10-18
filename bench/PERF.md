# Throughput work

## Goal

At least **+25% throughput** (rows per second) on each of the three headline
pipelines, against `main` at 003bacf, as measured by `bench/compare.sh main`:

| pipeline | what it stresses | baseline |
|:--|:--|--:|
| `gen 1000000 \| count` | rows with strings | ~990 ms |
| `gen-flat 1000000 \| count-flat` | flat rows (numbers only) | ~430 ms |
| `parse-log \| top-paths 10` | a realistic query over a 210 MB log | ~2,700 ms |

Rules:

- Wasmtime's public API only, unpatched. No `unsafe` in our code.
- No changes to the WIT types, and no making the example tools' own logic
  faster (the log parser, the counting). Changes to tools only count if every
  tool would benefit, such as how they're built or how they use streams.
- Simplicity and speed together: a change has to pay for its lines.

## Method

`bench/compare.sh` builds the baseline in its own worktree, checks that both
print the same output, then alternates runs of the two and reports the median
change over pairs. Two identical builds agree within ±4% on this machine (a
2-core VM with no hardware counters), so smaller changes are noise.
`bench/profile.sh` counts instructions with callgrind to say *why*.

## Result

Goal met. `PROFILE=dist bench/compare.sh main 1000000 31`, run twice:

| pipeline | `main` | now | throughput |
|:--|--:|--:|--:|
| `gen 1000000 \| count` | 996, 981 ms | 702, 706 ms | **+42%, +42%** |
| `gen-flat 1000000 \| count-flat` | 423, 422 ms | 259, 263 ms | **+61%, +55%** |
| `parse-log \| top-paths 10` | 2672, 2631 ms | 1696, 1750 ms | **+56%, +51%** |

Two changes did it: mimalloc as the host's allocator (step 1) and a `dist`
profile with fat LTO for the binary we ship (step 7). The allocator alone
gives +23%, +46% and +46% (31 pairs), just short on rows with strings.
With mimalloc's eager commit off (step 9), startup and peak RSS are the same
as `main`'s (6 ms and 13 MiB for a small pipeline), and the binary shrinks
from 34 to 24 MB.

The rest of the time is in Wasmtime's copy between components and in the
tools themselves (see the README), so the next big steps are upstream.

## Log

In order. Medians over interleaved runs; "rounds" means a quick comparison
of several builds, "pairs" means `bench/compare.sh` against `main`.

### 1. Host allocator: mimalloc v2 (kept)

Copying a row between components allocates 5–15 times on the host, and
glibc's malloc and free were about 30% of the time. Throughput against
`main`: +29%/+41%/+48% (15 pairs), then +23%/+46%/+46% (31 pairs).

| allocator (9 rounds) | startup | strings | flat | log query | peak RSS |
|:--|--:|--:|--:|--:|--:|
| glibc (`main`) | 7.2 ms | 1027 ms | 464 ms | 2554 ms | 14 / 38 MiB |
| mimalloc v3 | 9.9 ms | 748 ms | 310 ms | 1879 ms | 30 / 52 MiB |
| **mimalloc v2** | 8.1 ms | 785 ms | 293 ms | 1841 ms | 21 / 46 MiB |
| jemalloc | 8.1 ms | 893 ms | 336 ms | 2114 ms | 15 / 61 MiB |

(Peak RSS for a small pipeline / the log query.) v2 is as fast as v3 with
half its memory overhead, which in both is mimalloc committing its arena
up front (`MIMALLOC_ARENA_EAGER_COMMIT=0` removes it, but the crate has no
safe way to set that from code). The `stats` build keeps the system
allocator, since it counts allocations rather than timing them.

### 2. Fat LTO and one codegen unit in `release` (not kept)

+11%/+3%/+14% on top of step 1 (9 rounds, host only). But the tools can't
build with LTO (wit-bindgen links a prebuilt object with no bitcode), and
every release link, including the test binaries, goes from seconds to
minutes. Revisited as step 7.

### 3. A faster allocator in the tools (not possible)

Most guest time is allocation too, so `rlsf` (TLSF, with a safe `const`
constructor) went into the six headline tools. They corrupted their heaps
within a second (`count` trapped in `deallocate` on a pointer made of string
bytes). The likely cause is that wasi-libc's own `malloc` still runs for libc
internals, so two allocators grow the same memory. Not something to fight
from a tool.

### 4. Batch sizes (no change)

The tools' default of 256 rows per read and write is already the best
(7 rounds):

| batch | 64 | 128 | 256 | 512 | 1024 | 2048 |
|:--|--:|--:|--:|--:|--:|--:|
| strings | 846 | 796 | **763** | 800 | 774 | 805 ms |
| flat | 313 | 290 | 297 | 281 | **273** | 288 ms |

### 5. One codegen unit without LTO (no gain)

−5%/+4%/+0% (9 rounds): noise. The gain in step 2 is inlining *across*
crates.

### 6. Thin LTO (no gain)

+0%/−1%/+2% (9 rounds), and incremental builds of `wp` and its tests went
from about a minute to 8.

### 7. A `dist` profile with fat LTO (kept)

The ripgrep approach: `release` stays quick to build and is what the tools,
tests and `cargo wp` use. `cargo build --profile dist -p wp` builds the
binary to ship, with fat LTO and one codegen unit. A clean build takes about
8 minutes on this machine. Together with step 1: +42%/+61%/+56% (31 pairs,
the result above).

### 8. mimalloc's `secure` mode (not kept)

Guard pages, encoded free lists and double-free checks, for a heap at least
as hardened as glibc's. Costs 26–29% throughput (9 rounds, release builds),
which gives back almost the whole gain. Guests can't reach the host heap, so
hardening only matters as a second line of defence against a bug in Wasmtime
or WASI.

### 9. mimalloc without eager arena commit (kept)

mimalloc commits its arena up front, which cost about 6 MiB of private
memory. With it off, interleaved against the `dist` build of step 7 (peak
RSS; throughput over 21 pairs):

| | small pipeline | strings | flat | log query |
|:--|--:|--:|--:|--:|
| peak RSS | 19.8 → 13.7 MiB | 20.0 → 13.8 MiB | 19.7 → 13.7 MiB | 43.8 → 38.3 MiB |
| throughput | | +1.5% | (9 rounds: −2%) | −3.4% |

The throughput changes are within noise, though the log query may pay a few
percent for committing memory as it grows. The crate has no feature for this
option, and its Rust bindings lack the option's constant, so setting it from
code would take an `unsafe` FFI call, a magic number and a hand-built tokio
runtime. Instead `.cargo/config.toml` passes
`-DMI_DEFAULT_ARENA_EAGER_COMMIT=0` to the C build: two lines, but they only
apply to builds from this repository (not `cargo install --git`), and are
skipped if `CFLAGS` is already set. A feature in the mimalloc crate would be
the proper fix.

### Not tried, and why

- Wasmtime `Config` settings: none touch the copy path. The host-to-guest
  call that copying a string makes (about 1,150 instructions of Wasmtime
  machinery each, 3 per `commit`) has no knobs, and call hooks, fuel and
  epochs are already off.
- Pooling allocator, `InstancePre`, reusing stores: these make startup
  cheaper, which is 6 ms of a run.
- Running tools in parallel: streams can't cross Stores, and a Store is
  single-threaded.

## Memory

Resident memory at startup with two tools loaded (`parse-log | top-paths`
paused on an empty stdin):

| | `main` (glibc) | release (mimalloc) | dist |
|:--|--:|--:|--:|
| `wp`'s code pages (shared, clean) | 8.2 | 8.4 | 6.3 MiB |
| shared libraries | 2.1 | 2.1 | 2.1 MiB |
| private: heap, wasm memories, stacks | 2.7 | 13.8 | 13.5 MiB |

About 6 MiB of mimalloc's extra was its eager arena commit, now turned off
(step 9). The log query's peak is mostly data: `top-paths` keeps every
distinct path, and the native version needs 20 MiB for the same.

## Startup

5.3 ms for `gen 1 | count` (dist, hyperfine): 1.5 ms is this VM's cost of
starting any process (`true` takes 1.46 ms), 0.35 ms starting `wp`, about
2.7 ms loading the two tools from Wasmtime's cache, and 0.5 ms instantiating
and running them. The cache hashes each `.wasm`, then reads, decompresses and
copies its compiled code: 1.0–2.2 ms per tool. A precompiled `.cwasm` loads
in 0.23–0.62 ms, but `Component::deserialize_file` is `unsafe`, since it
trusts the file's native code.
