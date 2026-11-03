#pragma once
#define ANSWER (6 * 7)
#define HIGH_BIT (1U << 31)
#define ALL_BITS (~0ULL)
#define NEGATIVE (-19LL)
#define FRACTION (3.0 / 8.0)
#define SINGLE 1.5f
#define TEXT "hi\n"
#define NIL ((void *)0)
static const void *const CONST_NIL = (void *)0;
static const short NUMBERS[3] = {2, -4, 9};
typedef unsigned long Count;
typedef struct Node {
    unsigned char tag;
    long value;
    const struct Node *next;
    short samples[2];
    int (*matrix)[3];
} Node;
typedef struct { int x; double y; } Pair;
#define MAKE_RECORD(NAME, FIELD) typedef struct { int FIELD; } NAME
MAKE_RECORD(MacroFirst, first);
MAKE_RECORD(MacroSecond, second);
struct Outer { Pair pair; Node nodes[2]; };
enum { CHOICE = 17 };
int forbidden_function(int);
extern int mutable_global;
union BadUnion { int i; double d; };
struct BadBits { unsigned bits : 3; };
struct __attribute__((packed)) BadPacked { char c; int i; };
struct __attribute__((aligned(16))) BadAligned { long value; };
struct BadFieldAlignment { _Alignas(16) int value; };
struct BadVolatile { volatile int value; };
struct BadFlexible { int count; int values[]; };
struct BadEmpty {};
struct BadZero { int values[0]; };
struct Incomplete;
typedef void (*Callback)(int);
typedef long double TooWide;
typedef int AlignedInt __attribute__((aligned(16)));
struct BadNested { union BadUnion value; };
