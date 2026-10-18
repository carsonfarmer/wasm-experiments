#!/usr/bin/env bash
# Where the time goes: instructions per row (a run over ROWS rows, less a run
# over none), and the share of them in Wasmtime's copy between components.
# Needs valgrind.
#
#   bench/profile.sh [ROWS]    (default: 100000)
set -euo pipefail
cd "$(dirname "$0")/.."
rows=${1:-100000}
export WP_PATH=target/wasm32-wasip2/release
cargo tools -q
cargo build --release -q -p loggen
cargo build --profile profiling -q -p wp
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
target/release/loggen "$rows" "$tmp/log"

profile() { # PIPELINE INPUT: total instructions, and those inside Instance::copy
  valgrind --tool=callgrind --callgrind-out-file="$tmp/cg" target/profiling/wp "$1" < "$2" > /dev/null 2>&1
  callgrind_annotate --inclusive=yes "$tmp/cg" | tr -d , | awk '
    /PROGRAM TOTALS/ { total = $1 }
    /futures_and_streams.rs:<wasmtime::runtime::component::instance::Instance>::copy/ && !copy { copy = $1 }
    END { print total, copy + 0 }'
}

echo "| pipeline | instructions per row | in Wasmtime's copy |"
echo "|:--|--:|--:|"
for p in "gen-flat ROWS | count-flat" "gen ROWS | count" "parse-log | top-paths"; do
  profile "${p//ROWS/0}" /dev/null > /dev/null # compile into the cache first
  read -r none _ < <(profile "${p//ROWS/0}" /dev/null)
  read -r all copy < <(profile "${p//ROWS/$rows}" "$tmp/log")
  echo "| \`${p//ROWS/$rows}\` | $(((all - none) / rows)) | $((100 * copy / (all - none)))% |"
done
