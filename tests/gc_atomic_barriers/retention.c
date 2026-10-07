#include <assert.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sched.h>
#include <unistd.h>

typedef void (*Mark)(void *, void *, uint64_t);
struct Root { void *address; uint64_t size; Mark mark; };
extern void sol_start(void (*)(void *), void *, size_t, void (*)(void));
extern void *sol_alloc_impl(size_t, size_t, Mark);
extern void sol_thread_spawn(void (*)(void *), void *, void (*)(void), void (*)(void *));
extern void sol_collect_gc(void);
extern void sol_gc_mark(void *, void *);
extern void sol_enable_gc_san(void);
extern void sol_gc_san_check(const void *, size_t);
extern unsigned char SOL_SAFEPOINT_PAGE[];
const char *sol_payload_type_name(uint64_t tag) { return "Unit"; }

#define OP(name) extern void name(void *, uintptr_t);
OP(solar_store) OP(solar_exchange) OP(solar_compare)
OP(solar_wide_store) OP(solar_wide_load) OP(solar_wide_copy)
OP(solar_wide_compare) OP(solar_unordered_store) OP(solar_unordered_load)
OP(solar_direct_wide_exchange) OP(solar_direct_wide_compare)
static void (*operations[])(void *, uintptr_t) = {
    solar_store, solar_exchange, solar_compare, solar_wide_store,
    solar_wide_load, solar_wide_copy, solar_wide_compare,
    solar_unordered_store, solar_unordered_load,
    solar_direct_wide_exchange, solar_direct_wide_compare
};
static int operation;
static void *container;
static atomic_bool scanned, ready, proceed, completed;
static void poll(void) { (void)*(volatile unsigned char *)SOL_SAFEPOINT_PAGE; }
static void mark_none(void *ctx, void *object, uint64_t size) {}
static void mark_root(void *ctx, void *object, uint64_t size) {
    sol_gc_mark(ctx, *(void **)object);
}
static void mark_container(void *ctx, void *object, uint64_t size) {
    // Visit its initial edge exactly once, then let the mutator publish a new
    // white child. Later duplicate visits must not conceal a missing barrier.
    if (!atomic_exchange(&scanned, 1)) {
        sol_gc_mark(ctx, atomic_load((_Atomic(void *) *)object));
        sol_gc_mark(ctx, atomic_load((_Atomic(void *) *)object + 1));
        atomic_store(&ready, 1);
        while (!atomic_load(&proceed)) sched_yield();
    }
}
__attribute__((noinline)) static uintptr_t make_hidden_target(void) {
    unsigned char *p = sol_alloc_impl(128, 16, mark_none);
    memset(p, 77, 128);
    // No conservative root may expose the target before the atomic store.
    return (uintptr_t)p ^ UINTPTR_MAX;
}
static void collect(void *unused) {
    sol_collect_gc();
    atomic_store(&completed, 1);
}
static void body(void *unused) {
    container = sol_alloc_impl(128, 16, mark_container);
    memset(container, 0, 128);
    uintptr_t hidden = make_hidden_target();
    sol_thread_spawn(collect, 0, 0, 0);
    while (!atomic_load(&ready)) poll();
    operations[operation](container, hidden);
    atomic_store(&proceed, 1);
    while (!atomic_load(&completed)) poll();
    unsigned char *child = atomic_load((_Atomic(void *) *)container + (operation >= 9));
    sol_gc_san_check(child, 128);
    assert(child[0] == 77 && child[127] == 77);
}
int main(int argc, char **argv) {
    assert(argc == 2);
    operation = atoi(argv[1]);
    assert(operation >= 0 && operation < (int)(sizeof(operations) / sizeof(*operations)));
    alarm(30);
    sol_enable_gc_san();
    struct Root root = {&container, sizeof(container), mark_root};
    sol_start(body, &root, 1, 0);
}
