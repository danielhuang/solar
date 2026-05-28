#!/bin/bash
set -e

cargo build --release -p solar-system
cargo build --all
cargo run --bin compile -- "examples/$1.solar" "target/$1"

samply record "target/$1"