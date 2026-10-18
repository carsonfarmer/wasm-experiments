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

## Log

Each change against `main`, by `bench/compare.sh main` unless noted.

### 1. Host allocator: mimalloc v2 (kept)

Copying a row between components allocates 5–15 times on the host (see the
README), and glibc's malloc and free were about 30% of the time.

| | strings | flat | log query |
|:--|--:|--:|--:|
| throughput | **+29%** | **+41%** | **+48%** |

Startup goes from 7.2 to 8.1 ms and peak RSS from 14 to 21 MiB: a fixed
+7 MiB from mimalloc committing its arena up front, the same at any size.
Alternatives, medians over 9 interleaved rounds:

| allocator | startup | strings | flat | log query | peak RSS |
|:--|--:|--:|--:|--:|--:|
| glibc (`main`) | 7.2 ms | 1027 ms | 464 ms | 2554 ms | 14 / 38 MiB |
| mimalloc v3 | 9.9 ms | 748 ms | 310 ms | 1879 ms | 30 / 52 MiB |
| mimalloc v2 | 8.1 ms | 785 ms | 293 ms | 1841 ms | 21 / 46 MiB |
| jemalloc | 8.1 ms | 893 ms | 336 ms | 2114 ms | 15 / 61 MiB |

(Peak RSS for a small pipeline / the log query.) v3's extra memory is the
same eager arena commit (`MIMALLOC_ARENA_EAGER_COMMIT=0` removes it), but
the crate has no safe way to set it. The `stats` build keeps the system
allocator, since it counts allocations rather than timing them.
