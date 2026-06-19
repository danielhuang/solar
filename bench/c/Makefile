# Build the C ports with the same optimization level Solar's release link uses (-O3).
CC      = clang
CFLAGS  ?= -std=c11 -O3 -march=native
LDFLAGS ?= -pthread

all: allocs3 threads_list2

allocs3: allocs3.c
	$(CC) $(CFLAGS) -o $@ $<

threads_list2: threads_list2.c
	$(CC) $(CFLAGS) $(LDFLAGS) -o $@ $<

clean:
	rm -f allocs3 threads_list2
