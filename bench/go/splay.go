// Go port of examples/splay.solar (and bench/java/Splay.java, bench/c/splay.c),
// the V8/Dart splay benchmark. A ~8000-node splay tree is continually mutated;
// each inserted node carries a freshly allocated payload object graph, and an
// equal amount becomes garbage every modification -- a high-churn allocation /
// GC test that also constantly rewires a large tree object graph.
//
// Go's garbage collector reclaims the discarded nodes and payloads; nothing is
// freed manually. Keys are java.util.Random.nextDouble() doubles, exactly as in
// the Java reference (the Solar port keeps the equivalent 53-bit integer
// mantissa, which orders identically), so every port runs bit-identical tree
// operations and prints the same checksum.
package main

import "fmt"

const (
	kTreeSize          = 8000
	kTreeModifications = 80
	kTreePayloadDepth  = 5
	kRuns              = 5000 // exercise() iterations per outer run
	// Whole-benchmark repetitions: each builds a fresh tree (setup + kRuns
	// exercises) with a re-seeded RNG, dropping the previous ~35 MB tree as
	// garbage, so every iteration must produce the identical checksum.
	kOuterRuns = 5
)

// ---- java.util.Random: 48-bit LCG, producing nextDouble() -------------------
type Random struct{ seed uint64 }

func newRandom(seed int64) *Random {
	return &Random{seed: (uint64(seed) ^ 0x5DEECE66D) & ((1 << 48) - 1)}
}
func (r *Random) next(bits uint) uint64 {
	r.seed = (r.seed*0x5DEECE66D + 0xB) & ((1 << 48) - 1)
	return r.seed >> (48 - bits)
}
func (r *Random) nextDouble() float64 {
	hi := r.next(26)
	lo := r.next(27)
	return float64((hi<<27)+lo) / float64(uint64(1)<<53)
}

// keyMantissa recovers the exact 53-bit integer mantissa of a nextDouble()
// value (== what Solar keys on); key*2^53 is exact since key = mantissa / 2^53.
func keyMantissa(key float64) int64 { return int64(key * float64(uint64(1)<<53)) }

// ---- Synthetic payload (allocation pressure) --------------------------------
type Leaf struct {
	tag   int64
	array []int64
}
type Payload struct {
	left, right *Payload
	leaf        *Leaf
}

func generate(depth int, tag float64) *Payload {
	if depth == 0 {
		arr := make([]int64, 10)
		for i := range arr {
			arr[i] = int64(i)
		}
		return &Payload{leaf: &Leaf{tag: keyMantissa(tag), array: arr}}
	}
	return &Payload{left: generate(depth-1, tag), right: generate(depth-1, tag)}
}

// ---- Splay tree -------------------------------------------------------------
type Node struct {
	key         float64
	value       *Payload
	left, right *Node
}
type SplayTree struct{ root *Node }

func (t *SplayTree) splay(key float64) {
	if t.root == nil {
		return
	}
	var dummy Node // stack dummy; never escapes
	left, right, current := &dummy, &dummy, t.root
	for {
		if key < current.key {
			if current.left == nil {
				break
			}
			if key < current.left.key {
				tmp := current.left // rotate right
				current.left = tmp.right
				tmp.right = current
				current = tmp
				if current.left == nil {
					break
				}
			}
			right.left = current // link right
			right = current
			current = current.left
		} else if key > current.key {
			if current.right == nil {
				break
			}
			if key > current.right.key {
				tmp := current.right // rotate left
				current.right = tmp.left
				tmp.left = current
				current = tmp
				if current.right == nil {
					break
				}
			}
			left.right = current // link left
			left = current
			current = current.right
		} else {
			break
		}
	}
	left.right = current.left
	right.left = current.right
	current.left = dummy.right
	current.right = dummy.left
	t.root = current
}

func (t *SplayTree) insert(key float64, value *Payload) {
	if t.root == nil {
		t.root = &Node{key: key, value: value}
		return
	}
	t.splay(key)
	if t.root.key == key {
		return
	}
	node := &Node{key: key, value: value}
	if key > t.root.key {
		node.left = t.root
		node.right = t.root.right
		t.root.right = nil
	} else {
		node.right = t.root
		node.left = t.root.left
		t.root.left = nil
	}
	t.root = node
}

func (t *SplayTree) remove(key float64) *Node {
	t.splay(key)
	removed := t.root
	if t.root.left == nil {
		t.root = t.root.right
	} else {
		right := t.root.right
		t.root = t.root.left
		t.splay(key)
		t.root.right = right
	}
	return removed
}

func (t *SplayTree) find(key float64) *Node {
	if t.root == nil {
		return nil
	}
	t.splay(key)
	if t.root.key == key {
		return t.root
	}
	return nil
}

func (t *SplayTree) findMax(start *Node) *Node {
	if t.root == nil {
		return nil
	}
	current := start
	if current == nil {
		current = t.root
	}
	for current.right != nil {
		current = current.right
	}
	return current
}

func (t *SplayTree) findGreatestLessThan(key float64) *Node {
	if t.root == nil {
		return nil
	}
	t.splay(key)
	if t.root.key < key {
		return t.root
	}
	if t.root.left != nil {
		return t.findMax(t.root.left)
	}
	return nil
}

// ---- Benchmark driver -------------------------------------------------------
func insertNewNode(t *SplayTree, r *Random) float64 {
	key := r.nextDouble()
	for t.find(key) != nil {
		key = r.nextDouble()
	}
	t.insert(key, generate(kTreePayloadDepth, key))
	return key
}

func setup(t *SplayTree, r *Random) {
	for i := 0; i < kTreeSize; i++ {
		insertNewNode(t, r)
	}
}

func exercise(t *SplayTree, r *Random) {
	for i := 0; i < kTreeModifications; i++ {
		key := insertNewNode(t, r)
		greatest := t.findGreatestLessThan(key)
		if greatest == nil {
			t.remove(key)
		} else {
			t.remove(greatest.key)
		}
	}
}

// In-order traversal: checksum (Σ mantissa, wrapping), node count, sortedness.
func traverseCheck(node *Node, acc *uint64, count *int, last *float64, ok *bool) {
	for current := node; current != nil; current = current.right {
		traverseCheck(current.left, acc, count, last, ok)
		if *count > 0 && current.key <= *last {
			*ok = false
		}
		*last = current.key
		*acc += uint64(keyMantissa(current.key))
		*count++
	}
}

// runOnce is one full benchmark iteration: fresh RNG + tree, setup, kRuns
// exercises, verify, return the checksum.
func runOnce() uint64 {
	r := newRandom(12345)
	tree := &SplayTree{}

	setup(tree, r)
	for i := 0; i < kRuns; i++ {
		exercise(tree, r)
	}

	var acc uint64
	count := 0
	last := 0.0
	ok := true
	traverseCheck(tree.root, &acc, &count, &last, &ok)
	if count != kTreeSize {
		panic("Splay tree has wrong size")
	}
	if !ok {
		panic("Splay tree not sorted")
	}
	return acc
}

func main() {
	var checksum uint64
	for i := 0; i < kOuterRuns; i++ {
		acc := runOnce()
		if i == 0 {
			checksum = acc
		} else if acc != checksum {
			panic("Splay checksum differs between runs")
		}
	}
	fmt.Printf("Splay done: size=%d checksum=%d\n", kTreeSize, checksum)
}
