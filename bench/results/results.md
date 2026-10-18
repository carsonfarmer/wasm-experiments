# Results

1000000 rows on 2 × Intel(R) Xeon(R) Processor @ 2.10GHz,
2026-10-18, with `wp` built by `cargo build --profile dist`.
Made by `bench/run.sh 1000000`.

## Pipe cost by batch size

| command | wall time | peak RSS |
|:--|--:|--:|
| `wp 'gen 1000000 1 \| count 1'` | 3983 ms | 12.9 MiB |
| `wp 'gen-flat 1000000 1 \| count-flat 1'` | 3032 ms | 12.8 MiB |
| `wp 'gen 1000000 16 \| count 16'` | 1115 ms | 12.9 MiB |
| `wp 'gen-flat 1000000 16 \| count-flat 16'` | 457 ms | 12.7 MiB |
| `wp 'gen 1000000 256 \| count 256'` | 710 ms | 13.4 MiB |
| `wp 'gen-flat 1000000 256 \| count-flat 256'` | 263 ms | 13.4 MiB |
| `wp 'gen 1000000 4096 \| count 4096'` | 708 ms | 16.1 MiB |
| `wp 'gen-flat 1000000 4096 \| count-flat 4096'` | 276 ms | 14.9 MiB |

## Startup, warm cache

| command | wall time | peak RSS |
|:--|--:|--:|
| `wp 'gen 1 \| count'` | 6 ms | 13.4 MiB |

## Startup, cold cache

| command | wall time | peak RSS |
|:--|--:|--:|
| `wp 'gen 1 \| count'` | 129 ms | 30.9 MiB |

## Log query: `parse-log | top-paths 10`

| command | wall time | peak RSS |
|:--|--:|--:|
| `wp 'parse-log \| top-paths 10'` | 1781 ms | 38 MiB |
| `native-logs 10 ` | 277 ms | 19.7 MiB |
| `mawk ' {c[$7]++} END {for (p in c) printf "%9d %s\n", c[p], p}' \| sort -k1,1nr -k2,2 \| head -10` | 958 ms | 17.1 MiB |

## Log query: `parse-log | errors 500 | top-paths 10`

| command | wall time | peak RSS |
|:--|--:|--:|
| `wp 'parse-log \| errors 500 \| top-paths 10'` | 1392 ms | 23.8 MiB |
| `native-logs 10 500` | 189 ms | 3.4 MiB |
| `mawk '$9 >= 500 {c[$7]++} END {for (p in c) printf "%9d %s\n", c[p], p}' \| sort -k1,1nr -k2,2 \| head -10` | 464 ms | 3.4 MiB |

## Host allocations

| pipeline | per run | per row |
|:--|--:|--:|
| `gen 1000000 256 \| count 256` | 2117 | 8.03 |
| `gen-flat 1000000 256 \| count-flat 256` | 2116 | 5.03 |
| `parse-log \| top-paths 10` | 2238 | 15 |
