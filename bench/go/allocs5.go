// Go port of examples/allocs5.solar (and the Java/C/C# ports).
//
// Phase 1 (allocs3): builds a heap chain of 100M Chain cells that stays live
// for the whole run -- the retained live set. Phase 2 (threads_list2): 16
// goroutines each build, 1000 times, a fresh 100k-node singly-linked list and
// publish the head into the shared atomic `root`; the previous list becomes
// garbage immediately. Go's concurrent GC therefore has to re-mark the ~800 MB
// retained chain on every cycle while reclaiming the 1.6 billion-node churn --
// a combined large-live-set + high-garbage-rate test.
//
// As in the other ports, the first worker to finish sets isDone; main observes
// it, reads the chain head to keep it live, prints, and returns.
package main

import (
	"fmt"
	"sync/atomic"
)

type Chain struct {
	next *Chain // nil == empty (null#[Chain]), else points to prev
}

type Node struct {
	value int64 // Solar `Int` is 64-bit
	next  *Node // nil == empty (null#[Node])
}

var (
	root   atomic.Pointer[Node] // atomic_store / atomic_load target
	isDone atomic.Bool
)

func main() {
	// Phase 1: the retained chain, live until exit.
	chain := &Chain{next: nil}
	for i := 0; i < 100_000_000; i++ {
		chain = &Chain{next: chain}
	}

	// Phase 2: concurrent churn while the chain stays live.
	sentinel := &Node{value: 0, next: nil}
	root.Store(sentinel)
	for t := 0; t < 16; t++ {
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
	if chain.next != nil {
		fmt.Println("chain-live")
	}
	fmt.Println("done")
}
