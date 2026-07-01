// Java port of the V8/Dart splay benchmark (newspeaklanguage/benchmarks
// Splay.java), made self-contained and runnable. A ~8000-node splay tree is
// continually mutated; each inserted node carries a freshly allocated payload
// object graph, and an equal amount becomes garbage every modification -- a
// high-churn allocation / GC test that also constantly rewires a large tree
// object graph. The JVM collector reclaims the discarded nodes and payloads.
//
// Keys are java.util.Random.nextDouble() doubles (the reference RNG the C/Go/C#
// ports reimplement, and whose 53-bit integer mantissa the Solar port keys on),
// so every port runs bit-identical tree operations and prints the same checksum.

import java.util.Random;

public class Splay {
    static final int kTreeSize = 8000;
    static final int kTreeModifications = 80;
    static final int kTreePayloadDepth = 5;
    static final int kRuns = 2000;

    // The exact 53-bit integer mantissa of a nextDouble() value (== what Solar
    // keys on); key*2^53 is exact because key = mantissa / 2^53, mantissa < 2^53.
    static long keyMantissa(double key) { return (long)(key * (double)(1L << 53)); }

    // ---- Synthetic payload (allocation pressure) ----------------------------
    static final class Leaf {
        long tag;
        long[] array;
        Leaf(long tag) { this.tag = tag; this.array = new long[]{0,1,2,3,4,5,6,7,8,9}; }
    }
    static final class Payload {
        Payload left, right;
        Leaf leaf;
        Payload(Payload left, Payload right) { this.left = left; this.right = right; }
        Payload(Leaf leaf) { this.leaf = leaf; }
    }
    static Payload generate(int depth, double tag) {
        if (depth == 0) return new Payload(new Leaf(keyMantissa(tag)));
        return new Payload(generate(depth - 1, tag), generate(depth - 1, tag));
    }

    // ---- Splay tree ---------------------------------------------------------
    static final class Node {
        double key;
        Payload value;
        Node left, right;
        Node(double key, Payload value) { this.key = key; this.value = value; }
    }
    static final class SplayTree {
        Node root;

        boolean isEmpty() { return root == null; }

        void splay(double key) {
            if (isEmpty()) return;
            Node dummy = new Node(0.0, null);
            Node left = dummy, right = dummy, current = root;
            while (true) {
                if (key < current.key) {
                    if (current.left == null) break;
                    if (key < current.left.key) {
                        Node tmp = current.left; // rotate right
                        current.left = tmp.right;
                        tmp.right = current;
                        current = tmp;
                        if (current.left == null) break;
                    }
                    right.left = current; // link right
                    right = current;
                    current = current.left;
                } else if (key > current.key) {
                    if (current.right == null) break;
                    if (key > current.right.key) {
                        Node tmp = current.right; // rotate left
                        current.right = tmp.left;
                        tmp.left = current;
                        current = tmp;
                        if (current.right == null) break;
                    }
                    left.right = current; // link left
                    left = current;
                    current = current.right;
                } else {
                    break;
                }
            }
            left.right = current.left;
            right.left = current.right;
            current.left = dummy.right;
            current.right = dummy.left;
            root = current;
        }

        void insert(double key, Payload value) {
            if (isEmpty()) { root = new Node(key, value); return; }
            splay(key);
            if (root.key == key) return;
            Node node = new Node(key, value);
            if (key > root.key) {
                node.left = root;
                node.right = root.right;
                root.right = null;
            } else {
                node.right = root;
                node.left = root.left;
                root.left = null;
            }
            root = node;
        }

        Node remove(double key) {
            splay(key);
            Node removed = root;
            if (root.left == null) {
                root = root.right;
            } else {
                Node right = root.right;
                root = root.left;
                splay(key);
                root.right = right;
            }
            return removed;
        }

        Node find(double key) {
            if (isEmpty()) return null;
            splay(key);
            return root.key == key ? root : null;
        }

        Node findMax(Node start) {
            if (isEmpty()) return null;
            Node current = start == null ? root : start;
            while (current.right != null) current = current.right;
            return current;
        }

        Node findGreatestLessThan(double key) {
            if (isEmpty()) return null;
            splay(key);
            if (root.key < key) return root;
            if (root.left != null) return findMax(root.left);
            return null;
        }
    }

    // ---- Benchmark driver ---------------------------------------------------
    static double insertNewNode(SplayTree tree, Random rnd) {
        double key = rnd.nextDouble();
        while (tree.find(key) != null) key = rnd.nextDouble();
        tree.insert(key, generate(kTreePayloadDepth, key));
        return key;
    }

    static void setup(SplayTree tree, Random rnd) {
        for (int i = 0; i < kTreeSize; i++) insertNewNode(tree, rnd);
    }

    static void exercise(SplayTree tree, Random rnd) {
        for (int i = 0; i < kTreeModifications; i++) {
            double key = insertNewNode(tree, rnd);
            Node greatest = tree.findGreatestLessThan(key);
            if (greatest == null) tree.remove(key);
            else tree.remove(greatest.key);
        }
    }

    // In-order traversal: checksum (Σ mantissa, wrapping), node count, sortedness.
    static long acc;
    static int count;
    static double last;
    static boolean ok = true;
    static void traverseCheck(Node node) {
        Node current = node;
        while (current != null) {
            traverseCheck(current.left);
            if (count > 0 && current.key <= last) ok = false;
            last = current.key;
            acc += keyMantissa(current.key);
            count++;
            current = current.right;
        }
    }

    public static void main(String[] args) {
        Random rnd = new Random(12345);
        SplayTree tree = new SplayTree();

        setup(tree, rnd);
        for (int i = 0; i < kRuns; i++) exercise(tree, rnd);

        traverseCheck(tree.root);
        if (count != kTreeSize) throw new RuntimeException("Splay tree has wrong size");
        if (!ok) throw new RuntimeException("Splay tree not sorted");
        // Print the wrapping sum as an unsigned 64-bit value (matches C/Go/Solar).
        System.out.println("Splay done: size=" + count
                + " checksum=" + Long.toUnsignedString(acc));
    }
}
