; Small runtime helpers optimized together with generated Solar code.
; Rust retains the cold paths so debug and release use identical exceptions.
; ODR linkage also permits the native fallback object in libsolar_system.a to
; satisfy debug callers without conflicting with release-inlined definitions.
target triple = "x86_64-unknown-linux-gnu"
declare i64 @sol_checked_add_int_slow(i64, i64) cold noreturn
define weak_odr i64 @sol_checked_add_int(i64 %a, i64 %b) alwaysinline uwtable {
entry:
  %pair = call { i64, i1 } @llvm.sadd.with.overflow.i64(i64 %a, i64 %b)
  %overflow = extractvalue { i64, i1 } %pair, 1
  br i1 %overflow, label %slow, label %ok
ok:
  %value = extractvalue { i64, i1 } %pair, 0
  ret i64 %value
slow:
  tail call i64 @sol_checked_add_int_slow(i64 %a, i64 %b)
  unreachable
}
declare i64 @sol_checked_sub_int_slow(i64, i64) cold noreturn
define weak_odr i64 @sol_checked_sub_int(i64 %a, i64 %b) alwaysinline uwtable {
entry:
  %pair = call { i64, i1 } @llvm.ssub.with.overflow.i64(i64 %a, i64 %b)
  %overflow = extractvalue { i64, i1 } %pair, 1
  br i1 %overflow, label %slow, label %ok
ok:
  %value = extractvalue { i64, i1 } %pair, 0
  ret i64 %value
slow:
  tail call i64 @sol_checked_sub_int_slow(i64 %a, i64 %b)
  unreachable
}
declare i64 @sol_checked_mul_int_slow(i64, i64) cold noreturn
define weak_odr i64 @sol_checked_mul_int(i64 %a, i64 %b) alwaysinline uwtable {
entry:
  %pair = call { i64, i1 } @llvm.smul.with.overflow.i64(i64 %a, i64 %b)
  %overflow = extractvalue { i64, i1 } %pair, 1
  br i1 %overflow, label %slow, label %ok
ok:
  %value = extractvalue { i64, i1 } %pair, 0
  ret i64 %value
slow:
  tail call i64 @sol_checked_mul_int_slow(i64 %a, i64 %b)
  unreachable
}
declare i64 @sol_checked_div_int_slow(i64, i64) cold noreturn
define weak_odr i64 @sol_checked_div_int(i64 %a, i64 %b) alwaysinline uwtable {
entry:
  %zero = icmp eq i64 %b, 0
  %min = icmp eq i64 %a, -9223372036854775808
  %neg_one = icmp eq i64 %b, -1
  %overflow = and i1 %min, %neg_one
  %invalid = or i1 %zero, %overflow
  br i1 %invalid, label %slow, label %ok
ok:
  %value = sdiv i64 %a, %b
  ret i64 %value
slow:
  tail call i64 @sol_checked_div_int_slow(i64 %a, i64 %b)
  unreachable
}
declare i64 @sol_checked_mod_int_slow(i64, i64) cold noreturn
define weak_odr i64 @sol_checked_mod_int(i64 %a, i64 %b) alwaysinline uwtable {
entry:
  %zero = icmp eq i64 %b, 0
  %min = icmp eq i64 %a, -9223372036854775808
  %neg_one = icmp eq i64 %b, -1
  %overflow = and i1 %min, %neg_one
  %invalid = or i1 %zero, %overflow
  br i1 %invalid, label %slow, label %ok
ok:
  %value = srem i64 %a, %b
  ret i64 %value
slow:
  tail call i64 @sol_checked_mod_int_slow(i64 %a, i64 %b)
  unreachable
}
declare i64 @sol_checked_add_uint_slow(i64, i64) cold noreturn
define weak_odr i64 @sol_checked_add_uint(i64 %a, i64 %b) alwaysinline uwtable {
entry:
  %pair = call { i64, i1 } @llvm.uadd.with.overflow.i64(i64 %a, i64 %b)
  %overflow = extractvalue { i64, i1 } %pair, 1
  br i1 %overflow, label %slow, label %ok
ok:
  %value = extractvalue { i64, i1 } %pair, 0
  ret i64 %value
slow:
  tail call i64 @sol_checked_add_uint_slow(i64 %a, i64 %b)
  unreachable
}
declare i64 @sol_checked_sub_uint_slow(i64, i64) cold noreturn
define weak_odr i64 @sol_checked_sub_uint(i64 %a, i64 %b) alwaysinline uwtable {
entry:
  %pair = call { i64, i1 } @llvm.usub.with.overflow.i64(i64 %a, i64 %b)
  %overflow = extractvalue { i64, i1 } %pair, 1
  br i1 %overflow, label %slow, label %ok
ok:
  %value = extractvalue { i64, i1 } %pair, 0
  ret i64 %value
slow:
  tail call i64 @sol_checked_sub_uint_slow(i64 %a, i64 %b)
  unreachable
}
declare i64 @sol_checked_mul_uint_slow(i64, i64) cold noreturn
define weak_odr i64 @sol_checked_mul_uint(i64 %a, i64 %b) alwaysinline uwtable {
entry:
  %pair = call { i64, i1 } @llvm.umul.with.overflow.i64(i64 %a, i64 %b)
  %overflow = extractvalue { i64, i1 } %pair, 1
  br i1 %overflow, label %slow, label %ok
ok:
  %value = extractvalue { i64, i1 } %pair, 0
  ret i64 %value
slow:
  tail call i64 @sol_checked_mul_uint_slow(i64 %a, i64 %b)
  unreachable
}
declare i64 @sol_checked_div_uint_slow(i64, i64) cold noreturn
define weak_odr i64 @sol_checked_div_uint(i64 %a, i64 %b) alwaysinline uwtable {
entry:
  %zero = icmp eq i64 %b, 0
  br i1 %zero, label %slow, label %ok
ok:
  %value = udiv i64 %a, %b
  ret i64 %value
slow:
  tail call i64 @sol_checked_div_uint_slow(i64 %a, i64 %b)
  unreachable
}
declare i64 @sol_checked_mod_uint_slow(i64, i64) cold noreturn
define weak_odr i64 @sol_checked_mod_uint(i64 %a, i64 %b) alwaysinline uwtable {
entry:
  %zero = icmp eq i64 %b, 0
  br i1 %zero, label %slow, label %ok
ok:
  %value = urem i64 %a, %b
  ret i64 %value
slow:
  tail call i64 @sol_checked_mod_uint_slow(i64 %a, i64 %b)
  unreachable
}
declare { i64, i1 } @llvm.sadd.with.overflow.i64(i64, i64)
declare { i64, i1 } @llvm.ssub.with.overflow.i64(i64, i64)
declare { i64, i1 } @llvm.smul.with.overflow.i64(i64, i64)
declare { i64, i1 } @llvm.uadd.with.overflow.i64(i64, i64)
declare { i64, i1 } @llvm.usub.with.overflow.i64(i64, i64)
declare { i64, i1 } @llvm.umul.with.overflow.i64(i64, i64)

