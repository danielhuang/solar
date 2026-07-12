// JavaScript (Node.js) port of the V8/Dart splay benchmark, matching
// bench/java/Splay.java (which see for the full description). A ~8000-node
// splay tree is continually mutated; each inserted node carries a freshly
// allocated payload object graph, and an equal amount becomes garbage every
// modification -- a high-churn allocation / GC test that also constantly
// rewires a large tree object graph. (Fitting: this benchmark is originally
// V8's own.)
//
// Keys are java.util.Random.nextDouble() doubles, reimplemented here (like
// the C/Go/C# ports) as the 48-bit LCG in exact double arithmetic -- all
// intermediate products stay below 2^53 -- so every port runs bit-identical
// tree operations and prints the same checksum.
"use strict";

const kTreeSize = 8000;
const kTreeModifications = 80;
const kTreePayloadDepth = 5;
const kRuns = 5000; // exercise() iterations per outer run
// Whole-benchmark repetitions: each builds a fresh tree (setup + kRuns
// exercises) with a re-seeded RNG, dropping the previous ~35 MB tree as
// garbage, so every iteration must produce the identical checksum.
const kOuterRuns = 5;

const TWO24 = 16777216;          // 2^24
const TWO53 = 9007199254740992;  // 2^53

// ---- java.util.Random (48-bit LCG), in exact double arithmetic ------------
// seed is held as two 24-bit halves; every product below is < 2^49, exact in
// a double. MULT = 0x5DEECE66D = 0x5DE * 2^24 + 0xECE66D.
const MULT_HI = 0x5de;
const MULT_LO = 0xece66d;
class Random {
  constructor(seed) {
    // (seed ^ 0x5DEECE66D) & (2^48 - 1); seed fits in 32 bits here so the
    // xor only touches the low half's low 32 bits and the constant's rest.
    const s = seed >>> 0;
    this.lo = ((s & 0xffffff) ^ MULT_LO) >>> 0;
    this.hi = (((s >>> 24) & 0xff) ^ MULT_HI) >>> 0;
  }
  next(bits) {
    // seed = (seed * MULT + 0xB) mod 2^48
    const lo = this.lo * MULT_LO + 0xb;
    const mid = this.lo * MULT_HI + this.hi * MULT_LO + Math.floor(lo / TWO24);
    this.lo = lo % TWO24;
    this.hi = mid % TWO24;
    // top `bits` of the 48-bit seed
    return Math.floor((this.hi * TWO24 + this.lo) / 2 ** (48 - bits));
  }
  nextDouble() {
    return (this.next(26) * 134217728 + this.next(27)) / TWO53;
  }
}

// The exact 53-bit integer mantissa of a nextDouble() value (== what Solar
// keys on); key*2^53 is exact because key = mantissa / 2^53, mantissa < 2^53.
function keyMantissa(key) { return key * TWO53; }

// ---- Synthetic payload (allocation pressure) -------------------------------
class Leaf {
  constructor(tag) { this.tag = tag; this.array = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]; }
}
class Payload {
  constructor(left, right, leaf) { this.left = left; this.right = right; this.leaf = leaf; }
}
function generate(depth, tag) {
  if (depth === 0) return new Payload(null, null, new Leaf(keyMantissa(tag)));
  return new Payload(generate(depth - 1, tag), generate(depth - 1, tag), null);
}

// ---- Splay tree -------------------------------------------------------------
class Node {
  constructor(key, value) { this.key = key; this.value = value; this.left = null; this.right = null; }
}
class SplayTree {
  constructor() { this.root = null; }

  isEmpty() { return this.root === null; }

