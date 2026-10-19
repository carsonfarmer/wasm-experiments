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
from a tool. (Wrong: see step 18 for the real cause.)

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

# Round 2: throughput and startup

## Goal

At least **+25% throughput** on each headline pipeline and **25% faster
startup** (`gen 1 | count`, warm cache), against `main` at b0ae819 with both
sides built `dist`, as measured by `PROFILE=dist bench/compare.sh main
1000000 31`, twice. Same rules as round 1, plus: peak RSS at most `main`'s +
1 MiB, and any extra build step must be one command.

## Method

Round 1's `compare.sh` built `main` with `release` even with `PROFILE=dist`,
which credited fat LTO's ~10% to whatever was measured. Now both sides use
`$PROFILE` (or `release` for a REF that lacks it), a fourth row times startup
(10 × RUNS pairs), and `BIN=` measures a binary that isn't a cargo profile.
It runs copies of both binaries: a copy of a binary starts about 6% faster
than the file the linker wrote (1,119 vs 1,150 page faults), which inflated
the startup row of every `BIN=` build until this was fixed.

Machine: a 4-vCPU Intel Xeon @ 2.1 GHz cloud VM. `main` against itself:
+3% / +3% / +2% / −1%, then −1% / −0% / +2% / +1%, so under ~4% is noise.
`main` here: about 875 ms / 330 ms / 2,000 ms / 7.9 ms, drifting by up to
10% during the day. `perf` works with software clock events, and with
`Config::profiler(PerfMap)` it names JIT-compiled guest functions.

## Result

Met for rows with strings and flat rows, not for the log query or startup.
Everything kept (fat LTO for the tools, PGO, `panic = "abort"` and three
`Config` settings), with `BIN=target/pgo/wp PROFILE=dist bench/compare.sh
main 1000000 31`, twice:

| pipeline | `main` | now | faster |
|:--|--:|--:|--:|
| `gen 1000000 \| count` | 879, 881 ms | 706, 686 ms | **+30%, +26%** |
| `gen-flat 1000000 \| count-flat` | 340, 329 ms | 261, 264 ms | **+29%, +27%** |
| `parse-log \| top-paths 10` | 2,056, 2,013 ms | 1,768, 1,744 ms | +16%, +17% |
| `gen 1 \| count` (startup) | 8.9, 8.4 ms | 7.6, 7.2 ms | +18%, +18% |

On pipelines PGO wasn't trained on: `zoo | zoo-check` +14%, `parse-log |
errors 404 | top-paths 20` +12%, `gen | show` +36%. Peak RSS is 3 MiB lower
(9.1 instead of 12.3 MiB for a small pipeline, 34.1 instead of 37.2 MiB for
the log query), and the binary is 18 MB instead of 24.

What's left of the log query is mostly Wasmtime's copy (a `Val` per field,
a `realloc` call into the tool per string) and the tools' `malloc` (step
18), and what's left of startup is mostly the kernel and Wasmtime loading
compiled tools from its cache ("Where startup goes"). Both need changes
upstream.

## Log

Columns: strings / flat / log query / startup, against `main`, both `dist`,
31 pairs (310 for startup).

### 10. Wasmtime 50.0.0-rc.1 (no gain)

0% / −0% / −0% / −1%. It builds without changes and passes every test, so
upgrading is free when it's released, but it doesn't make `wp` faster.

### 11. Fat LTO for the tools (kept)

+6% / −1% / +8% / +6%, then +6% / +3% / +6% / +6%. Step 2 found that
wit-bindgen's prebuilt object blocks LTO, but that no longer reproduces
(Rust 1.97, wit-bindgen 0.63): every tool links and passes the tests. The
`cargo tools` alias passes `--config profile.release.lto=true`, so only the
wasm builds pay for it. The log parser lives in its own crate, which LTO can
now inline into the tools, and the tools are 15% smaller, which speeds
startup. One codegen unit on top: −5% / +2% / +2% / −5% (not kept).

### 12. Loading a pipeline's tools in parallel (no gain)

−3% / −0% / −0% / +3%, with one thread per tool, although loading is about
3 ms of the 8. Why isn't clear: much of loading is page faults and `mmap`
and `mprotect` calls on the one address space (see "Where startup goes"),
which may serialize.

### 13. PGO (kept)

`bench/pgo.sh` builds `dist` with `-Cprofile-generate`, runs the three
benchmark pipelines and startup, and builds again with `-Cprofile-use`. Two
runs: +15% / +32% / +15%, then +11% / +37% / +15% / +6%. Most of the time is
in Wasmtime's branchy, generic `Val` code, which is what PGO is good at.

On pipelines it wasn't trained on: +16% on `parse-log | errors 404 |
top-paths 20` and +18% on `gen | show`, but −2% on `zoo | zoo-check`: PGO had
laid out the code for variants, lists and flags as cold. So the training now
includes `zoo`, which has every kind of WIT value. With 30,000 `zoo` rows it
dominated the profile and flat rows got only +20%, so the mix is now 300,000
commits, 1,000,000 ticks, 300,000 log lines, 20 startups and 3,000 `zoo`
rows: +6% on flat rows over the first mix, and `zoo` +14%. Two full `dist`
builds take about 15 minutes on 4 cores. Needs `rustup component add
llvm-tools`.

### 14. BOLT on top of PGO (no gain)

LLVM 18's BOLT, instrumented and trained like PGO, then
`-reorder-blocks=ext-tsp -reorder-functions=cdsort -split-functions
-split-all-cold -icf`, measured directly against the PGO build: +2% / −0% /
+1% / +1%. PGO already lays out the hot code, and the rest isn't worth
another tool and four build steps.

### 15. `panic = "abort"` in `dist` (kept)

−1% / +4% / +3% / −, then −2% / +5% / +5% / +1%, and a 2.4 MB smaller
binary. Marginal, but it costs one line.

### 16. `target-cpu=x86-64-v3` (not kept)

−3% / −10% / −2% / ~0%, and the binary would need a 2013 or later CPU.

### 17. snmalloc and rpmalloc (not kept)

snmalloc 0% / +1% / −0% / −8%; rpmalloc −2% / −3% / −3% / −13% (startup
corrected for the copy effect). Neither beats mimalloc v2.

### 18. A faster allocator in the tools: the real cause (blocked)

Guest `malloc` and `free` are about 12% of all the time in both the strings
pipeline and the log query (perf with JIT symbols), so this is worth having.
`dlmalloc`'s `GlobalDlmalloc` crashes the same way `rlsf` did, but only in
the tool that *reads* rows. The cause: with any `#[global_allocator]`, the
component's canonical-ABI `realloc` (the exported `cabi_realloc`) resolves
to wasi-libc's weak definition, which calls libc `realloc`, instead of
std's, which calls the Rust allocator. So the strings the host writes into a
tool come from wasi-libc's `malloc`, and Rust frees them with the custom
allocator. Without a custom allocator both are the same `malloc`, which hides
the problem. `-C link-arg=--undefined=cabi_realloc` doesn't change which one
wins, and defining our own `cabi_realloc` needs `unsafe`. This needs a fix in
Rust's std or wasi-libc for `wasm32-wasip2`.

