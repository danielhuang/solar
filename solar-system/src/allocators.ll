; Allocator contracts live on wrappers, independently of rustc's IR spelling.
; Do not mark allocations zeroed: generated code explicitly initializes them.
; Keep wrappers opaque to preserve allocation elision and GC metadata through
; optimization. The native runtime implements allocation and size-class logic.
target triple = "x86_64-unknown-linux-gnu"

declare ptr @sol_alloc_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_0_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_0(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_0_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_1_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_1(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_1_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_2_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_2(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_2_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_3_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_3(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_3_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_4_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_4(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_4_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_5_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_5(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_5_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_6_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_6(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_6_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_7_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_7(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_7_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_8_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_8(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_8_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_9_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_9(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_9_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_10_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_10(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_10_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_11_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_11(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_11_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_12_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_12(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_12_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_13_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_13(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_13_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_14_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_14(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_14_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_15_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_15(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_15_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_16_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_16(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_16_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_17_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_17(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_17_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_18_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_18(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_18_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_19_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_19(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_19_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_20_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_20(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_20_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_21_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_21(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_21_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_22_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_22(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_22_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_23_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_23(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_23_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_24_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_24(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_24_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_25_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_25(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_25_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_26_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_26(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_26_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}

declare ptr @sol_alloc_class_27_impl(i64, i64, ptr) nounwind

define weak_odr noalias ptr @sol_alloc_class_27(i64 %size, i64 allocalign %align, ptr %mark) noinline nounwind allocsize(0) allockind("alloc,aligned") {
  %result = tail call ptr @sol_alloc_class_27_impl(i64 %size, i64 %align, ptr %mark)
  ret ptr %result
}
