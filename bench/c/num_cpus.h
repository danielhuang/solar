#ifndef BENCH_NUM_CPUS_H
#define BENCH_NUM_CPUS_H
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#include <assert.h>
#include <sched.h>

// Count CPUs available to this Linux process, including affinity restrictions.
static inline int num_cpus(void) {
    cpu_set_t cpus;
    int error = sched_getaffinity(0, sizeof(cpus), &cpus);
    assert(error == 0);
    int count = CPU_COUNT(&cpus);
    assert(count > 0);
    return count;
}
#endif
