# Port of examples/threads_list2.solar. Run with --threads=17: sixteen
# allocating workers plus the main task waiting for the first completion.
mutable struct ListNode
    value::Int64
    next::Union{Nothing,ListNode}
end

mutable struct SharedLists
    @atomic root::ListNode
    @atomic done::Bool
end

function churn!(shared, sentinel, iterations, list_size)
    for _ in 1:iterations
        head = sentinel
        for j in 0:(list_size - 1)
            head = ListNode(j, head)
        end
        @atomic shared.root = head
    end
    @atomic shared.done = true
end

function churn_lists(; workers=16, iterations=1000, list_size=100_000)
    @assert Threads.nthreads() >= workers + 1 "Use --threads=17 for the default workload"
    sentinel = ListNode(0, nothing)
    shared = SharedLists(sentinel, false)
    for _ in 1:workers
        Threads.@spawn churn!(shared, sentinel, iterations, list_size)
    end
    while !(@atomic shared.done)
        # An allocation-free spin must still let the collector stop this thread.
        GC.safepoint()
    end
    return (@atomic shared.root).value
end

if abspath(PROGRAM_FILE) == @__FILE__
    println(churn_lists())
    println("done")
    # Match the other ports: abandon remaining workers after the first finishes.
    exit(0)
end
