/* The Computer Language Benchmarks Game
 * https://salsa.debian.org/benchmarksgame-team/benchmarksgame/
 *
 * binarytrees-gpp-7, contributed by Danial Klimkin (C++), Dmytro Ovdiienko,
 * Martin Jambrek, and the Rust Project Developers.
 * Original source:
 *   https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/binarytrees-gpp-7.html
 *
 * ADAPTED for this benchmark: the original uses boost::counting_iterator and
 * TBB-backed parallel STL (std::execution::par), neither available in this
 * environment. The std::for_each(par, ...) over depths is replaced with one
 * std::thread per depth, each with its own pmr::monotonic_buffer_resource
 * arena -- structurally identical to the Solar port (examples/binarytrees.solar).
 * The arena allocation that defines this entry's performance is unchanged.
 *
 * Build: g++ -O3 -march=native -std=c++17 binarytrees_arena.cpp -o bt -lpthread
 */
#include <algorithm>
#include <iostream>
#include <memory_resource>
#include <thread>
#include <vector>

using MemoryPool = std::pmr::monotonic_buffer_resource;

struct Node {
    Node *l, *r;

    int check() const
    {
        if (l)
            return l->check() + 1 + r->check();
        else
            return 1;
    }
};

inline static Node* make(const int d, MemoryPool& store)
{
    void* mem = store.allocate(sizeof(Node), alignof(Node));
    Node* root = new (mem) Node;
    if (d > 0) {
        root->l = make(d - 1, store);
        root->r = make(d - 1, store);
    } else {
        root->l = root->r = nullptr;
    }
    return root;
}

constexpr auto MIN_DEPTH = 4;

int main(int argc, char* argv[])
{
    const int max_depth = std::max(MIN_DEPTH + 2, (argc == 2 ? atoi(argv[1]) : 10));
    const int stretch_depth = max_depth + 1;

    {
        MemoryPool store;
        Node* c = make(stretch_depth, store);
        std::cout << "stretch tree of depth " << stretch_depth << "\t "
                  << "check: " << c->check() << std::endl;
    }

    MemoryPool long_lived_store;
    Node* long_lived_tree = make(max_depth, long_lived_store);

    std::vector<std::pair<int, int>> results((max_depth - MIN_DEPTH) / 2 + 1);
    for (size_t i = 0; i < results.size(); ++i)
        results[i].first = i * 2 + MIN_DEPTH;

    std::vector<std::thread> workers;
    for (auto& res : results) {
        workers.emplace_back([&res, max_depth] {
            int d = res.first;
            int iters = 1 << (max_depth - d + MIN_DEPTH);
            int sum = 0;
            for (int i = 0; i < iters; ++i) {
                MemoryPool pool;
                sum += make(d, pool)->check();
            }
            res.second = sum;
        });
    }
    for (auto& w : workers)
        w.join();

    for (const auto& [d, c] : results) {
        std::cout << (1 << (max_depth - d + MIN_DEPTH))
                  << "\t trees of depth " << d
                  << "\t check: " << c << "\n";
    }

    std::cout << "long lived tree of depth " << max_depth << "\t "
              << "check: " << (long_lived_tree->check()) << "\n";

    return 0;
}
