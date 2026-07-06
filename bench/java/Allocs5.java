// Java port of examples/allocs5.solar.
//
// Phase 1 (allocs3): builds a heap chain of 100M Chain cells that stays live
// for the whole run -- the retained live set. Phase 2 (threads_list2): 16
// worker threads each build, 1000 times, a fresh 100k-node singly-linked list
// and publish the head into the shared volatile `root`; the previous list
// becomes garbage immediately. The collector therefore has to trace the large
// retained chain on every cycle while keeping up with the 1.6 billion-node
// churn -- a combined large-live-set + high-garbage-rate test.
//
// As in ThreadsList2, the workers are daemon threads: the first to finish sets
// isDone, main observes it, reads the chain head to keep it live, prints, and
// returns, terminating the other 15.
public final class Allocs5 {
    static final class Chain {
        final Chain next; // null == empty (null#[Chain]), else points to prev
        Chain(Chain next) { this.next = next; }
    }

    static final class Node {
        final long value;   // Solar `Int` is 64-bit
        final Node next;    // null == empty (null#[Node])
        Node(long value, Node next) { this.value = value; this.next = next; }
    }

    static volatile Node root;        // atomic_store / atomic_load target
    static volatile boolean isDone;

    public static void main(String[] args) {
        // Phase 1: the retained chain, live until exit.
        Chain chain = new Chain(null);
        for (int i = 0; i < 100_000_000; i++) {
            chain = new Chain(chain);
        }

        // Phase 2: concurrent churn while the chain stays live.
        final Node sentinel = new Node(0, null);
        root = sentinel;
        for (int t = 0; t < 16; t++) {
            Thread th = new Thread(() -> {
                for (int iter = 0; iter < 1000; iter++) {
                    Node head = sentinel;
                    for (int j = 0; j < 100_000; j++) {
                        head = new Node(j, head);
                    }
                    root = head;      // volatile store
                }
                isDone = true;        // volatile store
            });
            th.setDaemon(true);
            th.start();
        }
        while (!isDone) { /* spin, matching Solar's busy-wait */ }
        System.out.println(root.value);
        if (chain.next != null) System.out.println("chain-live");
        System.out.println("done");
    }
}
