# Port of examples/allocs5.solar: retain the chain throughout threaded churn.
include("allocs3.jl")
include("threads_list2.jl")

function combined(; chain_size=100_000_000, workers=16, iterations=1000,
                  list_size=100_000)
    chain = build_chain(chain_size)
    GC.@preserve chain begin
        println(churn_lists(; workers, iterations, list_size))
        if chain.next !== nothing
            println("chain-live")
        end
        println("done")
    end
end

if abspath(PROGRAM_FILE) == @__FILE__
    combined()
    exit(0)
end
