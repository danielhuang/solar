target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-linux-gnu"

define void @sol_atomic_store_128(ptr %dst, ptr %src) #0 {
  %val = load i128, ptr %src, align 8
  store atomic i128 %val, ptr %dst seq_cst, align 16
  ret void
}

define void @sol_atomic_load_128(ptr %dst, ptr %src) #0 {
  %val = load atomic i128, ptr %src seq_cst, align 16
  store i128 %val, ptr %dst, align 8
  ret void
}

define void @sol_atomic_copy_128(ptr %dst, ptr %src) #0 {
  %val = load atomic i128, ptr %src seq_cst, align 16
  store atomic i128 %val, ptr %dst seq_cst, align 16
  ret void
}

define void @sol_atomic_load_128_acq(ptr %dst, ptr %src) #0 {
  %val = load atomic i128, ptr %src acquire, align 16
  store i128 %val, ptr %dst, align 8
  ret void
}

define void @sol_atomic_store_128_rel(ptr %dst, ptr %src) #0 {
  %val = load i128, ptr %src, align 8
  store atomic i128 %val, ptr %dst release, align 16
  ret void
}

define void @sol_atomic_compare_exchange_128_acq_rel(ptr %dst, ptr %ref, ptr %expected, ptr %new_val) #0 {
  %exp_val = load i128, ptr %expected, align 8
  %new_v = load i128, ptr %new_val, align 8
  %result = cmpxchg ptr %ref, i128 %exp_val, i128 %new_v acq_rel acquire, align 16
  %old_val = extractvalue { i128, i1 } %result, 0
  store i128 %old_val, ptr %dst, align 8
  ret void
}

attributes #0 = { "target-features"="+cx16" }
