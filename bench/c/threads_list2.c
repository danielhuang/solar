// C port of examples/threads_list2.solar (and bench/java/ThreadsList2.java).
//
// 16 worker threads each build, 1000 times, a fresh 100k-node singly-linked
// list hanging off a shared sentinel, publish the head into the shared `root`
// (an atomic store, matching Solar's `atomic_store`), and then -- because there
// is no collector -- *manually free* the list they built in the previous
// iteration. That makes this the manual-memory-management analogue of Solar's
// concurrent allocate-and-discard test: identical allocation volume
// (16 x 1000 x 100k = 1.6 billion `Node`s), with the reclamation that Solar's
// GC does concurrently instead paid inline by `free`.
//
// Differences forced by manual memory management:
//   * Each thread frees only the lists IT built (its own previous iteration's
//     list), never `root`. Freeing via the shared `root` would race other
//     threads into use-after-free; with a GC this is automatic and safe.
//   * `main` therefore reads `sentinel->value` (always live, never freed) for
//     its final print rather than `root->value`, which a worker may have just
//     freed. `root` is still written on every iteration so the atomic-store
//     traffic matches the original.
//
// Like the Solar/Java ports, the first worker to finish sets `is_done`; `main`
// observes it, prints, and returns, abandoning the other 15 threads (the
// process exits on return, matching Solar and Java's daemon threads).
#include <stdio.h>
#include <stdlib.h>
#include <stdbool.h>
#include <stdatomic.h>
#include <pthread.h>

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
    printf("done\n");
    return 0;
}
