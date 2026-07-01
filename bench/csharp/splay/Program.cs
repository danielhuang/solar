// C# port of examples/splay.solar (and bench/java/Splay.java, bench/c/splay.c,
// bench/go/splay.go), the V8/Dart splay benchmark. A ~8000-node splay tree is
// continually mutated; each inserted node carries a freshly allocated payload
// object graph, and an equal amount becomes garbage every modification -- a
// high-churn allocation / GC test that also constantly rewires a large tree
// object graph. The .NET collector reclaims the discarded nodes and payloads.
//
// Keys are java.util.Random.nextDouble() doubles (reimplemented below, matching
// the Java reference; the Solar port keys on the equivalent 53-bit integer
// mantissa), so every port runs bit-identical tree operations and prints the
// same checksum.
//
// GC flavor is selected at run time via DOTNET_gcServer / DOTNET_gcConcurrent
// (the harness sets them per contender); BENCH_GC_TRACE=1 enables the in-process
// STW-pause tracer in GcPause.cs.

GcPause.MaybeStart();

const int kTreeSize = 8000;
const int kTreeModifications = 80;
const int kTreePayloadDepth = 5;
const int kRuns = 2000;

var rnd = new JavaRandom(12345);
var tree = new SplayTree();

Splay.Setup(tree, rnd, kTreeSize, kTreePayloadDepth);
for (int i = 0; i < kRuns; i++)
    Splay.Exercise(tree, rnd, kTreeModifications, kTreePayloadDepth);

ulong acc = 0;
int count = 0;
double last = 0.0;
bool ok = true;
Splay.TraverseCheck(tree.Root, ref acc, ref count, ref last, ref ok);
if (count != kTreeSize) throw new Exception("Splay tree has wrong size");
if (!ok) throw new Exception("Splay tree not sorted");
Console.WriteLine($"Splay done: size={count} checksum={acc}");

// ---- java.util.Random: 48-bit LCG, producing nextDouble() -------------------
internal sealed class JavaRandom
{
    private ulong _seed;
    public JavaRandom(long seed) { _seed = ((ulong)seed ^ 0x5DEECE66DUL) & ((1UL << 48) - 1); }
    private ulong Next(int bits)
    {
        _seed = (_seed * 0x5DEECE66DUL + 0xBUL) & ((1UL << 48) - 1);
        return _seed >> (48 - bits);
    }
    public double NextDouble()
    {
        ulong hi = Next(26), lo = Next(27);
        return (double)((hi << 27) + lo) / (double)(1UL << 53);
    }
    // Exact 53-bit integer mantissa of a nextDouble() value (== what Solar keys
    // on); key*2^53 is exact because key = mantissa / 2^53, mantissa < 2^53.
    public static long Mantissa(double key) => (long)(key * (double)(1UL << 53));
}

// ---- Synthetic payload (allocation pressure) --------------------------------
internal sealed class Leaf
{
    public long Tag;
    public long[] Array;
    public Leaf(long tag) { Tag = tag; Array = new long[] { 0, 1, 2, 3, 4, 5, 6, 7, 8, 9 }; }
}
internal sealed class Payload
{
    public Payload? Left, Right;
    public Leaf? Leaf;
    public Payload(Payload left, Payload right) { Left = left; Right = right; }
    public Payload(Leaf leaf) { Leaf = leaf; }
}

// ---- Splay tree -------------------------------------------------------------
internal sealed class Node
{
    public double Key;
    public Payload? Value;
    public Node? Left, Right;
    public Node(double key, Payload? value) { Key = key; Value = value; }
}

internal sealed class SplayTree
{
    public Node? Root;

    public bool IsEmpty => Root == null;

