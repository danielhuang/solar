// Java port of examples/allocs3.solar.
//
// Solar source builds a heap chain of 100M `Node` cells, each holding a
// nullable reference `next: &?Node` to the previous one. Java models `&?Node`
// with a plain `Node` reference -- `null` for the empty `null#[Node]` case, a
// non-null reference otherwise -- so a single nullable `Node` field replaces the
// nullable ref AND the `&` indirection. That collapses to exactly one heap
// allocation per iteration -- which matches Solar, whose only per-iteration heap
// object is the `Node { next: prev& }` cell.
//
// Result: 100M allocations, a single ~1.6GB live chain rooted at `node`, never
// freed -- a pure allocation-throughput + growing-live-set mark benchmark.
public final class Allocs3 {
    static final class Node {
        Node next; // null == empty (null#[Node]), non-null == &?Node to prev
        Node(Node next) { this.next = next; }
    }

    public static void main(String[] args) {
        Node node = new Node(null);
        for (int i = 0; i < 100_000_000; i++) {
            node = new Node(node);
        }
        // Keep the whole chain reachable and defeat dead-code elimination by
        // forcing a read of the head after the loop.
        long sink = 0;
        if (node.next != null) sink++;
        System.out.println("head-live=" + (sink == 1));
    }
}
