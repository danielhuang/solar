// C port of examples/splay.solar (and bench/java/Splay.java), the V8/Dart splay
// benchmark. A ~8000-node splay tree is continually mutated; each inserted node
// carries a freshly allocated payload object graph, and an equal amount becomes
// garbage every modification. It stresses allocation and the constant rewiring
// of a large tree object graph.
//
// C has no collector, so this port reclaims manually: every node removed from
// the tree has its node object and its payload graph free()d immediately (the
// reclamation Solar/Java/Go/C# do via GC), and the whole remaining tree is freed
// at the end. Keys are java.util.Random.nextDouble() doubles, exactly as in the
// Java reference; the Solar port keeps the equivalent 53-bit integer mantissa
// (Solar has no float literals), which orders identically, so all ports run
// bit-identical tree operations and print the same checksum.

#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>

enum { kTreeSize = 8000, kTreeModifications = 80, kTreePayloadDepth = 5 };
#define kRuns 2000

// ---- java.util.Random: 48-bit LCG, producing nextDouble() -------------------
typedef struct { uint64_t seed; } Random;

static void rnd_init(Random *r, int64_t seed) {
    r->seed = ((uint64_t)seed ^ 0x5DEECE66DULL) & ((1ULL << 48) - 1);
}
static uint64_t rnd_next(Random *r, int bits) {
    r->seed = (r->seed * 0x5DEECE66DULL + 0xBULL) & ((1ULL << 48) - 1);
    return r->seed >> (48 - bits);
}
static double rnd_double(Random *r) {
    uint64_t hi = rnd_next(r, 26);
    uint64_t lo = rnd_next(r, 27);
    return (double)((hi << 27) + lo) / (double)(1ULL << 53);
}
// The exact 53-bit integer mantissa of a nextDouble() value (== what Solar keys
// on). key * 2^53 is exact because key = mantissa / 2^53, mantissa < 2^53.
static int64_t key_mantissa(double key) {
    return (int64_t)(key * (double)(1ULL << 53));
}

// ---- Synthetic payload (allocation pressure) --------------------------------
typedef struct Leaf { int64_t tag; int64_t *array; } Leaf;
typedef struct Payload { struct Payload *left, *right; Leaf *leaf; } Payload;

static Payload *generate(int depth, double tag) {
    Payload *p = (Payload *)malloc(sizeof(Payload));
    if (depth == 0) {
        Leaf *lf = (Leaf *)malloc(sizeof(Leaf));
        lf->tag = key_mantissa(tag);
        lf->array = (int64_t *)malloc(10 * sizeof(int64_t));
        for (int i = 0; i < 10; i++) lf->array[i] = i;
        p->left = NULL; p->right = NULL; p->leaf = lf;
    } else {
        p->left = generate(depth - 1, tag);
        p->right = generate(depth - 1, tag);
        p->leaf = NULL;
    }
    return p;
}
static void free_payload(Payload *p) {
    if (!p) return;
    if (p->leaf) { free(p->leaf->array); free(p->leaf); }
    else { free_payload(p->left); free_payload(p->right); }
    free(p);
}

// ---- Splay tree -------------------------------------------------------------
typedef struct Node { double key; Payload *value; struct Node *left, *right; } Node;
typedef struct { Node *root; } SplayTree;

static Node *new_node(double key, Payload *value) {
    Node *n = (Node *)malloc(sizeof(Node));
    n->key = key; n->value = value; n->left = NULL; n->right = NULL;
    return n;
}

static void splay(SplayTree *t, double key) {
    if (t->root == NULL) return;
    Node dummy = {0.0, NULL, NULL, NULL}; // stack dummy; never escapes
    Node *left = &dummy, *right = &dummy, *current = t->root;
    for (;;) {
        if (key < current->key) {
            if (current->left == NULL) break;
            if (key < current->left->key) {
                Node *tmp = current->left; // rotate right
                current->left = tmp->right;
                tmp->right = current;
                current = tmp;
                if (current->left == NULL) break;
            }
            right->left = current; // link right
            right = current;
            current = current->left;
        } else if (key > current->key) {
            if (current->right == NULL) break;
            if (key > current->right->key) {
                Node *tmp = current->right; // rotate left
                current->right = tmp->left;
                tmp->left = current;
                current = tmp;
                if (current->right == NULL) break;
            }
            left->right = current; // link left
            left = current;
            current = current->right;
        } else {
            break;
        }
    }
    left->right = current->left;
    right->left = current->right;
    current->left = dummy.right;
    current->right = dummy.left;
    t->root = current;
}

