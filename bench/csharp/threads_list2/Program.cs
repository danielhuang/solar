// C# port of examples/threads_list2.solar (and bench/java/ThreadsList2.java).
//
// 16 worker threads each build, 1000 times, a fresh 100k-node singly-linked
// list hanging off a shared sentinel, then publish the head into the shared
// `root`. The previous list becomes garbage as soon as `root` is overwritten,
// so this is a concurrent allocate-and-discard (high garbage rate) benchmark
// (16 x 1000 x 100k = 1.6 billion Nodes), reclaimed by the .NET GC.
//
// Solar's nullable reference `next: &?Node` (`null#[Node]` when empty) becomes a
// nullable `Node? next`. Solar `atomic_store`/`atomic_load` become Volatile
// reads/writes (reference and bool fields are valid `volatile` targets in C#).
//
// Crucially the Solar process exits the moment `main` returns -- the first
// worker to finish its 1000 iterations sets `is_done`, main observes it, prints,
// and returns, abandoning the other 15 threads mid-flight. To reproduce that
// semantics (and not run 16x the work) the C# workers are background threads, so
// the runtime terminates them when main returns -- the analogue of Java daemon
// threads and Go goroutines.

GcPause.MaybeStart();

ThreadsList2.Run();

internal static class ThreadsList2
{
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
        Console.WriteLine("done");
    }
}
