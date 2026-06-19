// C port of examples/allocs3.solar (and bench/java/Allocs3.java).
//
// Builds a heap chain of 100M `Node` cells, each holding a pointer to the
// previous one, and *never frees it* -- matching the Solar source, whose only
// per-iteration heap object is the `Node { next: prev& }` cell and which leaves
// the whole chain live at exit. With manual memory management this collapses to
// a pure malloc-throughput test: there is no collector, so nothing scans the
// growing live set; the only cost is 100M `malloc` calls plus the resident
// footprint of the chain.
//
// Footprint note: a Solar node is a single 8-byte `&?Node` cell (~800 MB live).
// glibc's malloc rounds this 8-byte request up to its 32-byte minimum chunk, so
// the C chain resides in ~3.2 GB -- 4x Solar -- which is the allocator-overhead
// tax manual malloc pays here.
#include <stdio.h>
#include <stdlib.h>

typedef struct Node {
    struct Node *next; // NULL == empty (null#[Node]), else points to prev
} Node;

int main(void) {
    Node *node = malloc(sizeof(Node));
    node->next = NULL;
    for (long i = 0; i < 100000000L; i++) {
        Node *n = malloc(sizeof(Node));
        n->next = node;
        node = n;
    }
    // Keep the chain reachable / defeat dead-code elimination by reading the
    // head after the loop, exactly like the Solar and Java ports.
    long sink = 0;
    if (node->next != NULL) sink++;
    printf("head-live=%s\n", sink == 1 ? "true" : "false");
    return 0;
}
