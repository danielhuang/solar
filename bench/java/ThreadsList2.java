// Java port of examples/threads_list2.solar.
//
// 16 worker threads each build, 1000 times, a fresh 100k-node singly-linked
// list hanging off a shared sentinel, then publish the head into the shared
// `root`. The previous list becomes garbage as soon as `root` is overwritten,
// so this is a concurrent allocate-and-discard (high garbage rate) benchmark.
//
// Solar's nullable reference `next: &?Node` (`null#[Node]` when empty) becomes a nullable `Node next`.
// Solar `atomic_store`/`atomic_load` become `volatile` field accesses.
//
// Crucially the Solar process exits the moment `main` returns -- the first
// worker to finish its 1000 iterations sets `is_done`, main observes it,
// prints, and returns, abandoning the other 15 threads mid-flight. To
// reproduce that semantics (and not run 16x the work) the Java workers are
// daemon threads, so the JVM terminates them when main returns.
public final class ThreadsList2 {
    static final class Node {
        final long value;   // Solar `Int` is 64-bit
        final Node next;    // null == empty (null#[Node])
        Node(long value, Node next) { this.value = value; this.next = next; }
    }

    static volatile Node root;        // atomic_store / atomic_load target
    static volatile boolean isDone;

    public static void main(String[] args) {
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
        System.out.println("done");
    }
}
