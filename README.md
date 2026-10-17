# wasm-experiments: typed pipes

`wp` runs Unix-style pipelines where every stage is a sandboxed WebAssembly
component and every `|` is a typed WASI 0.3 `stream<T>`:

```
$ wp --stats 'cat access.log | parse-log-cols | errors-cols | top-paths-cols 5'
     2751 /api/v1/checkout
     1161 /
      730 /graphql
      623 /api/v1/uploads
      216 /api/v1/search?q=rust
parallel · load 2.2 ms · run 786.3 ms · 11063 rows · 401 MB in at 510 MB/s
memory: peak RSS 24.6 MB · guest linear memory 9.4 MB · host allocations 10246 (4.1 MB)
```

The host type-checks every pipe before anything runs, carries rows between
components itself, and runs each stage on its own thread.

## Results

2M access-log lines (420 MB) on a 2-core cloud VM, median of 5 interleaved
runs. Every implementation of each query printed byte-identical output.
Full tables: [`bench/results/latest.md`](bench/results/latest.md).

**Top 10 paths over all requests** (every row crosses a pipe):

| | time | vs `mawk` |
|---|---:|---:|
| `wp`, columnar, projected, parallel | 838 ms | **2.0× faster** |
| `wp`, rows, projected, parallel | 1,125 ms | 1.5× faster |
| native Rust: the same parser in one process, no sandbox | 663 ms | 2.5× faster |
| `mawk \| sort \| head` | 1,675 ms | |
| `wp` rows using Wasmtime's built-in copy | 5,743 ms | 3.4× slower |

**Cost of the pipe itself** (1M synthetic rows with three strings each, `gen | count`):

| copy engine | rows/s | host allocations |
|---|---:|---:|
| Wasmtime's built-in copy | 1.0 M | 8,030,096 |
| host pump | 2.4 M | 34,130 |
| host pump, parallel stages | 3.1 M | 29,167 |
| columnar batches, parallel | 10.3 M | 4,662 |

## What makes it fast

1. **A host pump instead of Wasmtime's copy.** Wasmtime copies a row with
   pointers between components by lifting it into a dynamic `Val` tree and
   lowering it again: one heap allocation per field name, string and list
   element (a `list<u8>` costs one per byte). The pump compiles the row type
   into a copy plan once, stages rows as raw canonical-ABI bytes, and lowers
   them with `memcpy` plus one `realloc` per string. Stages are recycled, so
   forwarding allocates nothing per row.
2. **Projection.** A consumer may declare a record with a subset of the
   producer's fields (`top-paths` reads `record { path: string }` from
   `request` rows). This is plain WIT record width-subtyping; the pump only
   copies the fields the consumer declared, in both directions. On the row
   path that cut `realloc`s from 4 to 1 per row; on the columnar path it cut
   staged bytes from 359 MB to 37 MB.
3. **Columnar batches.** A stream element can be a whole batch: a WIT record
   of columns, with strings in Arrow's layout (`data` + `offsets`). A batch
   of thousands of rows moves with about a dozen copies.
4. **Parallel stages.** Each component gets its own Store and thread, like
   Unix processes, and the pump bridges the Stores. Wasmtime streams can't
   cross Stores on their own.
5. **Instant startup.** Compiled components are cached on disk, so loading a
   pipeline takes about 1 ms.

## Rows or columnar?

Both are built and benchmarked, side by side.

| | rows (`stream<request>`) | columnar (`stream<request-batch>`) |
|---|---|---|
| Tool code | Natural: one typed record per row | Builders and offset lookups (`tools/cols`) |
| WIT | Any record | A record of columns, following a convention |
| Cost per hop | One `realloc` per string or list the consumer reads | About a dozen copies per batch, whatever its size |
| Q2 above | 1,125 ms | 838 ms |
| `take N` | Exact, per row | Exact; the host cuts the last batch by its offsets |
| Projection | Yes | Yes (whole columns) |

