#!/usr/bin/env bash
# Build wp with profile-guided optimization: the dist profile, trained on the
# benchmark pipelines. Writes target/pgo/wp. Needs the llvm-tools component
# (rustup component add llvm-tools).
#
#   bench/pgo.sh
set -euo pipefail
cd "$(dirname "$0")/.."
host=$(rustc -vV | sed -n 's/^host: //p')
dir=$PWD/target/pgo
build() { # TARGET-DIR RUSTFLAGS: a dist build of wp
  RUSTFLAGS=$2 cargo build --profile dist -q -p wp --target "$host" --target-dir "$dir/$1"
}
cargo tools -q
cargo build --release -q -p loggen
rm -rf "$dir/profiles"
build instrumented "-Cprofile-generate=$dir/profiles"
wp=$dir/instrumented/$host/dist/wp
export WP_PATH=target/wasm32-wasip2/release
target/release/loggen 300000 "$dir/train.log"
"$wp" 'gen 300000 | count' > /dev/null
"$wp" 'gen-flat 300000 | count-flat' > /dev/null
"$wp" 'parse-log | top-paths 10' < "$dir/train.log" > /dev/null
for _ in $(seq 20); do "$wp" 'gen 1 | count' > /dev/null; done
"$(rustc --print sysroot)/lib/rustlib/$host/bin/llvm-profdata" merge -o "$dir/wp.profdata" "$dir/profiles"
build optimized "-Cprofile-use=$dir/wp.profdata"
cp "$dir/optimized/$host/dist/wp" "$dir/wp"
