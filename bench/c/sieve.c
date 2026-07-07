// C port of examples/sieve.solar: a Sieve of Eratosthenes over 10^8, counting
// the primes below the limit (5761455). Unlike the other benchmarks this is
// NOT allocation-heavy: one 100 MB byte array up front, then two nested
// scan/mark loops (~3.4e8 array stores). It isolates raw generated-code
// quality -- array indexing, bounds checks (none in C), loop optimization --
// from the allocator and collector, which are idle after setup.
//
// The algorithm mirrors the Solar source exactly: a single pass that counts n
// as prime when prime[n] is still set and marks its multiples from 2n upward
// (not the n*n optimization), so every port does identical work.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdbool.h>

int main(void) {
    const size_t limit = 100000000;
    bool *prime = malloc(limit);
    memset(prime, 1, limit);
    prime[0] = false;
    prime[1] = false;
    long primes = 0;
    for (size_t n = 0; n < limit; n++) {
        if (prime[n]) {
            for (size_t i = n * 2; i < limit; i += n)
                prime[i] = false;
            primes++;
        }
    }
    printf("%ld\n", primes);
    free(prime);
    return 0;
}