; Bounds checks remain visible to loop optimization. Overflow and invalid ranges
; go through the original Rust implementation, including its panic behavior.
declare ptr @sol_slice_index_slow(ptr, i64, i64, i64) cold noreturn
declare ptr @sol_slice_range_slow(ptr, i64, i64, i64, i64) cold noreturn
declare ptr @sol_null_check_slow(ptr) cold noreturn
declare void @sol_assert_array_len_slow(i64, i64) cold noreturn

define weak_odr ptr @sol_slice_index(ptr %base, i64 %index, i64 %len, i64 %size) alwaysinline uwtable {
entry:
  %bounds = icmp uge i64 %index, %len
  %pair = call { i64, i1 } @llvm.umul.with.overflow.i64(i64 %index, i64 %size)
  %overflow = extractvalue { i64, i1 } %pair, 1
  %invalid = or i1 %bounds, %overflow
  br i1 %invalid, label %slow, label %ok
ok:
  %offset = extractvalue { i64, i1 } %pair, 0
  %result = getelementptr inbounds i8, ptr %base, i64 %offset
  ret ptr %result
slow:
  tail call ptr @sol_slice_index_slow(ptr %base, i64 %index, i64 %len, i64 %size)
  unreachable
}

define weak_odr ptr @sol_slice_range(ptr %base, i64 %start, i64 %end, i64 %len, i64 %size) alwaysinline uwtable {
entry:
  %reversed = icmp ugt i64 %start, %end
  %bounds = icmp ugt i64 %end, %len
  %range = or i1 %reversed, %bounds
  %pair = call { i64, i1 } @llvm.umul.with.overflow.i64(i64 %start, i64 %size)
  %overflow = extractvalue { i64, i1 } %pair, 1
  %invalid = or i1 %range, %overflow
  br i1 %invalid, label %slow, label %ok
ok:
  %offset = extractvalue { i64, i1 } %pair, 0
  %result = getelementptr inbounds i8, ptr %base, i64 %offset
  ret ptr %result
slow:
  tail call ptr @sol_slice_range_slow(ptr %base, i64 %start, i64 %end, i64 %len, i64 %size)
  unreachable
}

