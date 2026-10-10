#include <assert.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdlib.h>
#include <unistd.h>

typedef void (*Mark)(void *, void *, uint64_t);
struct Root { void *address; uint64_t size; Mark mark; };
extern void sol_start(void (*)(void *), void *, size_t, void (*)(void));
extern void *sol_alloc_impl(size_t, size_t, Mark);
extern void sol_set_mark_fn(void *, Mark);
extern void sol_collect_gc(void);
extern void sol_gc_mark(void *, void *);
extern void sol_enable_gc_san(void);
extern void sol_gc_san_check(const void *, size_t);
const char *sol_payload_type_name(uint64_t tag) { (void)tag; return "()"; }

static void *parent;
static int big_mode;
static atomic_int precise_scans;

static void mark_none(void *ctx, void *object, uint64_t size) {
    (void)ctx; (void)object; (void)size;
}
static void mark_root(void *ctx, void *object, uint64_t size) {
    (void)object; (void)size;
    sol_gc_mark(ctx, parent);
}
static void mark_parent(void *ctx, void *object, uint64_t size) {
    (void)size;
    atomic_fetch_add(&precise_scans, 1);
    sol_gc_mark(ctx, *(void **)object);
}

__attribute__((noinline)) static void *make_parent(void) {
    void *child = sol_alloc_impl(128, 16, mark_none);
    *(uint64_t *)child = 0xabcddcba;
    void *object = sol_alloc_impl(128, big_mode ? 1UL << 31 : 16, NULL);
    *(void **)object = child;
    return object;
}

static void body(void *unused) {
    (void)unused;
    parent = make_parent();
    sol_collect_gc();
    void *child = *(void **)parent;
    sol_gc_san_check(child, 128);
    assert(*(uint64_t *)child == 0xabcddcba);

    sol_set_mark_fn(parent, mark_parent);
    sol_collect_gc();
    assert(atomic_load(&precise_scans) > 0);
    sol_gc_san_check(child, 128);
    assert(*(uint64_t *)child == 0xabcddcba);
}

int main(int argc, char **argv) {
    assert(argc == 2);
    big_mode = atoi(argv[1]);
    alarm(20);
    sol_enable_gc_san();
    struct Root root = { &parent, sizeof(parent), mark_root };
    sol_start(body, &root, 1, NULL);
}
