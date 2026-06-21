//! Reference HashMap benchmark mirroring `examples/hashmap.solar`.
//!
//! Uses std's `HashMap` with the foldhash hasher (fixed seed) and the language's
//! default hashing: `#[derive(Hash)]` for struct keys, the built-in `Hash` impls
//! for primitives. The workload (splitmix64 key stream, insert/hit/miss loops,
//! checksum) is identical to the Solar version, so the per-phase checksums match
//! across the two implementations — a cross-implementation correctness check.
//!
//!   cargo run --release            # run every phase (stdin is EOF)
//!   echo 2 | cargo run --release   # run only phase 2
//! A phase index on stdin selects a single phase; EOF runs all.

use foldhash::fast::FixedState;
use std::collections::HashMap;
use std::hash::Hash;
use std::io::Read;

const N: u64 = 1_000_000;

type Map<K> = HashMap<K, u64, FixedState>;

fn new_map<K: Eq + Hash>() -> Map<K> {
    HashMap::with_hasher(FixedState::default())
}

/// splitmix64 — identical constants to the Solar version.
#[inline(always)]
fn rng_next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E3779B97F4A7C15);
    let s = *state;
    let z1 = s ^ (s >> 30);
    let z2 = z1.wrapping_mul(0xBF58476D1CE4E5B9);
    let z3 = z2 ^ (z2 >> 27);
    let z4 = z3.wrapping_mul(0x94D049BB133111EB);
    z4 ^ (z4 >> 31)
}

#[derive(Hash, PartialEq, Eq, Clone, Copy)]
struct Point {
    x: i64,
    y: i64,
}

#[derive(Hash, PartialEq, Eq, Clone, Copy)]
struct Mixed {
    a: u64,
    b: u32,
    c: bool,
}

/// Run insert / hit / miss loops and return the hasher-independent checksum.
/// `build` draws from the RNG state and returns `(key, value)`; it draws exactly
/// as many values per call as the matching Solar phase (one for the primitive
/// phases, two for the struct phases), keeping the RNG streams in lockstep.
fn run<K: Eq + Hash>(n: u64, mut build: impl FnMut(&mut u64) -> (K, u64)) -> u64 {
    let mut m: Map<K> = new_map();

    let mut st = 0x1234567u64;
    for _ in 0..n {
        let (k, v) = build(&mut st);
        m.insert(k, v);
    }

    let mut st = 0x1234567u64;
    let mut sum = 0u64;
    for _ in 0..n {
        let (k, _) = build(&mut st);
        if let Some(v) = m.get(&k) {
            sum = sum.wrapping_add(*v);
        }
    }

    let mut miss_st = 0xDEADBEEFu64;
    let mut misses = 0u64;
    for _ in 0..n {
        let (k, _) = build(&mut miss_st);
        if m.get(&k).is_none() {
            misses = misses.wrapping_add(1);
        }
    }

    sum.wrapping_add(m.len() as u64).wrapping_add(misses)
}

fn bench_u64(n: u64) -> u64 {
    run(n, |st| {
        let r = rng_next(st);
        (r, r)
    })
}
fn bench_u32(n: u64) -> u64 {
    run(n, |st| {
        let r = rng_next(st);
        ((r & 0xFFFF_FFFF) as u32, r)
    })
}
fn bench_point(n: u64) -> u64 {
    let mask = 0x3FFF_FFFF_FFFF_FFFFu64;
    run(n, move |st| {
        let r = rng_next(st);
        let r2 = rng_next(st);
        (
            Point {
                x: (r & mask) as i64,
                y: (r2 & mask) as i64,
            },
            r,
        )
    })
}
fn bench_mixed(n: u64) -> u64 {
    run(n, |st| {
        let r = rng_next(st);
        let r2 = rng_next(st);
        (
            Mixed {
                a: r,
                b: (r2 & 0xFFFF_FFFF) as u32,
                c: (r & 1) == 0,
            },
            r,
        )
    })
}

/// Read an optional phase index from stdin. Returns -1 (run all) on EOF / no
/// leading digit.
fn read_phase() -> i64 {
    let mut buf = [0u8; 16];
    let n = std::io::stdin().read(&mut buf).unwrap_or(0);
    let mut val: i64 = 0;
    let mut any = false;
    for &c in &buf[..n] {
        if c.is_ascii_digit() {
            val = val * 10 + (c - b'0') as i64;
            any = true;
        } else if any {
            break;
        }
    }
    if any { val } else { -1 }
}

fn main() {
    let phase = read_phase();
    if phase == -1 || phase == 0 {
        println!("u64  : {}", bench_u64(N));
    }
    if phase == -1 || phase == 1 {
        println!("u32  : {}", bench_u32(N));
    }
    if phase == -1 || phase == 2 {
        println!("point: {}", bench_point(N));
    }
    if phase == -1 || phase == 3 {
        println!("mixed: {}", bench_mixed(N));
    }
}
