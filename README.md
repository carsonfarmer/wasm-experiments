# wp

Unix pipes for WebAssembly components, with typed rows instead of bytes.

```console
$ wp 'parse-log | errors 500 | top-paths 3' < access.log
     1396 /api/v1/checkout
      580 /
      361 /graphql
```

Each tool is a sandboxed [WASI 0.3] component. Tools pass each other
*records* over native component-model streams, so no tool re-parses text,
and every pipe is type-checked before anything runs:

```console
$ wp 'gen 3 | top-paths'
wp: `top-paths` reads stream<request>, but `gen` produces stream<commit>
```

## Tools

A tool is any component that exports `run`:

- if its first parameter is a `stream<T>`, the tool before it feeds it (at the
  start of a pipeline, a `stream<u8>` is fed stdin);
- if it returns a `stream<U>`, the tool after it reads it;
- the remaining parameters come from the command line, as [WAVE] values
  (`42`, `some(42)`, `none`, `[1, 2]`, `{x: 1}`). Strings can be bare, and
  trailing `option`s can be left out;
- if the last tool returns anything else, `wp` prints it as WAVE.

To add a tool, drop `NAME.wasm` into a directory on `WP_PATH`
(`:`-separated, default `.`). `wp --list` shows what's there:

```console
$ wp --list
count(input: stream<commit>, batch: option<u32>) -> u64
errors(input: stream<request>, min-status: option<u16>) -> stream<request>
parse-log(input: stream<u8>) -> stream<request>
top-paths(input: stream<request>, n: option<u32>)
...
```

### Writing one

Describe it in WIT. Rows that tools share go in an interface they `use`, and
two tools can be piped together when their row types match exactly
(the same fields with the same names, types and order):

```wit
world errors {
  use logs.{request};
  export run: async func(input: stream<request>, min-status: option<u16>) -> stream<request>;
}
```

Then implement it in any language with component-model async support. In
Rust, with [wit-bindgen]:

```rust
wit_bindgen::generate!({ world: "errors", path: "../../wit" });

struct Errors;

impl Guest for Errors {
    async fn run(
        mut input: wit_bindgen::StreamReader<Request>,
        min_status: Option<u16>,
    ) -> wit_bindgen::StreamReader<Request> {
        let min = min_status.unwrap_or(500);
        let (mut tx, rx) = wit_stream::new();
        wit_bindgen::spawn_local(async move {
            loop {
                let (_, mut rows) = input.read(Vec::with_capacity(8192)).await;
                if rows.is_empty() {
                    return; // the writer is done
                }
                rows.retain(|r| r.status >= min);
                if !rows.is_empty() && !tx.write_all(rows).await.is_empty() {
                    return; // the reader stopped reading
                }
            }
        });
        rx
    }
}

export!(Errors);
```

Build it with `cargo build --release --target wasm32-wasip2` and put the
`.wasm` on `WP_PATH`.

The [example tools](tools) cover byte input (`parse-log`), filters
(`errors`), sinks that print (`show`, `top-paths`) or return a value
(`count`), infinite producers (`gen`, which stops when its reader does), and
every kind of WIT value (`zoo`).

## As a library

```rust
let wp = wp::Wp::new(vec!["tools".into()])?;
let pipeline = wp.pipeline("gen 1000 | count")?; // parsed and type-checked
let wasi = WasiCtx::builder().inherit_stdout().inherit_stderr().build();
let result = pipeline.run(wasi, tokio::io::empty()).await?; // Some(Val::U64(1000))
```

## How it works

`wp` uses only Wasmtime's public API. Loading a tool reads its compiled code
from Wasmtime's on-disk cache, and the pipeline is checked with
`Component::component_type()`, comparing `Type`s structurally. To run, every
tool is instantiated in one `Store`, each `run` is started with
`Func::call_concurrent`, and each returned stream is handed to the next tool
as its argument. Wasmtime checks each stream's type again as it's passed,
copies rows from one component's memory into the next one's, and runs all the
tools concurrently on one thread. Nothing in `wp` touches a row.

## Performance

One million rows on a 2-core VM, with `wp` built by
`cargo build --profile dist -p wp` ([all results](bench/results/results.md),
from `bench/run.sh`):

| | wall time | peak RSS |
|:--|--:|--:|
| startup, warm cache (`gen 1 \| count`) | 6 ms | 18 MiB |
| `gen \| count`: commits, 3 strings each | 0.80 s | 18 MiB |
| `gen-flat \| count-flat`: ticks, 4 numbers each | 0.29 s | 18 MiB |
| `parse-log \| top-paths 10` over a 210 MB log | 1.79 s | 42 MiB |
| the same query in native Rust (same parser, no wasm) | 0.28 s | 20 MiB |
| the same query in `mawk \| sort \| head` | 0.95 s | 17 MiB |

That's 42–61% more throughput than the first version, from a faster
allocator and link-time optimization ([bench/PERF.md](bench/PERF.md)). `wp`
is still about 6× slower than native code. A profile (callgrind, with
`bench/profile.sh`) shows where the time goes. 61–96% of it is Wasmtime
copying rows between components, not the tools:

| pipeline | instructions per row | in Wasmtime's copy |
|:--|--:|--:|
| `gen-flat \| count-flat` | 2,590 | 96% |
| `gen \| count` | 9,760 | 61% |
| `parse-log \| top-paths` | 16,160 | 63% |

- Only streams of primitive numbers are copied with a `memcpy`. Every other
  payload, even a record of four numbers, is lifted into a `Vec<Val>` and
  lowered again one row at a time ([a TODO in Wasmtime][flat-todo]).
- A `Val::Record` owns its field names as `String`s, so every row copied
  allocates them: 5 host allocations per `tick`, 8 per `commit`.
- Each string copied into a tool is a call into that tool's `realloc`, about
  1,150 instructions of Wasmtime's call machinery each.

All three are Wasmtime's to fix, and the next step for this project.

[flat-todo]: https://github.com/bytecodealliance/wasmtime/blob/a33c18ed395ab09d8366d593c7941fc479efa9c6/crates/cranelift/src/compiler/component.rs#L823-L824

## Development

```console
$ cargo tools        # build the example tools
$ cargo test-all     # unit and end-to-end tests
$ cargo wp 'gen 5 | show'
$ bench/run.sh       # benchmarks (needs hyperfine, mawk and jq)
$ bench/profile.sh   # where the time goes (needs valgrind)
$ bench/compare.sh   # throughput against main
$ cargo build --profile dist -p wp   # the binary to ship (slow to link)
```

[WASI 0.3]: https://wasi.dev
[WAVE]: https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wasm-wave
[wit-bindgen]: https://github.com/bytecodealliance/wit-bindgen
