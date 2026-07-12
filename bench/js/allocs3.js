// JavaScript (Node.js) port of examples/allocs3.solar.
//
// Solar builds a heap chain of 100M `Node` cells, each holding a nullable
// reference `next: &?Node` to the previous one. JS models `&?Node` with a
// plain object reference -- `null` for the empty `null#[Node]` case -- so a
// single nullable field replaces the nullable ref AND the `&` indirection,
// exactly one heap allocation per iteration (matching the Java/C#/Go ports).
//
// Result: 100M allocations, a single live chain rooted at `node`, never
// freed -- a pure allocation-throughput + growing-live-set mark benchmark.
// Run with --max-old-space-size=8192 (the harness does), the JVM -Xmx8g
// equivalent.
"use strict";

class Node {
  constructor(next) { this.next = next; }
}

let node = new Node(null);
for (let i = 0; i < 100_000_000; i++) {
  node = new Node(node);
}
// Keep the whole chain reachable and defeat dead-code elimination by forcing
// a read of the head after the loop.
let sink = 0;
if (node.next !== null) sink++;
console.log("head-live=" + (sink === 1));
