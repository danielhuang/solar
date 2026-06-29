// C# port of examples/allocs3.solar (and bench/java/Allocs3.java).
//
// Solar source builds a heap chain of 100M `Node` cells, each holding a
// nullable reference `next: &?Node` to the previous one. C# models `&?Node`
// with a plain nullable `Node?` reference -- `null` for the empty `null#[Node]`
// case, a non-null reference otherwise -- so a single nullable `Node?` field
// replaces the nullable ref AND the `&` indirection. That collapses to exactly
// one heap allocation per iteration, matching Solar, whose only per-iteration
// heap object is the `Node { next: prev& }` cell.
//
// Result: 100M allocations, a single live chain rooted at `node`, never freed
// -- a pure allocation-throughput + growing-live-set mark benchmark. The .NET
// GC sees a monotonically growing live set with zero garbage.
//
// GC flavor is selected at run time via DOTNET_gcServer / DOTNET_gcConcurrent
// (the harness sets them per contender); BENCH_GC_TRACE=1 enables the in-process
// STW-pause tracer in GcPause.cs.

GcPause.MaybeStart();

Node node = new Node(null);
for (int i = 0; i < 100_000_000; i++)
{
    node = new Node(node);
}
// Keep the whole chain reachable and defeat dead-code elimination by forcing a
// read of the head after the loop.
long sink = 0;
if (node.next != null) sink++;
Console.WriteLine("head-live=" + (sink == 1 ? "true" : "false"));

internal sealed class Node
{
    public Node? next; // null == empty (null#[Node]), non-null == &?Node to prev
    public Node(Node? next) { this.next = next; }
}