define weak_odr ptr @sol_null_check(ptr %value) alwaysinline uwtable {
entry:
  %null = icmp eq ptr %value, null
  br i1 %null, label %slow, label %ok
ok:
  ret ptr %value
slow:
  tail call ptr @sol_null_check_slow(ptr %value)
  unreachable
}

define weak_odr void @sol_assert_array_len(i64 %actual, i64 %expected) alwaysinline uwtable {
entry:
  %equal = icmp eq i64 %actual, %expected
  br i1 %equal, label %done, label %slow
slow:
  tail call void @sol_assert_array_len_slow(i64 %actual, i64 %expected)
  unreachable
done:
  ret void
}

define weak_odr void @sol_carrying_mul_add(i64 %a, i64 %b, i64 %carry, i64 %add, ptr %lo, ptr %hi) alwaysinline nounwind {
  %aa = zext i64 %a to i128
  %bb = zext i64 %b to i128
  %cc = zext i64 %carry to i128
  %dd = zext i64 %add to i128
  %product = mul i128 %aa, %bb
  %carried = add i128 %product, %cc
  %result = add i128 %carried, %dd
  %low = trunc i128 %result to i64
  %shifted = lshr i128 %result, 64
  %high = trunc i128 %shifted to i64
  store i64 %low, ptr %lo, align 8
  store i64 %high, ptr %hi, align 8
  ret void
}

; Keep these definitions until the post-optimization write-barrier pass inserts
; calls. The final object compilation inlines the marking-state test.
@SOL_CONCURRENT_MARKING = external global i8
declare void @sol_write_barrier_slow(ptr, ptr) nounwind
declare void @sol_gc_memcpy_barrier_slow(ptr, i64) nounwind

define weak_odr void @sol_write_barrier(ptr %dst, ptr %value) alwaysinline nounwind {
entry:
  %marking = load atomic i8, ptr @SOL_CONCURRENT_MARKING monotonic, align 1
  %active = icmp ne i8 %marking, 0
  br i1 %active, label %slow, label %done
slow:
  tail call void @sol_write_barrier_slow(ptr %dst, ptr %value)
  br label %done
done:
  ret void
}

define weak_odr void @sol_gc_memcpy_barrier(ptr %dst, i64 %size) alwaysinline nounwind {
entry:
  %marking = load atomic i8, ptr @SOL_CONCURRENT_MARKING monotonic, align 1
  %active = icmp ne i8 %marking, 0
  br i1 %active, label %slow, label %done
slow:
  tail call void @sol_gc_memcpy_barrier_slow(ptr %dst, i64 %size)
  br label %done
done:
  ret void
}
