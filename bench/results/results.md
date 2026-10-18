# Results

1000000 rows on 2 × Intel(R) Xeon(R) Processor @ 2.10GHz,
2026-10-18. Made by `bench/run.sh 1000000`.

## Pipe cost by batch size

| command | wall time | peak RSS |
|:--|--:|--:|
| `wp 'gen 1000000 1 \| count 1'` | 3861 ms | 12.9 MiB |
| `wp 'gen-flat 1000000 1 \| count-flat 1'` | 2648 ms | 12.9 MiB |
| `wp 'gen 1000000 16 \| count 16'` | 1218 ms | 12.9 MiB |
| `wp 'gen-flat 1000000 16 \| count-flat 16'` | 607 ms | 13 MiB |
| `wp 'gen 1000000 256 \| count 256'` | 997 ms | 13.1 MiB |
| `wp 'gen-flat 1000000 256 \| count-flat 256'` | 380 ms | 13.1 MiB |
| `wp 'gen 1000000 4096 \| count 4096'` | 1041 ms | 16 MiB |
| `wp 'gen-flat 1000000 4096 \| count-flat 4096'` | 456 ms | 14.7 MiB |

## Startup, warm cache

| command | wall time | peak RSS |
|:--|--:|--:|
| `wp 'gen 1 \| count'` | 6 ms | 13.1 MiB |

## Startup, cold cache

| command | wall time | peak RSS |
|:--|--:|--:|
| `wp 'gen 1 \| count'` | 140 ms | 25.1 MiB |

## Log query: `parse-log | top-paths 10`

| command | wall time | peak RSS |
|:--|--:|--:|
| `wp 'parse-log \| top-paths 10'` | 2742 ms | 37.8 MiB |
| `native-logs 10 ` | 283 ms | 19.7 MiB |
| `mawk ' {c[$7]++} END {for (p in c) printf "%9d %s\n", c[p], p}' \| sort -k1,1nr -k2,2 \| head -10` | 808 ms | 17.2 MiB |

## Log query: `parse-log | errors 500 | top-paths 10`

| command | wall time | peak RSS |
|:--|--:|--:|
| `wp 'parse-log \| errors 500 \| top-paths 10'` | 2335 ms | 24.4 MiB |
| `native-logs 10 500` | 205 ms | 3.3 MiB |
| `mawk '$9 >= 500 {c[$7]++} END {for (p in c) printf "%9d %s\n", c[p], p}' \| sort -k1,1nr -k2,2 \| head -10` | 428 ms | 3.3 MiB |

## Host allocations

| pipeline | per run | per row |
|:--|--:|--:|
| `gen 1000000 256 \| count 256` | 2279 | 8.03 |
| `gen-flat 1000000 256 \| count-flat 256` | 2116 | 5.03 |
| `parse-log \| top-paths 10` | 2239 | 15 |
