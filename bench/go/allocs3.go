// Go port of examples/allocs3.solar (and bench/java/Allocs3.java, bench/c/allocs3.c).
//
// Builds a heap chain of 100M Node cells, each holding a pointer to the previous
// one, and never drops the head -- matching the Solar source, whose only
// per-iteration heap object is the `Node { next: prev& }` cell and which leaves
// the whole chain live at exit. Go's garbage collector therefore sees a
// monotonically growing live set with zero garbage: a mark-throughput /
// growing-live-set test for a concurrent, non-moving collector.
//
// A Solar node is a single 8-byte `&?Node` cell (~800 MB live); a Go Node is one
// pointer field, also 8 bytes, but Go's allocator rounds the 8-byte object into
// a size class and adds per-object/heap metadata.
package main

import "fmt"

type Node struct {
	next *Node // nil == empty (null#[Node]), else points to prev
}

func main() {
	node := &Node{next: nil}
	for i := 0; i < 100_000_000; i++ {
		node = &Node{next: node}
	}
	// Keep the chain reachable / defeat dead-code elimination by reading the
	// head after the loop, exactly like the Solar/Java/C ports.
	sink := 0
	if node.next != nil {
		sink++
	}
	fmt.Printf("head-live=%v\n", sink == 1)
}