static void insert(SplayTree *t, double key, Payload *value) {
    if (t->root == NULL) { t->root = new_node(key, value); return; }
    splay(t, key);
    if (t->root->key == key) { free_payload(value); return; } // key present (won't happen)
    Node *node = new_node(key, value);
    if (key > t->root->key) {
        node->left = t->root;
        node->right = t->root->right;
        t->root->right = NULL;
    } else {
        node->right = t->root;
        node->left = t->root->left;
        t->root->left = NULL;
    }
    t->root = node;
}

// Remove the node with `key` (assumed present) and return it for the caller to
// free (node object + payload graph).
static Node *removeNode(SplayTree *t, double key) {
    splay(t, key);
    Node *removed = t->root;
    if (t->root->left == NULL) {
        t->root = t->root->right;
    } else {
        Node *right = t->root->right;
        t->root = t->root->left;
        splay(t, key);
        t->root->right = right;
    }
    return removed;
}

static Node *find(SplayTree *t, double key) {
    if (t->root == NULL) return NULL;
    splay(t, key);
    return t->root->key == key ? t->root : NULL;
}

static Node *find_max(SplayTree *t, Node *start) {
    if (t->root == NULL) return NULL;
    Node *current = (start == NULL) ? t->root : start;
    while (current->right != NULL) current = current->right;
    return current;
}

static Node *find_greatest_less_than(SplayTree *t, double key) {
    if (t->root == NULL) return NULL;
    splay(t, key);
    if (t->root->key < key) return t->root;
    if (t->root->left != NULL) return find_max(t, t->root->left);
    return NULL;
}

// ---- Benchmark driver -------------------------------------------------------
static double insert_new_node(SplayTree *t, Random *r) {
    double key = rnd_double(r);
    while (find(t, key) != NULL) key = rnd_double(r);
    Payload *payload = generate(kTreePayloadDepth, key);
    insert(t, key, payload);
    return key;
}

static void setup(SplayTree *t, Random *r) {
    for (int i = 0; i < kTreeSize; i++) insert_new_node(t, r);
}

static void exercise(SplayTree *t, Random *r) {
    for (int i = 0; i < kTreeModifications; i++) {
        double key = insert_new_node(t, r);
        Node *greatest = find_greatest_less_than(t, key);
        Node *removed = (greatest == NULL) ? removeNode(t, key)
                                           : removeNode(t, greatest->key);
        free_payload(removed->value); // manual reclamation (no GC in C)
        free(removed);
    }
}

// In-order traversal: checksum (Σ mantissa, wrapping), node count, sortedness.
static void traverse_check(Node *node, uint64_t *acc, int *count,
                           double *last, int *ok) {
    Node *current = node;
    while (current != NULL) {
        traverse_check(current->left, acc, count, last, ok);
        if (*count > 0 && current->key <= *last) *ok = 0;
        *last = current->key;
        *acc += (uint64_t)key_mantissa(current->key);
        (*count)++;
        current = current->right;
    }
}

static void free_tree(Node *node) {
    if (!node) return;
    free_tree(node->left);
    free_tree(node->right);
    free_payload(node->value);
    free(node);
}

int main(void) {
    Random r;
    rnd_init(&r, 12345);
    SplayTree tree = {NULL};

    setup(&tree, &r);
    for (int i = 0; i < kRuns; i++) exercise(&tree, &r);

    uint64_t acc = 0;
    int count = 0, ok = 1;
    double last = 0.0;
    traverse_check(tree.root, &acc, &count, &last, &ok);
    if (count != kTreeSize) { fprintf(stderr, "Splay tree has wrong size\n"); return 1; }
    if (!ok) { fprintf(stderr, "Splay tree not sorted\n"); return 1; }
    printf("Splay done: size=%d checksum=%llu\n", count, (unsigned long long)acc);

    free_tree(tree.root);
    return 0;
}
