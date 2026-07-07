# Build the C ports with the same optimization level Solar's release link uses (-O3).
CC      = clang
CFLAGS  ?= -std=c11 -O3 -march=native
LDFLAGS ?= -pthread

all: allocs3 threads_list2 splay allocs5 sieve

allocs3: allocs3.c
	$(CC) $(CFLAGS) -o $@ $<

threads_list2: threads_list2.c
	$(CC) $(CFLAGS) $(LDFLAGS) -o $@ $<

splay: splay.c
	$(CC) $(CFLAGS) -o $@ $<

allocs5: allocs5.c
	$(CC) $(CFLAGS) $(LDFLAGS) -o $@ $<

sieve: sieve.c
	$(CC) $(CFLAGS) -o $@ $<

clean:
	rm -f allocs3 threads_list2 splay allocs5 sieve
