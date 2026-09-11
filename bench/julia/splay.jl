# Port of examples/splay.solar and bench/go/splay.go. Match java.util.Random
# and the payload graph so all ports perform the same tree operations.
mutable struct JavaRandom
    seed::UInt64
end

function next_bits!(rng, bits)
    rng.seed = (rng.seed * 0x5deece66d + 0xb) & ((UInt64(1) << 48) - 1)
    return rng.seed >> (48 - bits)
end

function next_double!(rng)
    hi = next_bits!(rng, 26)
    lo = next_bits!(rng, 27)
    return Float64((hi << 27) + lo) / 2.0^53
end

mutable struct Leaf
    tag::Int64
    array::Vector{Int64}
end

mutable struct Payload
    left::Union{Nothing,Payload}
    right::Union{Nothing,Payload}
    leaf::Union{Nothing,Leaf}
end

function generate(depth, key)
    if depth == 0
        return Payload(nothing, nothing, Leaf(Int64(key * 2.0^53), collect(Int64, 0:9)))
    end
    return Payload(generate(depth - 1, key), generate(depth - 1, key), nothing)
end

mutable struct Node
    key::Float64
    value::Union{Nothing,Payload}
    left::Union{Nothing,Node}
    right::Union{Nothing,Node}
end

mutable struct SplayTree
    root::Union{Nothing,Node}
end

function splay!(tree, key)
    tree.root === nothing && return
    dummy = Node(0.0, nothing, nothing, nothing)
    left = right = dummy
    current = tree.root
    while true
        if key < current.key
            current.left === nothing && break
            if key < current.left.key
                tmp = current.left
                current.left = tmp.right
                tmp.right = current
                current = tmp
                current.left === nothing && break
            end
            right.left = current
            right = current
            current = current.left
        elseif key > current.key
            current.right === nothing && break
            if key > current.right.key
                tmp = current.right
                current.right = tmp.left
                tmp.left = current
                current = tmp
                current.right === nothing && break
            end
            left.right = current
            left = current
            current = current.right
        else
            break
        end
    end
    left.right = current.left
    right.left = current.right
    current.left = dummy.right
    current.right = dummy.left
    tree.root = current
end

function insert!(tree, key, value)
    if tree.root === nothing
        tree.root = Node(key, value, nothing, nothing)
        return
    end
    splay!(tree, key)
    tree.root.key == key && return
    node = Node(key, value, nothing, nothing)
    if key > tree.root.key
        node.left = tree.root
        node.right = tree.root.right
        tree.root.right = nothing
    else
        node.right = tree.root
        node.left = tree.root.left
        tree.root.left = nothing
    end
    tree.root = node
end

function remove!(tree, key)
    splay!(tree, key)
    @assert tree.root !== nothing && tree.root.key == key
    if tree.root.left === nothing
        tree.root = tree.root.right
    else
        right = tree.root.right
        tree.root = tree.root.left
        splay!(tree, key)
        tree.root.right = right
    end
end

function find!(tree, key)
    tree.root === nothing && return nothing
    splay!(tree, key)
    return tree.root.key == key ? tree.root : nothing
end

function greatest_less_than!(tree, key)
    tree.root === nothing && return nothing
    splay!(tree, key)
    tree.root.key < key && return tree.root
    current = tree.root.left
    current === nothing && return nothing
    while current.right !== nothing
        current = current.right
    end
    return current
end

function insert_new!(tree, rng, payload_depth)
    key = next_double!(rng)
    while find!(tree, key) !== nothing
        key = next_double!(rng)
    end
    insert!(tree, key, generate(payload_depth, key))
    return key
end

function traverse_check(node, acc=UInt64(0), count=0, last=0.0)
    current = node
    while current !== nothing
        acc, count, last = traverse_check(current.left, acc, count, last)
        @assert count == 0 || current.key > last "Splay tree not sorted"
        last = current.key
        acc += UInt64(current.key * 2.0^53)
        count += 1
        current = current.right
    end
    return acc, count, last
end

function run_once(; tree_size=8000, modifications=80, payload_depth=5, runs=5000)
    rng = JavaRandom((UInt64(12345) ⊻ 0x5deece66d) & ((UInt64(1) << 48) - 1))
    tree = SplayTree(nothing)
    for _ in 1:tree_size
        insert_new!(tree, rng, payload_depth)
    end
    for _ in 1:runs
        for _ in 1:modifications
            key = insert_new!(tree, rng, payload_depth)
            greatest = greatest_less_than!(tree, key)
            remove!(tree, greatest === nothing ? key : greatest.key)
        end
    end
    acc, count, _ = traverse_check(tree.root)
    @assert count == tree_size "Splay tree has wrong size"
    return acc
end

function main(; outer_runs=5, tree_size=8000, kwargs...)
    checksum = run_once(; tree_size, kwargs...)
    for _ in 2:outer_runs
        @assert run_once(; tree_size, kwargs...) == checksum "Splay checksum differs between runs"
    end
    println("Splay done: size=", tree_size, " checksum=", checksum)
end

if abspath(PROGRAM_FILE) == @__FILE__
    main()
end
