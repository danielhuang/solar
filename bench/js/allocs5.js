// JavaScript (Node.js) port of examples/allocs5.solar.
//
// Phase 1 (allocs3): builds a heap chain of 100M Chain cells that stays live
// for the whole run -- the retained live set. Phase 2 (threads_list2): 16
// workers each build, 1000 times, a fresh 100k-node list and republish it,
// discarding the previous one -- 1.6 billion total churn allocations while
// the chain stays live.
//
// Port caveat (bigger here than in threads_list2): JS worker_threads are
// separate V8 isolates with separate heaps and separate GCs. The retained
// chain lives in the *main* isolate, and the churn happens in 16 *worker*
// isolates -- so no single collector ever traces the 100M-node chain
// concurrently with the churn, which is exactly the combined stress this
// benchmark exists to measure in the shared-heap runtimes. Treat the JS
// column as "allocs3's live set + threads_list2's churn in disjoint heaps",
// not as a mark-throughput-under-churn test.
//
// As in the other ports, main busy-waits for the first worker to finish,
// reads the chain head to keep it live, prints, and exits, abandoning the
// other 15.
"use strict";
const { Worker, isMainThread, workerData } = require("worker_threads");

class Chain {
  constructor(next) { this.next = next; }
}

class Node {
  constructor(value, next) { this.value = value; this.next = next; }
}

if (isMainThread) {
  // Phase 1: the retained chain, live until exit.
  let chain = new Chain(null);
  for (let i = 0; i < 100_000_000; i++) {
    chain = new Chain(chain);
  }

  // Phase 2: concurrent churn while the chain stays live (in this isolate).
  const sab = new SharedArrayBuffer(8);
  const flags = new Int32Array(sab);
  for (let t = 0; t < 16; t++) {
    new Worker(__filename, { workerData: sab });
  }
  while (Atomics.load(flags, 0) === 0) { /* spin */ }
  console.log(Atomics.load(flags, 1));
  if (chain.next !== null) console.log("chain-live");
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
