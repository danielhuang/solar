#!/usr/bin/env bash
# Build both benchmarks (release) and print the timing table.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "Building Solar runtime (release)..."
cargo build --release -p solar-system

echo "Compiling examples/hashmap.solar (release)..."
cargo run --quiet --bin compile -- examples/hashmap.solar target/hashmap

echo "Building Rust reference (release)..."
( cd bench/rust && cargo build --release --quiet )

echo
python3 bench/run.py
