#!/usr/bin/env bash
# Throughput baseline: rows/sec through one pipe, with and without the host's
# generic `take` in the middle, at a few batch sizes.
#
#   cargo tools && cargo build --release -p wp && bench/run.sh [ROWS] [RUNS]
set -euo pipefail
cd "$(dirname "$0")/.."

ROWS=${1:-1000000}
RUNS=${2:-3}
WP=./target/release/wp

# Best (max) rows/sec over RUNS runs of one pipeline.
best() {
  local pipeline=$1 best=0
  for _ in $(seq "$RUNS"); do
    local r
    r=$("$WP" --time "$pipeline" 2>&1 >/dev/null | sed -n 's/.* \([0-9.]*\) M rows\/s/\1/p' | tail -1)
    best=$(awk -v a="$best" -v b="$r" 'BEGIN { print (b > a) ? b : a }')
  done
  echo "$best"
}

echo "rows per run: $ROWS · best of $RUNS · record { sha, author, message: string, time: s64 }"
echo
echo "| batch | gen → count (M rows/s) | gen → take → count (M rows/s) |"
echo "|------:|-----------------------:|------------------------------:|"
for batch in 1 16 256 4096; do
  direct=$(best "gen $ROWS $batch | count $batch")
  viatake=$(best "gen $ROWS $batch | take $ROWS | count $batch")
  printf "| %5s | %22s | %29s |\n" "$batch" "$direct" "$viatake"
done