  splay(key) {
    if (this.isEmpty()) return;
    const dummy = new Node(0.0, null);
    let left = dummy, right = dummy, current = this.root;
    while (true) {
      if (key < current.key) {
        if (current.left === null) break;
        if (key < current.left.key) {
          const tmp = current.left; // rotate right
          current.left = tmp.right;
          tmp.right = current;
          current = tmp;
          if (current.left === null) break;
        }
        right.left = current; // link right
        right = current;
        current = current.left;
      } else if (key > current.key) {
        if (current.right === null) break;
        if (key > current.right.key) {
          const tmp = current.right; // rotate left
          current.right = tmp.left;
          tmp.left = current;
          current = tmp;
          if (current.right === null) break;
        }
        left.right = current; // link left
        left = current;
        current = current.right;
      } else {
        break;
      }
    }
    left.right = current.left;
    right.left = current.right;
    current.left = dummy.right;
    current.right = dummy.left;
    this.root = current;
  }

  insert(key, value) {
    if (this.isEmpty()) { this.root = new Node(key, value); return; }
    this.splay(key);
    if (this.root.key === key) return;
    const node = new Node(key, value);
    if (key > this.root.key) {
      node.left = this.root;
      node.right = this.root.right;
      this.root.right = null;
    } else {
      node.right = this.root;
      node.left = this.root.left;
      this.root.left = null;
    }
    this.root = node;
  }

  remove(key) {
    this.splay(key);
    const removed = this.root;
    if (this.root.left === null) {
      this.root = this.root.right;
    } else {
      const right = this.root.right;
      this.root = this.root.left;
      this.splay(key);
      this.root.right = right;
    }
    return removed;
  }

  find(key) {
    if (this.isEmpty()) return null;
    this.splay(key);
    return this.root.key === key ? this.root : null;
  }

  findMax(start) {
    if (this.isEmpty()) return null;
    let current = start === null ? this.root : start;
    while (current.right !== null) current = current.right;
    return current;
  }

  findGreatestLessThan(key) {
    if (this.isEmpty()) return null;
    this.splay(key);
    if (this.root.key < key) return this.root;
    if (this.root.left !== null) return this.findMax(this.root.left);
    return null;
  }
}

// ---- Benchmark driver --------------------------------------------------------
function insertNewNode(tree, rnd) {
  let key = rnd.nextDouble();
  while (tree.find(key) !== null) key = rnd.nextDouble();
  tree.insert(key, generate(kTreePayloadDepth, key));
  return key;
}

function setup(tree, rnd) {
  for (let i = 0; i < kTreeSize; i++) insertNewNode(tree, rnd);
}

function exercise(tree, rnd) {
  for (let i = 0; i < kTreeModifications; i++) {
    const key = insertNewNode(tree, rnd);
    const greatest = tree.findGreatestLessThan(key);
    if (greatest === null) tree.remove(key);
    else tree.remove(greatest.key);
  }
}

// In-order traversal: checksum (Σ mantissa, wrapping mod 2^64), node count,
// sortedness. Mantissas are exact doubles < 2^53; the wrapping sum needs
// BigInt, but only over the 8000 traversal nodes -- never in the hot path.
const MASK64 = (1n << 64n) - 1n;
let acc, count, last, ok;
function traverseCheck(node) {
  let current = node;
  while (current !== null) {
    traverseCheck(current.left);
    if (count > 0 && current.key <= last) ok = false;
    last = current.key;
    acc = (acc + BigInt(keyMantissa(current.key))) & MASK64;
    count++;
    current = current.right;
  }
}

// One full benchmark iteration: fresh RNG + tree, setup, kRuns exercises,
// verify, return the checksum.
function runOnce() {
  const rnd = new Random(12345);
  const tree = new SplayTree();

  setup(tree, rnd);
  for (let i = 0; i < kRuns; i++) exercise(tree, rnd);

  acc = 0n; count = 0; last = 0.0; ok = true;
  traverseCheck(tree.root);
  if (count !== kTreeSize) throw new Error("Splay tree has wrong size");
  if (!ok) throw new Error("Splay tree not sorted");
  return acc;
}

let checksum = 0n;
for (let i = 0; i < kOuterRuns; i++) {
  const a = runOnce();
  if (i === 0) checksum = a;
  else if (a !== checksum) throw new Error("Splay checksum differs between runs");
}
// The wrapping sum as an unsigned 64-bit value (matches C/Go/Java/Solar).
console.log(`Splay done: size=${kTreeSize} checksum=${checksum}`);
