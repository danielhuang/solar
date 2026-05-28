#!/bin/bash
set -e

cargo build --release -p solar-system
cargo build --all
cargo run --bin compile --release -- "examples/$1.solar" "target/$1"

time "target/$1"
