#!/usr/bin/env bash
# Benchmarks. Needs hyperfine, mawk and jq.
#
#   bench/run.sh [ROWS]        (default: 1000000)
#
# Checks that every contender gives the same answer, then times them. Writes
# bench/results/results.md, and hyperfine's raw json to target/bench/.
set -euo pipefail
cd "$(dirname "$0")/.."
rows=${1:-1000000}
out=bench/results raw=target/bench
mkdir -p bench/data "$out" "$raw"
export LC_ALL=C WP_PATH=target/wasm32-wasip2/release

cargo tools -q
cargo build --release -q -p wp -p native-logs -p loggen
cargo build --release -q -p wp --features stats --target-dir target/stats
log=bench/data/access-$rows.log
[[ -f $log ]] || target/release/loggen "$rows" "$log"
wp=target/release/wp
results=$out/results.md
cat > "$results" <<EOF
# Results

$rows rows on $(nproc) × $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2 | xargs),
$(date -u +%F). Made by \`bench/run.sh $rows\`.
EOF

run() { # NAME TITLE HYPERFINE-ARGS...: time commands and add a table of results
  local name=$1 title=$2; shift 2
  hyperfine --style basic --metrics default --export-json "$raw/$name.json" "$@"
  printf '\n## %s\n\n| command | wall time | peak RSS |\n|:--|--:|--:|\n' "$title" >> "$results"
  jq -r '.results[] | .summary as $s | "| `\(.command | sub("^<\\S+ "; "") | gsub("target/release/"; "")
    | gsub("\\|"; "\\|"))` | \($s.time_wall_clock.mean * 1000 | round) ms | \($s.memory_peak_resident.mean
    / 1048576 * 10 | round / 10) MiB |"' "$raw/$name.json" >> "$results"
}

same() { # INPUT CMD...: every command prints the same thing
  local input=$1 want; want=$(eval "$2" < "$input")
  for cmd in "${@:3}"; do
    diff <(echo "$want") <(eval "$cmd" < "$input") > /dev/null || { echo "different output: $cmd" >&2; exit 1; }
  done
}

# 1. What a pipe costs per row: rows with strings vs flat rows, by batch size.
for p in "gen $rows 256 | count 256" "gen-flat $rows 256 | count-flat 256"; do
  [[ $($wp "$p") == "$rows" ]] || { echo "wrong count: $p" >&2; exit 1; }
done
run pipes "Pipe cost by batch size" --runs 5 -L batch 1,16,256,4096 \
  "$wp 'gen $rows {batch} | count {batch}'" "$wp 'gen-flat $rows {batch} | count-flat {batch}'"

# 2. Startup: a one-row pipeline with a warm and a cold compile cache.
cold=$(mktemp -d)
run startup-warm "Startup, warm cache" --warmup 3 "$wp 'gen 1 | count'"
run startup-cold "Startup, cold cache" --env "XDG_CACHE_HOME=$cold" --prepare "rm -rf $cold/wasmtime" "$wp 'gen 1 | count'"
rm -rf "$cold"

# 3. Log queries: wp vs the same Rust natively vs mawk.
awk_top() { printf '%s' "mawk '$1 {c[\$7]++} END {for (p in c) printf \"%9d %s\\n\", c[p], p}' | sort -k1,1nr -k2,2 | head -10"; }
for q in "top-paths:" "errors:500"; do
  name=${q%%:*} min=${q#*:}
  pipeline="parse-log | ${min:+errors $min | }top-paths 10"
  contenders=("$wp '$pipeline'" "target/release/native-logs 10 $min" "$(awk_top "${min:+\$9 >= $min}")")
  same "$log" "${contenders[@]}"
  run "logs-$name" "Log query: \`$pipeline\`" --warmup 1 --runs 5 --shell=bash \
    "<$log ${contenders[0]}" "<$log ${contenders[1]}" "<$log ${contenders[2]}"
done

# 4. Host allocations per row: a run over every row, less a run over none.
allocs() { target/stats/release/wp --stats "$1" < "$2" 2>&1 > /dev/null | tail -1 | jq .allocs; }
printf '\n## Host allocations\n\n| pipeline | per run | per row |\n|:--|--:|--:|\n' >> "$results"
for p in "gen ROWS 256 | count 256" "gen-flat ROWS 256 | count-flat 256" "parse-log | top-paths 10"; do
  none=$(allocs "${p//ROWS/0}" /dev/null) all=$(allocs "${p//ROWS/$rows}" "$log")
  p=${p//ROWS/$rows}
  echo "| \`${p//|/\\|}\` | $none | $(jq -n "($all - $none) / $rows * 100 | round / 100") |" >> "$results"
done
cat "$results"
