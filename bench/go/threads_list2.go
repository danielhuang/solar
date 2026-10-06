// Go port of examples/threads_list2.solar (and the Java/C ports).
//
// One goroutine per available CPU builds, 1000 times, a fresh 100k-node list
// hanging off a shared sentinel, then publish the head into the shared atomic
// `root` (matching Solar's `atomic_store`). The previous list becomes garbage as
// soon as `root` is overwritten -- Go's concurrent GC reclaims it, so unlike the
// C port there is no manual free; this is the GC-managed analogue, identical in
// shape to the Solar and Java versions (1000 x 100k Nodes per worker).
//
// Solar's nullable reference `next: &?Node` becomes a nullable `*Node`. Solar's
// `atomic_store`/`atomic_load` become atomic.Pointer / atomic.Bool. As in the
// Solar/Java/C ports the first worker to finish sets isDone; main observes it,
// prints, and returns -- and a Go program exits when main returns, abandoning
// the remaining goroutines mid-flight, matching Solar.
package main

import (
	"fmt"
	"runtime"
	"sync/atomic"
)

type Node struct {
	value int64 // Solar `Int` is 64-bit
	next  *Node // nil == empty (null#[Node])
}

var (
	root   atomic.Pointer[Node] // atomic_store / atomic_load target
	isDone atomic.Bool
)

func main() {
	sentinel := &Node{value: 0, next: nil}
	root.Store(sentinel)
	for t := 0; t < runtime.NumCPU(); t++ {
		go func() {
			for iter := 0; iter < 1000; iter++ {
				head := sentinel
				for j := int64(0); j < 100_000; j++ {
					head = &Node{value: j, next: head}
				}
				root.Store(head) // atomic store; previous list now garbage
			}
			isDone.Store(true)
		}()
	}
	for !isDone.Load() { // spin, matching Solar's busy-wait
	}
	fmt.Println(root.Load().value)
	fmt.Println("done")
}
