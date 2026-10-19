# Results

1000000 rows on 4 × Intel(R) Xeon(R) Processor @ 2.10GHz,
2026-10-19, with `wp` built by `bench/pgo.sh`.
Made by `bench/run.sh 1000000`.

## Pipe cost by batch size

| command | wall time | peak RSS |
|:--|--:|--:|
| `wp 'gen 1000000 1 \| count 1'` | 3508 ms | 8.3 MiB |
| `wp 'gen-flat 1000000 1 \| count-flat 1'` | 2507 ms | 8.3 MiB |
| `wp 'gen 1000000 16 \| count 16'` | 966 ms | 8.2 MiB |
| `wp 'gen-flat 1000000 16 \| count-flat 16'` | 386 ms | 8.3 MiB |
| `wp 'gen 1000000 256 \| count 256'` | 701 ms | 8.9 MiB |
| `wp 'gen-flat 1000000 256 \| count-flat 256'` | 282 ms | 8.8 MiB |
| `wp 'gen 1000000 4096 \| count 4096'` | 736 ms | 11 MiB |
| `wp 'gen-flat 1000000 4096 \| count-flat 4096'` | 266 ms | 9.6 MiB |

## Startup, warm cache

| command | wall time | peak RSS |
|:--|--:|--:|
| `wp 'gen 1 \| count'` | 6 ms | 8.6 MiB |

## Startup, cold cache

| command | wall time | peak RSS |
|:--|--:|--:|
| `wp 'gen 1 \| count'` | 86 ms | 30.4 MiB |

## Log query: `parse-log | top-paths 10`

| command | wall time | peak RSS |
|:--|--:|--:|
| `wp 'parse-log \| top-paths 10'` | 1719 ms | 33.5 MiB |
| `native-logs 10 ` | 350 ms | 19.7 MiB |
| `mawk ' {c[$7]++} END {for (p in c) printf "%9d %s\n", c[p], p}' \| sort -k1,1nr -k2,2 \| head -10` | 1153 ms | 17.2 MiB |

## Log query: `parse-log | errors 500 | top-paths 10`

| command | wall time | peak RSS |
|:--|--:|--:|
| `wp 'parse-log \| errors 500 \| top-paths 10'` | 1432 ms | 19.2 MiB |
| `native-logs 10 500` | 247 ms | 3.3 MiB |
| `mawk '$9 >= 500 {c[$7]++} END {for (p in c) printf "%9d %s\n", c[p], p}' \| sort -k1,1nr -k2,2 \| head -10` | 566 ms | 3.4 MiB |

## Host allocations

| pipeline | per run | per row |
|:--|--:|--:|
| `gen 1000000 256 \| count 256` | 2040 | 8.03 |
| `gen-flat 1000000 256 \| count-flat 256` | 2047 | 5.03 |
| `parse-log \| top-paths 10` | 2164 | 15 |
