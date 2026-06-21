// A bump allocator exposed as an LD_PRELOAD-able malloc replacement: every
// allocation is an 8-byte-granular slice off a *per-thread* mmap'd arena, and
// `free` is a no-op. This is the "pure allocation throughput, zero reclamation
// cost" floor for the C benchmarks -- the closest manual-memory analogue of
// Solar's SOLAR_DISABLE_GC=1 bump mode. Build:
//   clang -O3 -fPIC -shared -o libbump.so bump.c
// Use:  LD_PRELOAD=./libbump.so ./allocs3
//
// Notes:
//   * Per-thread arena: each thread bumps a thread-local cursor with NO atomics
//     and NO shared cache line, so the multi-thread bench measures allocation
//     throughput, not contention on one global cursor. A thread refills by
//     mmap'ing a fresh chunk when its current one is exhausted.
//   * MAP_NORESERVE + demand-paged; resident memory grows with touched bytes.
//     Since free() never reclaims, RSS == high-water of all live+dead
//     allocations, so high-churn benches grow large.
//   * Headerless, 8-byte granularity: each block costs exactly its 8-byte-
//     rounded size (an 8-byte node uses 8 bytes, like mimalloc's small size
//     classes), so the never-freed high-churn bench stays resident, not swapping.
//     realloc copies the *new* size from the old pointer; an over-read stays
//     inside the mapped chunk, which is fine for these benchmarks.
#define _GNU_SOURCE
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include <stdio.h>
#include <stdlib.h>

// Each refill reserves an 8 GiB chunk of virtual address space (demand-paged,
// MAP_NORESERVE). A thread chains a new chunk when this one runs out.
#define CHUNK_BYTES (8ULL << 30)

// Per-thread bump cursor. Zero-initialized => first alloc triggers a refill.
static __thread uintptr_t t_next;
static __thread uintptr_t t_end;

static uintptr_t refill(size_t need) {
    // Reserve a fresh chunk large enough for `need` (oversized allocations get
    // their own exact mapping). No locking: mmap is thread-safe and each chunk
    // belongs to exactly one thread.
    size_t bytes = need > CHUNK_BYTES ? need : CHUNK_BYTES;
    void *p = mmap(NULL, bytes, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
    if (p == MAP_FAILED) {
        const char *m = "bump: mmap failed\n";
        ssize_t r = write(2, m, strlen(m));
        (void)r;
        _exit(1);
    }
    t_next = (uintptr_t)p;
    t_end = t_next + bytes;
    return t_next;
}

static inline void *bump_alloc(size_t size, size_t align) {
    // 8-byte granularity: an N-byte block only needs N-byte (<=8) alignment to
    // hold any object that fits, so tiny nodes pack at 8 bytes like mimalloc's
    // small size classes -- forcing 16 would double the resident footprint (and
    // the page/cache traffic) of an 8-byte-node bench. 16-byte+ requests keep
    // 16-byte alignment via the round-up below.
    // malloc's contract wants 16-byte alignment for requests >= 16 bytes, but
    // an <16-byte block is fine at 8 (nothing 16-aligned fits in it anyway).
    if (align < 16) align = size >= 16 ? 16 : 8;
    // Round the claim up to `align` so the cursor stays aligned for the next one.
    size_t total = (size + (align - 1)) & ~(align - 1);
    if (total == 0) total = align;

    uintptr_t start = (t_next + (align - 1)) & ~(uintptr_t)(align - 1);
    if (start + total > t_end) {
        // Refill, then re-align inside the fresh chunk (chunk base is page- and
        // thus 16-aligned; for align>16 we still round up, wasting <align bytes).
        uintptr_t base = refill(total + align);
        start = (base + (align - 1)) & ~(uintptr_t)(align - 1);
    }
    t_next = start + total;
    return (void *)start;
}

void *malloc(size_t size) { return bump_alloc(size, 8); }

void free(void *ptr) { (void)ptr; } // no-op: the whole point

void *calloc(size_t n, size_t size) {
    // mmap memory is zero-filled and never reused (free is a no-op), so every
    // block handed out is already zero.
    return bump_alloc(n * size, 8);
}

void *realloc(void *ptr, size_t size) {
    void *fresh = bump_alloc(size, 8);
    // No stored size: copy `size` bytes from the old block. Reading past the old
    // allocation stays inside the mapped chunk, so it's safe here (the extra
    // bytes are simply discarded by callers that grew the block).
    if (ptr && size) memcpy(fresh, ptr, size);
    return fresh;
}

int posix_memalign(void **out, size_t align, size_t size) {
    *out = bump_alloc(size, align);
    return 0;
}

void *aligned_alloc(size_t align, size_t size) { return bump_alloc(size, align); }
void *memalign(size_t align, size_t size) { return bump_alloc(size, align); }

// No stored size; nothing in these benchmarks depends on the result.
size_t malloc_usable_size(void *ptr) { (void)ptr; return 0; }
