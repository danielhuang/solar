// C reference for examples/loop2.solar and examples/loop2fn5.solar.
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>

int main(void) {
    int64_t i = 0;
    while (i < 1000000000) {
        if (i % 10000 == 0) {
            printf("%" PRId64 "\n", i);
        }
        i = i + 1;
    }
    return 0;
}
