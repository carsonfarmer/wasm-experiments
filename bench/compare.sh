#!/usr/bin/env bash
# Compare throughput with another commit on the three headline pipelines:
# rows with strings, flat rows and a log query.
#
#   bench/compare.sh [REF] [ROWS] [RUNS]     (default: main, 1000000, 15)
#
# REF is built in its own worktree under target/compare/. Both builds of wp
# use $PROFILE (default: release), which REF must also define. Runs of the
# two alternate, so a noisy machine slows both alike, and the throughput
# change is the median over pairs of runs.
set -euo pipefail
cd "$(dirname "$0")/.."
ref=${1:-main} rows=${2:-1000000} runs=${3:-15} profile=${PROFILE:-release}
base=target/compare/$(git rev-parse --short "$ref")
[[ -d $base ]] || git worktree add -q --detach "$base" "$ref"
(cd "$base" && cargo tools -q && cargo build --profile "$profile" -q -p wp)
cargo tools -q
cargo build --release -q -p loggen
cargo build --profile "$profile" -q -p wp
log=bench/data/access-$rows.log
mkdir -p bench/data
[[ -f $log ]] || target/release/loggen "$rows" "$log"

echo "| pipeline | $ref | this tree | throughput |"
echo "|:--|--:|--:|--:|"
wp() { # DIR: run pipeline $p with DIR's build
  WP_PATH=$1/target/wasm32-wasip2/release "$1/target/$profile/wp" "$p" < "$log"
}
run() { # DIR: one run's wall time in ms
  local start=$EPOCHREALTIME
  wp "$1" > /dev/null
  echo "$start $EPOCHREALTIME" | awk '{ print ($2 - $1) * 1000 }'
}
median() { sort -g | awk '{ a[NR] = $1 } END { print (a[int((NR + 1) / 2)] + a[int(NR / 2) + 1]) / 2 }'; }
for p in "gen $rows | count" "gen-flat $rows | count-flat" "parse-log | top-paths 10"; do
  [[ $(wp "$base") == "$(wp .)" ]] || { echo "different output: $p" >&2; exit 1; }
  pairs=$(for i in $(seq "$runs"); do
    if ((i % 2)); then a=$(run "$base") b=$(run .)
    else b=$(run .) a=$(run "$base"); fi
    echo "$a $b"
  done)
  a=$(cut -d' ' -f1 <<< "$pairs" | median) b=$(cut -d' ' -f2 <<< "$pairs" | median)
  gain=$(awk '{ print ($1 / $2 - 1) * 100 }' <<< "$pairs" | median)
  printf "| \`%s\` | %.0f ms | %.0f ms | %+.0f%% |\n" "${p//|/\\|}" "$a" "$b" "$gain"
done
