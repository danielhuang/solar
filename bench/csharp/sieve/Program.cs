// C# port of examples/sieve.solar: a Sieve of Eratosthenes over 10^8, counting
// the primes below the limit (5761455). Unlike the other benchmarks this is
// NOT allocation-heavy: one 100 MB bool[] up front, then two nested scan/mark
// loops (~3.4e8 array stores) -- the collector has nothing to do after setup.
// It isolates raw generated-code quality (array indexing, bounds checks, loop
// optimization, JIT warmup) from the allocator and collector.
//
// The algorithm mirrors the Solar source exactly: a single pass that counts n
// as prime when prime[n] is still set and marks its multiples from 2n upward,
// so every port does identical work.

GcPause.MaybeStart();

const int limit = 100_000_000;
bool[] prime = new bool[limit];
Array.Fill(prime, true);
prime[0] = false;
prime[1] = false;
long primes = 0;
for (int n = 0; n < limit; n++)
{
    if (prime[n])
    {
        for (long i = (long)n * 2; i < limit; i += n)
            prime[(int)i] = false;
        primes++;
    }
}
Console.WriteLine(primes);
