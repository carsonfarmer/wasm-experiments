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
