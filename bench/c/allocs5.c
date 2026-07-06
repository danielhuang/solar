// C port of examples/allocs5.solar (and bench/java/Allocs5.java).
//
// Phase 1 (allocs3): builds a heap chain of 100M `Chain` cells that is never
// freed -- the retained live set. Phase 2 (threads_list2): 16 worker threads
// each build, 1000 times, a fresh 100k-node singly-linked list, publish the
// head into the shared `root`, and manually free the list they built the
// previous iteration. In the GC-managed ports every collection must trace the
// ~800 MB retained chain concurrently with the 1.6 billion-node churn; with
// manual memory management the retained chain costs nothing after it is built
// (no collector scans it), so this stays a malloc/free-throughput test with a
// large resident footprint.
//
// The same manual-memory caveats as threads_list2.c apply: each thread frees
// only the lists IT built, never `root`, and main reads the always-live
// sentinel/chain head rather than `root`.
#include <stdio.h>
#include <stdlib.h>
#include <stdbool.h>
#include <stdatomic.h>
#include <pthread.h>

typedef struct Chain {
    struct Chain *next; // NULL == empty (null#[Chain]), else points to prev
} Chain;

typedef struct Node {
    long value;        // Solar `Int` is 64-bit
    struct Node *next; // NULL == empty (null#[Node])
} Node;

static Node *sentinel;
static _Atomic(Node *) root;     // atomic_store / atomic_load target
static atomic_bool is_done;

static void free_list(Node *head) {
    while (head != NULL && head != sentinel) {
        Node *next = head->next;
        free(head);
        head = next;
    }
}

static void *worker(void *arg) {
    (void)arg;
    Node *prev_built = NULL; // this thread's previous-iteration list
    for (int iter = 0; iter < 1000; iter++) {
        Node *head = sentinel;
        for (long j = 0; j < 100000L; j++) {
            Node *n = malloc(sizeof(Node));
            n->value = j;
            n->next = head;
            head = n;
        }
        atomic_store_explicit(&root, head, memory_order_release);
        free_list(prev_built); // reclaim what this thread built last time
        prev_built = head;
    }
    atomic_store_explicit(&is_done, true, memory_order_release);
    return NULL;
}

int main(void) {
    // Phase 1: the retained chain, never freed.
    Chain *chain = malloc(sizeof(Chain));
    chain->next = NULL;
    for (long i = 0; i < 100000000L; i++) {
        Chain *c = malloc(sizeof(Chain));
        c->next = chain;
        chain = c;
    }

    // Phase 2: concurrent churn while the chain stays resident.
    sentinel = malloc(sizeof(Node));
    sentinel->value = 0;
    sentinel->next = NULL;
    atomic_store(&root, sentinel);

    pthread_t th[16];
    for (int t = 0; t < 16; t++)
        pthread_create(&th[t], NULL, worker, NULL);

    while (!atomic_load_explicit(&is_done, memory_order_acquire)) {
        /* spin, matching Solar's busy-wait */
    }
    printf("%ld\n", sentinel->value); // sentinel is always live (see header)
    if (chain->next != NULL) printf("chain-live\n");
    printf("done\n");
    return 0;
}