### 19. Startup: RELR, no address map, no unwind info, exiting early

- `-z pack-relative-relocs` turns the 441 KB of 18,209 relative relocations
  that the dynamic loader applies at every start into 4.8 KB: +1% on startup
  (not kept; it would also need glibc 2.36).
- `Config::generate_address_map(false)`: compiled tools 15% smaller, +5% on
  startup. Trap backtraces keep function names but lose wasm offsets.
- `Config::native_unwind_info(false)`: +4% on startup. Only third-party
  unwinders need it; Wasmtime's own backtraces don't.
- Both kept, as one change.
- `std::process::exit` instead of dropping the engine and tools: +1% (not
  kept).

### 20. Only the Wasmtime features `wp` uses (not kept)

`default-features = false` with the twelve features `wp` needs (and `wat` for
the tests only), on top of everything kept: a 2 MB smaller binary, and
+3% / −0% / −0% / +2% against the stack. Not worth a feature list to review
at every Wasmtime upgrade.

### 21. The cache's recompression (turned off)

The cache counts each entry's uses in a background thread (rewriting a stats
file each time, about 2% of startup), and at the 256th use recompresses the
entry at zstd level 20. From a fresh cache, runs 256 and 257 of `gen-flat |
count-flat` peaked at 170 MiB instead of 10 MiB, which also made
`results.md` report 73.5 MiB for that pipeline. That happens to every entry,
again after every rebuild of a tool or upgrade of Wasmtime.
`with_optimized_compression_usage_counter_threshold(u64::MAX)` turns it off;
the counting can't be turned off.

## Where startup goes

`perf` over 50 runs of `gen 1 | count`: 46% of the time is in the kernel (32%
page faults, about 1,150 per run), 30% in `wp` (zstd 8%, SHA-256 2%), 10% in
the dynamic loader and 4% in libc. Most page faults come from loading the
tools: Wasmtime's cache reads each compiled tool, decompresses it with
`zstd::decode_all` into a growing `Vec` (the frames don't record their size,
so the decoder also allocates a 2 MiB window), and copies it once more into
executable memory, so every byte lands in fresh memory two or three times.
Decompressing into one right-sized buffer would be a small change upstream.

With everything kept, a start takes 882 page faults instead of 1,104. What's
left in `wp` is mostly zstd (7%), SHA-256 (2%), detecting CPU features (3%:
libgcc, `std_detect` and the RNG `WasiCtx` seeds; `cpuid` traps to the
hypervisor on a VM) and interning WASI's names in the `Linker` (1.5%).

## Where the time goes now

`perf` with JIT symbols (release build): guest code is 33% of the strings
pipeline and 37% of the log query, a third of it in `dlmalloc`. In the host,
building and collecting `Vec<(String, Val)>` for each record is about 17%,
`Instance::options_memory_mut` (looked up for every value lowered) 6.5%,
`Val::load` 5.6%, `LowerContext::realloc` 3.2%, `Val::store` 2.8% and UTF-8
validation 2.5%.
