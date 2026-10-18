# Results

1000000 rows on 2 × Intel(R) Xeon(R) Processor @ 2.10GHz,
2026-10-18, with `wp` built by `cargo build --profile dist`.
Made by `bench/run.sh 1000000`.

## Pipe cost by batch size

| command | wall time | peak RSS |
|:--|--:|--:|
| `wp 'gen 1000000 1 \| count 1'` | 3390 ms | 17.4 MiB |
| `wp 'gen-flat 1000000 1 \| count-flat 1'` | 2486 ms | 17.3 MiB |
| `wp 'gen 1000000 16 \| count 16'` | 948 ms | 17.2 MiB |
| `wp 'gen-flat 1000000 16 \| count-flat 16'` | 423 ms | 17.3 MiB |
| `wp 'gen 1000000 256 \| count 256'` | 798 ms | 17.8 MiB |
| `wp 'gen-flat 1000000 256 \| count-flat 256'` | 288 ms | 17.6 MiB |
| `wp 'gen 1000000 4096 \| count 4096'` | 767 ms | 18.9 MiB |
| `wp 'gen-flat 1000000 4096 \| count-flat 4096'` | 270 ms | 17.8 MiB |

## Startup, warm cache

| command | wall time | peak RSS |
|:--|--:|--:|
| `wp 'gen 1 \| count'` | 6 ms | 17.6 MiB |

## Startup, cold cache

| command | wall time | peak RSS |
|:--|--:|--:|
| `wp 'gen 1 \| count'` | 118 ms | 48.9 MiB |

## Log query: `parse-log | top-paths 10`

| command | wall time | peak RSS |
|:--|--:|--:|
| `wp 'parse-log \| top-paths 10'` | 1787 ms | 42.2 MiB |
| `native-logs 10 ` | 284 ms | 19.7 MiB |
| `mawk ' {c[$7]++} END {for (p in c) printf "%9d %s\n", c[p], p}' \| sort -k1,1nr -k2,2 \| head -10` | 953 ms | 17.2 MiB |

## Log query: `parse-log | errors 500 | top-paths 10`

| command | wall time | peak RSS |
|:--|--:|--:|
| `wp 'parse-log \| errors 500 \| top-paths 10'` | 1402 ms | 27.5 MiB |
| `native-logs 10 500` | 174 ms | 3.3 MiB |
| `mawk '$9 >= 500 {c[$7]++} END {for (p in c) printf "%9d %s\n", c[p], p}' \| sort -k1,1nr -k2,2 \| head -10` | 452 ms | 3.3 MiB |

## Host allocations

| pipeline | per run | per row |
|:--|--:|--:|
| `gen 1000000 256 \| count 256` | 2117 | 8.03 |
| `gen-flat 1000000 256 \| count-flat 256` | 2097 | 5.03 |
| `parse-log \| top-paths 10` | 2239 | 15 |
