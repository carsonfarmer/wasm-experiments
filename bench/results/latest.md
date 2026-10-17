Ran 2026-10-16T23:35:23 on x86_64, 2 cores, Linux 6.18.44-fc-v139; median of 5 interleaved runs after a warm-up.
Logs: 2,000,000 lines, 420 MB. Synthetic: 1,000,000 rows.
Every implementation of Q1 and Q2 printed byte-identical output.

### Pipe cost: gen → count (synthetic commits)

| | median | best | throughput | peak RSS | host allocs | reallocs | staged | host copy time |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| rows · Wasmtime copy | 983 ms | 891 ms | 1.02 M rows/s | 15 MB | 8,030,096 |  |  |  |
| rows · host pump | 426 ms | 412 ms | 2.35 M rows/s | 16 MB | 34,130 | 3,000,000 | 67 MB | 182 ms |
| rows · host pump · parallel | 319 ms | 305 ms | 3.14 M rows/s | 17 MB | 29,167 | 3,000,000 | 67 MB | 251 ms |
| columnar · host pump | 123 ms | 119 ms | 8.14 M rows/s | 17 MB | 4,839 | 1,715 | 55 MB | 6 ms |
| columnar · host pump · parallel | 97 ms | 90 ms | 10.29 M rows/s | 17 MB | 4,662 | 1,715 | 55 MB | 9 ms |
| columnar · Wasmtime copy | 387 ms | 368 ms | 2.58 M rows/s | 15 MB | 18,737 |  |  |  |

### Q1: top 10 paths returning 5xx

| | median | best | throughput | peak RSS | host allocs | reallocs | staged | host copy time |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| wp rows · Wasmtime copy | 4,722 ms | 4,570 ms | 89 MB/s | 29 MB | 30,012,389 | 11,063 | 1 MB | 4 ms |
| wp rows | 1,776 ms | 1,563 ms | 237 MB/s | 28 MB | 10,478 | 8,011,063 | 402 MB | 816 ms |
| wp rows · parallel | 1,045 ms | 1,025 ms | 402 MB/s | 28 MB | 10,139 | 8,011,063 | 402 MB | 875 ms |
| wp columnar | 727 ms | 689 ms | 578 MB/s | 28 MB | 10,540 | 5,614 | 360 MB | 69 ms |
| wp columnar · parallel | 657 ms | 620 ms | 640 MB/s | 25 MB | 10,238 | 5,614 | 360 MB | 78 ms |
| native Rust (same parser, 1 process) | 309 ms | 285 ms | 1358 MB/s | 12 MB |  |  |  |  |
| mawk \| sort \| head | 846 ms | 803 ms | 497 MB/s | 12 MB |  |  |  |  |

### Q2: top 10 paths, all requests

| | median | best | throughput | peak RSS | host allocs | reallocs | staged | host copy time |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| wp rows · whole rows · Wasmtime copy | 5,743 ms | 5,121 ms | 73 MB/s | 54 MB | 30,009,921 |  |  |  |
| wp rows · whole rows | 2,587 ms | 2,292 ms | 162 MB/s | 55 MB | 6,698 | 8,000,000 | 401 MB | 1,083 ms |
| wp rows · projected | 1,616 ms | 1,567 ms | 260 MB/s | 50 MB | 6,629 | 2,000,000 | 141 MB | 306 ms |
| wp rows · projected · parallel | 1,125 ms | 1,043 ms | 374 MB/s | 50 MB | 6,287 | 2,000,000 | 141 MB | 302 ms |
| wp columnar · whole batches | 1,176 ms | 1,115 ms | 358 MB/s | 55 MB | 6,710 | 4,812 | 359 MB | 76 ms |
| wp columnar · projected | 1,048 ms | 1,024 ms | 401 MB/s | 51 MB | 6,591 | 802 | 37 MB | 10 ms |
| wp columnar · projected · parallel | 838 ms | 808 ms | 502 MB/s | 50 MB | 6,238 | 802 | 37 MB | 12 ms |
| native Rust (same parser, 1 process) | 663 ms | 593 ms | 634 MB/s | 38 MB |  |  |  |  |
| mawk \| sort \| head | 1,675 ms | 1,660 ms | 251 MB/s | 31 MB |  |  |  |  |
