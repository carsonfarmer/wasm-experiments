#!/usr/bin/env bash
# End-to-end checks for the spike's milestones.
#
#   cargo tools && cargo build --release -p wp && tests/smoke.sh
set -uo pipefail
cd "$(dirname "$0")/.."
export RUST_BACKTRACE=0
WP=./target/release/wp
fails=0

check() { # name, condition result (0 = pass)
  if [ "$2" -eq 0 ]; then echo "ok    $1"; else echo "FAIL  $1"; fails=$((fails + 1)); fi
}

# 1. Two components, one stream.
out=$($WP 'gen 5 | show')
[ "$(echo "$out" | wc -l)" -eq 5 ] && [[ "$out" == *"change #4"* ]]
check "gen 5 | show prints 5 commits" $?

# 2. Generic take on two differently shaped row types.
out=$($WP 'gen 20 | take 3 | show' 2>/dev/null)
[ "$(echo "$out" | wc -l)" -eq 3 ] && [[ "$out" == *"change #2"* ]] && [[ "$out" != *"change #3"* ]]
check "take 3 forwards exactly 3 commits" $?

out=$($WP 'fruit | take 4 | weigh' 2>/dev/null)
[ "$(echo "$out" | wc -l)" -eq 4 ] && [[ "$out" == *"[fuzzy, green, tart]"* ]] && [[ "$out" == *"87.5 g"* ]]
check "take works on fruit (lists, options, enums)" $?

out=$($WP 'gen 2 | take 10 | show')
[ "$(echo "$out" | wc -l)" -eq 2 ]
check "take ends cleanly when the producer ends first" $?

out=$($WP 'gen 5 | take 0 | show')
[ -z "$out" ]
check "take 0 forwards nothing" $?

# 3. Cancellation: an infinite producer stops once take has enough.
start=$(date +%s%N)
err=$(timeout 10 $WP 'gen | take 10 | show' 2>&1 >/dev/null)
code=$?
ms=$(( ($(date +%s%N) - start) / 1000000 ))
[ $code -eq 0 ] && [[ "$err" == *"reader hung up after 10 commits"* ]] && [ $ms -lt 5000 ]
check "infinite gen | take 10 exits (${ms} ms) and gen stops after 10" $?

# Type errors are caught before anything runs.
err=$($WP 'gen 3 | weigh' 2>&1)
[ $? -ne 0 ] && [[ "$err" == *"\`weigh\` expects stream<record { name: string"* ]]
check "type mismatch is rejected before running" $?

err=$($WP 'gen 3 | take 2 | weigh' 2>&1)
[ $? -ne 0 ] && [[ "$err" == *"but \`take 2\` produces"* ]]
check "type mismatch is caught through take" $?

echo
if [ $fails -eq 0 ]; then echo "all checks passed"; else echo "$fails check(s) failed"; exit 1; fi
