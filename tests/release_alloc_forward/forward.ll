target datalayout = "e-p:64:64-i64:64"

declare ptr @sol_alloc(i64, i64, ptr)
declare ptr @sol_alloc_class_1(i64, i64, ptr)
declare ptr @sol_alloc_class_1_impl(i64, i64, ptr)
declare void @observe(ptr)
declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1 immarg)
declare void @llvm.memmove.p0.p0.i64(ptr, ptr, i64, i1 immarg)
declare void @llvm.memset.p0.i64(ptr, i8, i64, i1 immarg)

; Preserve write order and leave destination observations before the copy alone.
; The source alignment and alias metadata do not apply to the destination.
define ptr @solar_stores(i64 %x, i64 %y) {
  %temporary = call ptr @sol_alloc(i64 16, i64 16, ptr null)
  store i64 %x, ptr %temporary, align 16, !alias.scope !0
  store i64 %y, ptr %temporary, align 16
  %tail = getelementptr i8, ptr %temporary, i64 8
  store i64 99, ptr %tail, align 8
  %destination = call ptr @sol_alloc(i64 24, i64 1, ptr null)
  store i8 0, ptr %destination
  call void @observe(ptr %destination)
  %offset = getelementptr i8, ptr %destination, i64 1
  call void @llvm.memcpy.p0.p0.i64(ptr %offset, ptr %temporary, i64 16, i1 false)
  ret ptr %destination
}

; to_string's unused prefix must disappear without writing outside the copied
; slice, and initialization of the destination must precede the forwarded byte.
define ptr @solar_slice() {
  %temporary = call ptr @sol_alloc(i64 20, i64 1, ptr null)
  call void @llvm.memset.p0.i64(ptr %temporary, i8 0, i64 19, i1 false)
  %digit = getelementptr i8, ptr %temporary, i64 19
  store i8 48, ptr %digit
  %destination = call ptr @sol_alloc(i64 2, i64 1, ptr null)
  call void @llvm.memset.p0.i64(ptr %destination, i8 0, i64 2, i1 false)
  call void @llvm.memmove.p0.p0.i64(ptr %destination, ptr %digit, i64 1, i1 false)
  %newline = getelementptr i8, ptr %destination, i64 1
  store i8 10, ptr %newline
  ret ptr %destination
}

define ptr @solar_memset() {
  %temporary = call ptr @sol_alloc(i64 16, i64 16, ptr null)
  call void @llvm.memset.p0.i64(ptr align 16 %temporary, i8 7, i64 16, i1 false)
  %destination = call ptr @sol_alloc(i64 32, i64 1, ptr null)
  %offset = getelementptr i8, ptr %destination, i64 8
  call void @llvm.memmove.p0.p0.i64(ptr %offset, ptr %temporary, i64 16, i1 false)
  ret ptr %destination
}

define internal ptr @sol_alloc_class_1.constprop.0() {
  %result = call ptr @sol_alloc_class_1_impl(i64 16, i64 16, ptr null)
  ret ptr %result
}

define ptr @solar_specialized(i64 %x) {
  %temporary = call ptr @sol_alloc_class_1.constprop.0()
  store i64 %x, ptr %temporary
  %tail = getelementptr i8, ptr %temporary, i64 8
  store i64 0, ptr %tail
  %destination = call ptr @sol_alloc_class_1(i64 16, i64 16, ptr null)
  call void @llvm.memcpy.p0.p0.i64(ptr %destination, ptr %temporary, i64 16, i1 false)
  ret ptr %destination
}

!0 = !{!1}
!1 = distinct !{!1, !2, !"temporary"}
!2 = distinct !{!2, !"domain"}
