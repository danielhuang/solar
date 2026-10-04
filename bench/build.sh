#!/usr/bin/env bash
# Build every benchmark group documented in guide.md.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release -p solar-system
cargo build --release --bin solar
for stem in allocs3 threads_list2 splay allocs5 sieve hashmap binarytrees binarytrees_st loop2 loop2fn5; do
  target/release/solar compile --release "examples/$stem.solar" "target/$stem"
done

make -B -C bench/c
clang -O3 -fPIC -ftls-model=initial-exec -shared \
  -o bench/c/libbump.so bench/c/bump.c

(
  cd bench/go
  for stem in allocs3 threads_list2 splay allocs5 sieve; do
    go build -o "$stem" "$stem.go"
  done
)

/usr/lib/jvm/java-21-openjdk-amd64/bin/javac bench/java/*.java
for project in allocs3 threads_list2 splay allocs5 sieve; do
  "$HOME/.dotnet/dotnet" build "bench/csharp/$project" -c Release --nologo
done

cargo build --release --manifest-path bench/rust/Cargo.toml
mkdir -p target/bench
g++ -O3 -march=native -std=c++17 \
  bench/binarytrees_arena.cpp -o target/bench/bt_arena -lpthread
gcc -O3 -march=native \
  bench/binaryTrees_vanilla.c -o target/bench/bt_vanilla -lm
