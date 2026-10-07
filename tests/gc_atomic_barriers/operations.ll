declare void @sol_store_128_unordered(ptr, ptr)
declare void @sol_load_128_unordered(ptr, ptr)
declare void @sol_copy_128_unordered(ptr, ptr)
declare void @sol_atomic_store_128_rel(ptr, ptr)
declare void @sol_atomic_load_128_acq(ptr, ptr)
declare void @sol_atomic_compare_exchange_128_acq_rel(ptr, ptr, ptr, ptr)
declare void @llvm.memcpy.element.unordered.atomic.p0.p0.i64(ptr, ptr, i64, i32 immarg)

define void @solar_store(ptr %dst, i64 %hidden) noinline {
  %v = xor i64 %hidden, -1
  store atomic i64 %v, ptr %dst release, align 8
  ret void
}
define void @solar_exchange(ptr %dst, i64 %hidden) noinline {
  %v = xor i64 %hidden, -1
  %old = atomicrmw xchg ptr %dst, i64 %v acq_rel
  ret void
}
define void @solar_compare(ptr %dst, i64 %hidden) noinline {
  %v = xor i64 %hidden, -1
  %old = cmpxchg ptr %dst, i64 0, i64 %v acq_rel acquire
  ret void
}

; Each wide helper writes a heap destination. Its source remains on the stack.
define void @solar_wide_store(ptr %dst, i64 %hidden) noinline {
  %src = alloca [2 x i64], align 16
  %v = xor i64 %hidden, -1
  store i64 %v, ptr %src
  %tail = getelementptr i64, ptr %src, i64 1
  store i64 8, ptr %tail
  call void @sol_atomic_store_128_rel(ptr %dst, ptr %src)
  ret void
}
define void @solar_wide_load(ptr %dst, i64 %hidden) noinline {
  %src = alloca [2 x i64], align 16
  %v = xor i64 %hidden, -1
  store i64 %v, ptr %src
  %tail = getelementptr i64, ptr %src, i64 1
  store i64 8, ptr %tail
  call void @sol_atomic_load_128_acq(ptr %dst, ptr %src)
  ret void
}
define void @solar_wide_copy(ptr %dst, i64 %hidden) noinline {
  %src = alloca [2 x i64], align 16
  %v = xor i64 %hidden, -1
  store i64 %v, ptr %src
  %tail = getelementptr i64, ptr %src, i64 1
  store i64 8, ptr %tail
  call void @llvm.memcpy.element.unordered.atomic.p0.p0.i64(ptr align 16 %dst, ptr align 16 %src, i64 16, i32 16)
  ret void
}
define void @solar_wide_compare(ptr %dst, i64 %hidden) noinline {
  %src = alloca [2 x i64], align 16
  %expected = alloca i128, align 16
  %old = alloca i128, align 16
  store i128 0, ptr %expected
  %v = xor i64 %hidden, -1
  store i64 %v, ptr %src
  %tail = getelementptr i64, ptr %src, i64 1
  store i64 8, ptr %tail
  call void @sol_atomic_compare_exchange_128_acq_rel(ptr %old, ptr %dst, ptr %expected, ptr %src)
  ret void
}
define void @solar_unordered_store(ptr %dst, i64 %hidden) noinline {
  %src = alloca [2 x i64], align 16
  %v = xor i64 %hidden, -1
  store i64 %v, ptr %src
  %tail = getelementptr i64, ptr %src, i64 1
  store i64 8, ptr %tail
  call void @sol_store_128_unordered(ptr %dst, ptr %src)
  ret void
}
define void @solar_unordered_load(ptr %dst, i64 %hidden) noinline {
  %src = alloca [2 x i64], align 16
  %v = xor i64 %hidden, -1
  store i64 %v, ptr %src
  %tail = getelementptr i64, ptr %src, i64 1
  store i64 8, ptr %tail
  call void @sol_load_128_unordered(ptr %dst, ptr %src)
  ret void
}
define void @solar_stack_only(i64 %v) {
  %dst = alloca i64, align 8
  store atomic i64 %v, ptr %dst release, align 8
  %old = atomicrmw xchg ptr %dst, i64 %v acq_rel
  %pair = cmpxchg ptr %dst, i64 0, i64 %v acq_rel acquire
  ret void
}

; Exercise the upper reference word when 128-bit atomics are visible in IR.
define void @solar_direct_wide_exchange(ptr %dst, i64 %hidden) noinline "target-features"="+cx16" {
  %v = xor i64 %hidden, -1
  %wide = zext i64 %v to i128
  %high = shl i128 %wide, 64
  %pair = or i128 %high, 8
  %old = atomicrmw xchg ptr %dst, i128 %pair acq_rel
  ret void
}
define void @solar_direct_wide_compare(ptr %dst, i64 %hidden) noinline "target-features"="+cx16" {
  %v = xor i64 %hidden, -1
  %wide = zext i64 %v to i128
  %high = shl i128 %wide, 64
  %pair = or i128 %high, 8
  %old = cmpxchg ptr %dst, i128 0, i128 %pair acq_rel acquire
  ret void
}