**Suggestion:** use rows as the default authoring model. With projection and
parallel stages they already beat `mawk`. Reach for columnar on hot, wide
streams where per-string allocation dominates. Converting between the two
formats would be a small component; it isn't written yet.

## Findings for Wasmtime

- **Guest-to-guest stream copies allocate per value**
  (`futures_and_streams.rs`, `copy` → `Val::load` / `Val::store`). A
  plan-based copy like the pump's is 2–3× faster with ~275× fewer
  allocations. This looks like a clear general improvement to propose
  upstream.
- **There's no dynamically typed host stream API.** A host can pass a
  `StreamAny` through but can't read or write elements whose type it learns
  at runtime. The pump works around this with `Elem<SIZE, ALIGN>`, a
  const-generic element type on Wasmtime's `#[doc(hidden)]` lift/lower hooks,
  so it is pinned to Wasmtime 49.0.3. A `StreamReader<Val>`-style API would
  remove the workaround.
- **Streams can't cross Stores.** The pump bridges them, which is what lets
  each stage run on its own thread.

## Quick start

You need Rust with the `wasm32-wasip2` target. The host embeds Wasmtime 49.

```sh
rustup target add wasm32-wasip2
cargo tools                 # build the components
cargo build --release       # build wp, loggen and the native baseline
cargo test-all              # unit + end-to-end tests (needs `cargo tools` first)

./target/release/wp 'gen 5 | show'
./target/release/wp 'gen | take 10 | show'                      # cancels the infinite producer
./target/release/wp 'zoo 1000 7 | zoo-check-lite 1000 7'        # every WIT value kind, projected
./target/release/loggen 1000000 access.log
./target/release/wp --stats 'cat access.log | parse-log | top-paths 10'
./target/release/wp --serial --copy wasmtime '...'              # compare engines
```

Arguments after a tool's name become its `run` parameters. Omitted optional
parameters are `none`, and `_` skips one: `gen _ 64` runs forever, writing 64
rows at a time.

## Tests

`cargo test-all` runs every pipeline against all three engines (Wasmtime
copy, host pump, parallel):

- **Type zoo:** every value kind the pump copies (nested lists, records,
  options, results, variants, enums, flags, tuples, chars, unicode, empty
  lists) round-trips identically, including through projection and `take`.
- **`take`:** exact row counts at batch boundaries, for rows and columnar
  batches; chained takes; cancellation of infinite producers.
- **Log queries:** results match a native computation.
- **Pump behavior:** which pipes it carries, what projection copies, and that
  steady-state forwarding doesn't allocate per row.
- **Errors:** type mismatches, bad projections, malformed pipelines and bad
  arguments are rejected with useful messages before anything runs.

## Benchmarks

`bench/bench.py` generates the log if needed and runs every scenario with
interleaved repeats. It fails if any two implementations of a query
disagree, then writes `bench/results/latest.{md,json}`.

## Layout

```
host/src/pump/      the host pump
  layout.rs         canonical-ABI layouts from Wasmtime's public types
  plan.rs           copy plans and projections
  stage.rs          staging rows, and copying them in and out of components
  elem.rs           the runtime-typed stream element (on hidden Wasmtime APIs)
  pipe.rs           the pipe halves, take, columnar batch cutting
host/src/run.rs     parsing, type checking, serial and parallel runners
host/tests/         end-to-end tests
wit/                row types and every tool's world
tools/              components; logparse and cols are shared libraries
bench/              loggen, the native baseline, bench.py, results
```

## Limits and next steps

- The pump relies on hidden Wasmtime APIs, so Wasmtime upgrades need care.
- Only UTF-8 components are supported, and rows can't contain resources or
  nested streams yet.
- On 2 cores, parallel stages help pipelines whose stages are balanced. A
  data-parallel fan-out (several `parse-log` instances over one file) is the
  next big win.
- A component converting between rows and columnar batches.
- Projection pushdown into producers, so `parse-log` doesn't build fields
  nobody reads.
