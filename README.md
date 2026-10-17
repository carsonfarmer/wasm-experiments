# wasm-experiments: typed pipes, spike 1

`wp` runs Unix-style pipelines where every stage is a WebAssembly component
and every `|` is a typed WASI 0.3 `stream<T>`:

```
$ wp 'gen | take 3 | show'
gen: reader hung up after 3 commits; stopping
0000000000  ada      change #0
9e3779b97f  grace    change #1
3c6ef372fe  linus    change #2
```

This spike answers one question: **can the host forward a `stream<T>` when it
only learns `T` at runtime?** That's what lets structural operations like
`take`, `skip`, `tee` and `merge` live in the shell instead of in every tool.

**Answer: yes in practice, no through Wasmtime's public API.** Details below.

## Quick start

You need Rust with the `wasm32-wasip2` target. The host embeds Wasmtime 49, so
no separate `wasmtime` install is needed.

```sh
rustup target add wasm32-wasip2
cargo tools                     # builds the components into target/wasm32-wasip2/release/
cargo build --release           # builds the `wp` host
tests/smoke.sh                  # end-to-end checks
```

Then:

```sh
./target/release/wp 'gen 5 | show'               # two components, one stream
./target/release/wp 'gen | take 10 | show'       # infinite producer, cancelled by take
./target/release/wp 'fruit | take 4 | weigh'     # same take, different row type
./target/release/wp 'gen 3 | weigh'              # type error, caught before running
./target/release/wp --time 'gen 1000000 | count' # throughput
```

Arguments after a tool's name become its `run` parameters; omitted optional
ones are `none`, and `_` skips one (`gen _ 64` = forever, 64 rows per write).

## Findings

**1. Async components with streams work today on stable tooling.** The tools
are plain Rust built for `wasm32-wasip2` with `wit-bindgen` 0.63; exports like
`run: async func(...) -> stream<commit>` just work. No `wasip3` target needed.

**2. Passing a stream through untouched is free.** The host hands the
producer's stream straight to the consumer as a `StreamAny`, with no code per
type. Wasmtime copies elements guest-to-guest.

**3. Reading elements of a runtime-only type is not in the public API.**
Every host-side read or write (`StreamReader<T>`, `Source<T>`,
`Destination<T>`) needs a Rust type `T: Lift + Lower` known at compile time.
There's no `Val`-typed stream.

**The workaround** ([`host/src/dynval.rs`](host/src/dynval.rs)):
`Elem<SIZE, ALIGN>` is a stand-in element type whose canonical-ABI layout is a
const generic. Its lift/lower walk the *runtime* type description to deep-copy
values between guest memories. The shell computes the element's layout from
the component's types and dispatches to the matching `Elem`. This uses
Wasmtime's `#[doc(hidden)]` `__internal` hooks, so it's pinned to 49.0.3. It
covers records, tuples, strings, lists, options, results, enums, variants and
flags; resources and nested streams are rejected.

**4. The upstream ask is small.** Wasmtime already does exactly this
internally: guest-to-guest copies of pointer-containing payloads go through
`Val::load` / `Val::store` (`futures_and_streams.rs`, `copy`). Exposing a
dynamically typed host reader/writer (e.g. `StreamReader<Val>`) would replace
the whole workaround.

**5. Cancellation works end to end.** `take N` hangs up on the producer after
N rows. `gen` sees it on its next write and stops after exactly N. An
infinite `gen | take 10 | show` exits in about 130 ms, including compiling
both components.

**6. Pipes are type-checked before anything runs**, using each component's
own exported types, including through host built-ins:

```
$ wp 'gen 3 | take 2 | weigh'
Error: `weigh` expects stream<record { name: string, ... }>, but `take 2` produces stream<record { sha: string, ... }>
```

## Throughput baseline

`bench/run.sh`: 1M rows of `record { sha, author, message: string, time: s64 }`,
best of 3, on a 2-core cloud VM.

| rows per write | gen → count | gen → take → count |
|------:|----------:|---------:|
|     1 | 0.38 M/s | 0.17 M/s |
|    16 | 0.84 M/s | 1.04 M/s |
|   256 | 1.01 M/s | 1.14 M/s |
|  4096 | 0.93 M/s | 1.29 M/s |

What it says:

- **Batch at least ~16 rows per write.** One row per write is 2-7x slower.
- **The copy dominates, not the producer.** Building 1M rows inside `gen`
  takes ~160 ms; moving them through one pipe takes ~1 s. Each row carries 3
  strings, and each string costs an allocation call into the receiving
  component.
- **The host hop isn't the bottleneck.** Going through `take` is as fast or
  faster than the direct path, because the host buffers more rows per transfer.

This supports the earlier design direction: keep rows out of per-hop copies
(host-owned row store, handles, or flat payloads, which Wasmtime moves with a
plain memcpy).

## Layout

```
wit/demo.wit       row types (commit, fruit) and each tool's world
host/              the `wp` CLI: parsing, type checks, running, built-ins
  src/dynval.rs    runtime-typed stream elements (the workaround)
  src/take.rs      `take N`, generic over any supported element layout
tools/             components: gen, show, count (commits); fruit, weigh (fruit)
bench/run.sh       throughput table
tests/smoke.sh     end-to-end checks for every milestone
```

## Next

- Ask upstream for a dynamically typed stream API, then delete `dynval.rs`'s
  hidden-API use.
- Add `skip`, `tee` and `merge` built-ins on the same machinery.
- Measure flat (pointer-free) rows to quantify the memcpy fast path.
- Start the host-owned row store, and record per-step file reads for the
  perfect-cache CI.
