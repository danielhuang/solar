target triple = "x86_64-unknown-linux-gnu"

declare ptr @sol_alloc(i64, i64, ptr)
declare ptr @sol_alloc_class_4_impl(i64, i64, ptr)
declare { i64, i64 } @sol_alloc_class_4_batch2(i64, i64, ptr)
declare ptr @sol_alloc_class_4_batch3_view(i64, i64, ptr)
declare void @llvm.memset.p0.i64(ptr, i8, i64, i1)
declare void @mark(ptr, ptr, i64)

define internal ptr @sol_alloc_class_4.constprop.0() {
  %result = call ptr @sol_alloc_class_4_impl(i64 128, i64 16, ptr @mark)
  ret ptr %result
}

define void @solar_publish(ptr %out, i1 %branch) {
entry:
  %a = call ptr @sol_alloc(i64 128, i64 16, ptr @mark)
  %specialized = call ptr @sol_alloc_class_4.constprop.0()
  %pair = call { i64, i64 } @sol_alloc_class_4_batch2(i64 128, i64 16, ptr @mark)
  %pair0.int = extractvalue { i64, i64 } %pair, 0
  %pair1.int = extractvalue { i64, i64 } %pair, 1
  %pair0 = inttoptr i64 %pair0.int to ptr
  %pair1 = inttoptr i64 %pair1.int to ptr
  %view = call ptr @sol_alloc_class_4_batch3_view(i64 128, i64 16, ptr @mark)
  %slot0 = getelementptr [3 x ptr], ptr %view, i32 0, i32 0
  %slot1 = getelementptr [3 x ptr], ptr %view, i32 0, i32 1
  %slot2 = getelementptr [3 x ptr], ptr %view, i32 0, i32 2
  %view0 = load volatile ptr, ptr %slot0
  %view1 = load volatile ptr, ptr %slot1
  %view2 = load volatile ptr, ptr %slot2
  store i64 0, ptr %a
  store i64 0, ptr %specialized
  store i64 0, ptr %pair0
  store i64 0, ptr %pair1
  store i64 0, ptr %view0
  store i64 0, ptr %view1
  store i64 0, ptr %view2
  br i1 %branch, label %left, label %right

left:
  store i64 1, ptr %a
  br label %end

right:
  store i64 2, ptr %a
  br label %end

end:
  store ptr %a, ptr %out
  ret void
}

define void @solar_debug(ptr %out) {
entry:
  %slot = alloca ptr
  %value = call ptr @sol_alloc(i64 128, i64 16, ptr @mark)
  store ptr %value, ptr %slot
  %reload = load ptr, ptr %slot
  call void @llvm.memset.p0.i64(ptr %reload, i8 0, i64 128, i1 false)
  store ptr %reload, ptr %out
  ret void
}
