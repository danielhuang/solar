# Port of examples/allocs3.solar: retain a chain of 100 million heap cells.
# Mutable nodes ensure reference identity and a separate heap allocation.
mutable struct Chain
    next::Union{Nothing,Chain}
end

function build_chain(count=100_000_000)
    chain = Chain(nothing)
    for _ in 1:count
        chain = Chain(chain)
    end
    return chain
end

function main()
    chain = build_chain()
    println("head-live=", chain.next !== nothing)
end

if abspath(PROGRAM_FILE) == @__FILE__
    main()
end
