// JavaScript (Node.js) port of examples/threads_list2.solar.
//
// 16 workers each build, 1000 times, a fresh 100k-node singly-linked list
// hanging off a sentinel, then publish the head into `root`; the previous
// list becomes garbage as soon as `root` is overwritten -- a concurrent
// allocate-and-discard (high garbage rate) benchmark.
//
// Port caveat: JS has no shared-heap threads. Each worker_threads Worker is
// its own V8 *isolate* with its own heap and its own GC, and object
// references cannot cross isolates -- so the shared `root` becomes a
// per-worker module-level `root` (same allocation and garbage timing, no
// cross-thread visibility), and Solar's `is_done` atomic becomes a
// SharedArrayBuffer flag ([0] = done, [1] = the finisher's root.value).
// This means 16 independent collectors each handle 1/16th of the churn
// instead of one collector handling all of it -- the closest possible port,
// but note it sidesteps the multi-threaded-collector stress the other
// runtimes face.
//
// Like the other ports, the process exits the moment the first worker
// finishes (main busy-waits on the flag, prints, and exits, abandoning the
// other 15 mid-flight).
"use strict";
const { Worker, isMainThread, workerData } = require("worker_threads");

class Node {
  constructor(value, next) { this.value = value; this.next = next; }
}

if (isMainThread) {
  const sab = new SharedArrayBuffer(8);
  const flags = new Int32Array(sab);
  for (let t = 0; t < 16; t++) {
    new Worker(__filename, { workerData: sab });
  }
  // Spin, matching Solar's busy-wait (workers are real OS threads with their
  // own event loops, so they run while the main thread blocks).
  while (Atomics.load(flags, 0) === 0) { /* spin */ }
  console.log(Atomics.load(flags, 1));
  console.log("done");
  process.exit(0); // abandon the other 15 workers
} else {
  const flags = new Int32Array(workerData);
  const sentinel = new Node(0, null);
  let root = sentinel; // per-isolate stand-in for the shared root
  for (let iter = 0; iter < 1000; iter++) {
    let head = sentinel;
    for (let j = 0; j < 100_000; j++) {
      head = new Node(j, head);
    }
    root = head; // previous list becomes garbage here
  }
  Atomics.store(flags, 1, root.value);
  Atomics.store(flags, 0, 1);
}
