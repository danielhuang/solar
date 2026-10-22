#include <assert.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

static unsigned allocations;

void *sol_alloc(uint64_t size, uint64_t alignment, void *mark) {
  assert(alignment <= 16);
  assert(mark == NULL);
  void *result = malloc(size);
  assert(result);
  memset(result, 0xa5, size);
  ++allocations;
  return result;
}

void *sol_alloc_class_1(uint64_t size, uint64_t alignment, void *mark) {
  return sol_alloc(size, alignment, mark);
}

void *sol_alloc_class_1_impl(uint64_t size, uint64_t alignment, void *mark) {
  return sol_alloc(size, alignment, mark);
}

void observe(unsigned char *destination) {
  assert(destination[0] == 0);
  memset(destination, 0xa5, 24);
}

extern unsigned char *solar_stores(uint64_t, uint64_t);
extern unsigned char *solar_slice(void);
extern unsigned char *solar_memset(void);
extern unsigned char *solar_specialized(uint64_t);

static uint64_t read_word(unsigned char *address) {
  uint64_t value;
  memcpy(&value, address, sizeof(value));
  return value;
}

int main(void) {
  unsigned char *stores = solar_stores(42, 123);
  assert(stores[0] == 0xa5);
  assert(read_word(stores + 1) == 123);
  assert(read_word(stores + 9) == 99);
  for (unsigned i = 17; i < 24; ++i)
    assert(stores[i] == 0xa5);
  free(stores);

  unsigned char *slice = solar_slice();
  assert(memcmp(slice, "0\n", 2) == 0);
  free(slice);

  unsigned char *set = solar_memset();
  for (unsigned i = 0; i < 32; ++i)
    assert(set[i] == (i >= 8 && i < 24 ? 7 : 0xa5));
  free(set);

  unsigned char *specialized = solar_specialized(456);
  assert(read_word(specialized) == 456);
  assert(read_word(specialized + 8) == 0);
  free(specialized);
  assert(allocations == 4);
}
