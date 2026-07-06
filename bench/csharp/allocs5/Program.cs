// C# port of examples/allocs5.solar (and bench/java/Allocs5.java).
//
// Phase 1 (allocs3): builds a heap chain of 100M Chain cells that stays live
// for the whole run -- the retained live set. Phase 2 (threads_list2): 16
// worker threads each build, 1000 times, a fresh 100k-node singly-linked list
// and publish the head into the shared volatile `root`; the previous list
// becomes garbage immediately. The .NET GC therefore has to trace the large
// retained chain (promoted to gen2) while keeping up with the 1.6 billion-node
// ephemeral churn -- a combined large-live-set + high-garbage-rate test.
//
// As in ThreadsList2, the workers are background threads: the first to finish
// sets isDone, main observes it, reads the chain head to keep it live, prints,
// and returns, terminating the other 15.

GcPause.MaybeStart();

Allocs5.Run();

internal static class Allocs5
{
    internal sealed class Chain
    {
        public readonly Chain? next; // null == empty (null#[Chain]), else prev
        public Chain(Chain? next) { this.next = next; }
    }

    internal sealed class Node
    {
        public readonly long value;   // Solar `Int` is 64-bit
        public readonly Node? next;   // null == empty (null#[Node])
        public Node(long value, Node? next) { this.value = value; this.next = next; }
    }

    private static volatile Node? root;     // atomic_store / atomic_load target
    private static volatile bool isDone;

    public static void Run()
    {
        // Phase 1: the retained chain, live until exit.
        Chain chain = new Chain(null);
        for (int i = 0; i < 100_000_000; i++)
        {
            chain = new Chain(chain);
        }

        // Phase 2: concurrent churn while the chain stays live.
        Node sentinel = new Node(0, null);
        root = sentinel;
        for (int t = 0; t < 16; t++)
        {
            Thread th = new Thread(() =>
            {
                for (int iter = 0; iter < 1000; iter++)
                {
                    Node head = sentinel;
                    for (int j = 0; j < 100_000; j++)
                    {
                        head = new Node(j, head);
                    }
                    root = head;      // volatile store
                }
                isDone = true;        // volatile store
            });
            th.IsBackground = true;
            th.Start();
        }
        while (!isDone) { /* spin, matching Solar's busy-wait */ }
        Console.WriteLine(root!.value);
        if (chain.next != null) Console.WriteLine("chain-live");
        Console.WriteLine("done");
    }
}
