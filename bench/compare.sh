#!/usr/bin/env bash
# Compare speed with another commit: throughput on the three headline
# pipelines (rows with strings, flat rows and a log query), and startup
# (a one-row pipeline, run 10 times as often).
#
#   bench/compare.sh [REF] [ROWS] [RUNS]     (default: main, 1000000, 15)
#
# REF is built in its own worktree under target/compare/. Both builds of wp
# use $PROFILE (default: release), or release for a REF that lacks it; $BIN
# replaces this tree's build with a given binary (e.g. a PGO build). Runs
# of the two alternate, so a noisy machine slows both alike, and the
# change in speed is the median over pairs of runs.
set -euo pipefail
cd "$(dirname "$0")/.."
ref=${1:-main} rows=${2:-1000000} runs=${3:-15} profile=${PROFILE:-release}
base=target/compare/$(git rev-parse --short "$ref")
[[ -d $base ]] || git worktree add -q --detach "$base" "$ref"
grep -q "^\[profile\.$profile\]" "$base/Cargo.toml" && ref_profile=$profile || ref_profile=release
(cd "$base" && cargo tools -q && cargo build --profile "$ref_profile" -q -p wp)
cargo tools -q
cargo build --release -q -p loggen
[[ -n ${BIN:-} ]] || cargo build --profile "$profile" -q -p wp
ref_bin=$base/target/$ref_profile/wp bin=${BIN:-target/$profile/wp}
# Run copies of both: a copy starts faster than the file the linker wrote (it
# takes fewer page faults), which would skew startup unless both are copies.
tmp=$(mktemp -d -p target); trap 'rm -rf "$tmp"' EXIT
cp "$ref_bin" "$tmp/ref"; cp "$bin" "$tmp/this"; ref_bin=$tmp/ref bin=$tmp/this
log=bench/data/access-$rows.log
mkdir -p bench/data
[[ -f $log ]] || target/release/loggen "$rows" "$log"

echo "| pipeline | $ref ($ref_profile) | this tree (${BIN:-$profile}) | faster |"
echo "|:--|--:|--:|--:|"
wp() { # DIR BIN: run pipeline $p with BIN and DIR's tools
  WP_PATH=$1/target/wasm32-wasip2/release "$2" "$p" < "$log"
}
run() { # DIR BIN: one run's wall time in ms
  local start=$EPOCHREALTIME
  wp "$1" "$2" > /dev/null
  echo "$start $EPOCHREALTIME" | awk '{ print ($2 - $1) * 1000 }'
}
median() { sort -g | awk '{ a[NR] = $1 } END { print (a[int((NR + 1) / 2)] + a[int(NR / 2) + 1]) / 2 }'; }
for p in "gen $rows | count" "gen-flat $rows | count-flat" "parse-log | top-paths 10" "gen 1 | count"; do
  n=$runs; [[ $p == "gen 1 | count" ]] && n=$((runs * 10))
  [[ $(wp "$base" "$ref_bin") == "$(wp . "$bin")" ]] || { echo "different output: $p" >&2; exit 1; }
  pairs=$(for i in $(seq "$n"); do
    if ((i % 2)); then a=$(run "$base" "$ref_bin") b=$(run . "$bin")
    else b=$(run . "$bin") a=$(run "$base" "$ref_bin"); fi
    echo "$a $b"
  done)
  a=$(cut -d' ' -f1 <<< "$pairs" | median) b=$(cut -d' ' -f2 <<< "$pairs" | median)
  gain=$(awk '{ print ($1 / $2 - 1) * 100 }' <<< "$pairs" | median)
  printf "| \`%s\` | %.1f ms | %.1f ms | %+.0f%% |\n" "${p//|/\\|}" "$a" "$b" "$gain"
done