    public void SplayOp(double key)
    {
        if (Root == null) return;
        var dummy = new Node(0.0, null);
        Node left = dummy, right = dummy, current = Root;
        while (true)
        {
            if (key < current.Key)
            {
                if (current.Left == null) break;
                if (key < current.Left.Key)
                {
                    var tmp = current.Left; // rotate right
                    current.Left = tmp.Right;
                    tmp.Right = current;
                    current = tmp;
                    if (current.Left == null) break;
                }
                right.Left = current; // link right
                right = current;
                current = current.Left;
            }
            else if (key > current.Key)
            {
                if (current.Right == null) break;
                if (key > current.Right.Key)
                {
                    var tmp = current.Right; // rotate left
                    current.Right = tmp.Left;
                    tmp.Left = current;
                    current = tmp;
                    if (current.Right == null) break;
                }
                left.Right = current; // link left
                left = current;
                current = current.Right;
            }
            else break;
        }
        left.Right = current.Left;
        right.Left = current.Right;
        current.Left = dummy.Right;
        current.Right = dummy.Left;
        Root = current;
    }

    public void Insert(double key, Payload value)
    {
        if (Root == null) { Root = new Node(key, value); return; }
        SplayOp(key);
        if (Root.Key == key) return;
        var node = new Node(key, value);
        if (key > Root.Key)
        {
            node.Left = Root;
            node.Right = Root.Right;
            Root.Right = null;
        }
        else
        {
            node.Right = Root;
            node.Left = Root.Left;
            Root.Left = null;
        }
        Root = node;
    }

    public Node Remove(double key)
    {
        SplayOp(key);
        var removed = Root!;
        if (Root!.Left == null)
        {
            Root = Root.Right;
        }
        else
        {
            var right = Root.Right;
            Root = Root.Left;
            SplayOp(key);
            Root!.Right = right;
        }
        return removed;
    }

    public Node? Find(double key)
    {
        if (Root == null) return null;
        SplayOp(key);
        return Root!.Key == key ? Root : null;
    }

    public Node? FindMax(Node? start)
    {
        if (Root == null) return null;
        var current = start ?? Root;
        while (current.Right != null) current = current.Right;
        return current;
    }

    public Node? FindGreatestLessThan(double key)
    {
        if (Root == null) return null;
        SplayOp(key);
        if (Root!.Key < key) return Root;
        if (Root.Left != null) return FindMax(Root.Left);
        return null;
    }
}

internal static class Splay
{
    static double InsertNewNode(SplayTree tree, JavaRandom rnd, int depth)
    {
        double key = rnd.NextDouble();
        while (tree.Find(key) != null) key = rnd.NextDouble();
        tree.Insert(key, Generate(depth, key));
        return key;
    }

    static Payload Generate(int depth, double tag)
    {
        if (depth == 0) return new Payload(new Leaf(JavaRandom.Mantissa(tag)));
        return new Payload(Generate(depth - 1, tag), Generate(depth - 1, tag));
    }

    public static void Setup(SplayTree tree, JavaRandom rnd, int treeSize, int depth)
    {
        for (int i = 0; i < treeSize; i++) InsertNewNode(tree, rnd, depth);
    }

    public static void Exercise(SplayTree tree, JavaRandom rnd, int mods, int depth)
    {
        for (int i = 0; i < mods; i++)
        {
            double key = InsertNewNode(tree, rnd, depth);
            var greatest = tree.FindGreatestLessThan(key);
            if (greatest == null) tree.Remove(key);
            else tree.Remove(greatest.Key);
        }
    }

    // In-order traversal: checksum (Σ mantissa, wrapping), node count, sortedness.
    public static void TraverseCheck(Node? node, ref ulong acc, ref int count,
                                     ref double last, ref bool ok)
    {
        var current = node;
        while (current != null)
        {
            TraverseCheck(current.Left, ref acc, ref count, ref last, ref ok);
            if (count > 0 && current.Key <= last) ok = false;
            last = current.Key;
            acc += (ulong)JavaRandom.Mantissa(current.Key);
            count++;
            current = current.Right;
        }
    }
}
