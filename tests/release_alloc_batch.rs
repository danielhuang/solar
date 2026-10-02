//! Batched allocation lowering and retained object graphs across collection.

use solar::pipeline::CompileOptions;
use std::process::Command;

#[test]
fn batches_only_equal_allocations_without_intervening_publication_or_calls() {
    let directory = tempdir::TempDir::new("solar-test").unwrap();
    for (name, separator, size, expected4, expected2, expected3, scalar) in [
        ("initialization", "store ptr %a, ptr %b", 8, 1, 0, 0, 0),
        ("publication", "store ptr %b, ptr %out", 8, 0, 2, 0, 0),
        ("call", "call void @escape(ptr %b)", 8, 0, 2, 0, 0),
        ("volatile", "store volatile ptr %a, ptr %b", 8, 0, 2, 0, 0),
        ("different_size", "store ptr %a, ptr %b", 7, 0, 0, 1, 1),
    ] {
        let input = directory.path().join(format!("{name}.ll"));
        let output = directory.path().join(format!("{name}-out.ll"));
        std::fs::write(
            &input,
            format!(
                r#"
declare ptr @sol_alloc_class_0(i64, i64, ptr)
declare void @escape(ptr)
define ptr @solar_example(ptr %out) {{
  %a = call ptr @sol_alloc_class_0(i64 8, i64 8, ptr null)
  %b = call ptr @sol_alloc_class_0(i64 8, i64 8, ptr null)
  {separator}
  %c = call ptr @sol_alloc_class_0(i64 {size}, i64 8, ptr null)
  store ptr %b, ptr %c
  %d = call ptr @sol_alloc_class_0(i64 8, i64 8, ptr null)
  store ptr %c, ptr %d
  ret ptr %d
}}
"#
            ),
        )
        .unwrap();
        let result = Command::new("opt")
            .arg(format!("-load-pass-plugin={}", env!("SOLAR_WB_PLUGIN")))
            .args(["-passes=solar-batch-gc-alloc,verify", "-S"])
            .arg(input)
            .arg("-o")
            .arg(&output)
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        let ir = std::fs::read_to_string(output).unwrap();
        assert_eq!(
            ir.matches("call void @sol_alloc_class_0_batch4").count(),
            expected4,
            "{name}: {ir}"
        );
        assert_eq!(
            ir.matches("call { i64, i64 } @sol_alloc_class_0_batch2")
                .count(),
            expected2,
            "{name}: {ir}"
        );
        assert_eq!(
            ir.matches("call void @sol_alloc_class_0_batch3").count(),
            expected3,
            "{name}: {ir}"
        );
        assert_eq!(ir.matches("call ptr @sol_alloc_class_0").count(), scalar);
    }
}

#[test]
fn splits_groups_at_eight_and_preserves_single_leftovers() {
    let directory = tempdir::TempDir::new("solar-test").unwrap();
    for count in 1..=19 {
        let input = directory.path().join("input.ll");
        let output = directory.path().join("output.ll");
        let mut ir = String::from(
            "declare ptr @sol_alloc_class_0(i64, i64, ptr)\ndefine ptr @solar_example() {\n",
        );
        for n in 0..count {
            ir.push_str(&format!(
                "%a{n} = call ptr @sol_alloc_class_0(i64 8, i64 8, ptr null)\n"
            ));
        }
        ir.push_str(&format!("ret ptr %a{}\n}}", count - 1));
        std::fs::write(&input, ir).unwrap();
        let result = Command::new("opt")
            .arg(format!("-load-pass-plugin={}", env!("SOLAR_WB_PLUGIN")))
            // Running twice also checks that existing batch calls are ignored.
            .args([
                "-passes=solar-batch-gc-alloc,solar-batch-gc-alloc,verify",
                "-S",
            ])
            .arg(&input)
            .arg("-o")
            .arg(&output)
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        let ir = std::fs::read_to_string(&output).unwrap();
        for batch in 2..=8 {
            let expected = if batch == 8 {
                count / 8
            } else {
                usize::from(count % 8 == batch)
            };
            let calls = ir
                .lines()
                .filter(|line| {
                    line.contains("call ")
                        && line.contains(&format!("@sol_alloc_class_0_batch{batch}("))
                })
                .count();
            assert_eq!(calls, expected, "count={count}, batch={batch}: {ir}");
        }
        assert_eq!(
            ir.matches("call ptr @sol_alloc_class_0(").count(),
            usize::from(count % 8 == 1),
            "{ir}"
        );
    }
}

#[test]
fn unrolled_chain_survives_cache_refills_and_collection() {
    test_utils::ensure_release_runtime_built();
    let directory = tempdir::TempDir::new("solar-test").unwrap();
    // A nonmultiple trip count leaves a conditional allocation in a separate
    // block; only the seven allocations in the straight-line block batch.
    for (count, batch) in [(1000, 8), (1001, 7)] {
        let source = directory.path().join("chain.solar");
        std::fs::write(
            &source,
            r#"
struct Node { next: &?Node }
fn main() {
    for _ in 0..3 {
        let node = Node { next: null#[Node] };
        for _ in 0..COUNT {
            let prev = node;
            node = Node { next: prev& };
        }
        gc::collect_gc();
        let cursor = node.next;
        let count = 0;
        while cursor != null#[Node] {
            count = count + 1;
            cursor = cursor@.next;
        }
        assert(count == COUNT);
    }
    println("batch passed"&);
}
"#
            .replace("COUNT", &count.to_string()),
        )
        .unwrap();
        let binary = solar::pipeline::compile(&source)
            .unwrap()
            .to_mangled()
            .to_tree_ir()
            .optimized()
            .to_c(&source.display().to_string())
            .to_binary(directory.path().join("chain"), CompileOptions::RELEASE);
        // Verify the optimized wrapper (whose constant parameters O3 can remove)
        // was recognized, rather than merely checking a scalar fallback's output.
        let disassembly = Command::new("objdump")
            .args(["-d", "--disassemble=solar_main"])
            .arg(&binary.path)
            .output()
            .unwrap();
        assert!(disassembly.status.success(), "{disassembly:?}");
        assert!(
            String::from_utf8_lossy(&disassembly.stdout)
                .contains(&format!("<sol_alloc_class_0_batch{batch}>")),
            "{}",
            String::from_utf8_lossy(&disassembly.stdout)
        );
        let output = Command::new(binary.path).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"batch passed\n");
    }
}

#[test]
fn batch_runtime_handles_alignment_metadata_classes_and_cache_boundaries() {
    test_utils::ensure_release_runtime_built();
    let directory = tempdir::TempDir::new("solar-test").unwrap();
    let source = directory.path().join("batch.c");
    let binary = directory.path().join("batch");
    let mut c = String::from(
        r#"
#include <assert.h>
#include <stdint.h>
#include <stddef.h>
#include <string.h>
extern void sol_start(void (*)(void*), void*, size_t, void (*)(void));
typedef void (*mark_fn)(void*, void*, uint64_t);
extern void *sol_alloc_impl(size_t, size_t, mark_fn);
const char *sol_payload_type_name(uint64_t tag) { return "Unit"; }
static void mark(void *ctx, void *p, uint64_t size) {}
"#,
    );
    for batch in 1..=8 {
        c.push_str(&r#"typedef struct { void *addresses[BATCH]; } AllocBatchBATCH;
extern AllocBatchBATCH sol_alloc_batchBATCH(size_t, size_t, mark_fn);
extern AllocBatchBATCH sol_alloc_class_0_batchBATCH(size_t, size_t, mark_fn);
extern AllocBatchBATCH sol_alloc_class_1_batchBATCH(size_t, size_t, mark_fn);
extern AllocBatchBATCH sol_alloc_class_2_batchBATCH(size_t, size_t, mark_fn);
extern AllocBatchBATCH sol_alloc_class_3_batchBATCH(size_t, size_t, mark_fn);
extern AllocBatchBATCH sol_alloc_class_4_batchBATCH(size_t, size_t, mark_fn);
extern AllocBatchBATCH sol_alloc_class_5_batchBATCH(size_t, size_t, mark_fn);
typedef AllocBatchBATCH (*batch_fnBATCH)(size_t, size_t, mark_fn);
static batch_fnBATCH class_allocatorsBATCH[] = {sol_alloc_class_0_batchBATCH,sol_alloc_class_1_batchBATCH,sol_alloc_class_2_batchBATCH,sol_alloc_class_3_batchBATCH,sol_alloc_class_4_batchBATCH,sol_alloc_class_5_batchBATCH};
static void checkBATCH(void) {
    const size_t sizes[] = {1, 8, 9, 64, 127, 128, 129, 256};
    for (size_t k = 0; k < sizeof(sizes) / sizeof(sizes[0]); ++k) {
        size_t size = sizes[k], align = k % 2 ? 8 : 32;
        // Misalign the cache cursor so batches must straddle bitmap words.
        void *pointers[(1 + 65 * BATCH)];
        pointers[0] = sol_alloc_impl(size, align, mark);
        memset(pointers[0], 0x5a, size);
        for (size_t n = 1; n < (1 + 65 * BATCH); n += BATCH) {
            size_t class = 0;
            while ((8u << class) < size || (8u << class) < align) ++class;
            AllocBatchBATCH batch = ((n - 1) / BATCH) % 2 == 0
                ? class_allocatorsBATCH[class](size, align, mark)
                : sol_alloc_batchBATCH(size, align, mark);
            memcpy(&pointers[n], batch.addresses, sizeof(batch.addresses));
            for (size_t j = n; j < n + BATCH; ++j) {
                assert((uintptr_t)pointers[j] % align == 0);
                for (size_t i = 0; i < j; ++i)
                    assert(pointers[i] != pointers[j]);
                memset(pointers[j], 0x5a, size);
            }
        }
        for (size_t j = 0; j < (1 + 65 * BATCH); ++j)
            for (size_t i = 0; i < size; ++i)
                assert(((unsigned char*)pointers[j])[i] == 0x5a);
    }
}
"#.replace("BATCH", &batch.to_string()));
    }
    c.push_str("static void body(void *env) {");
    for batch in 1..=8 {
        c.push_str(&format!("check{batch}();"));
    }
    c.push_str("}\nint main(void) { sol_start(body, 0, 0, 0); return 0; }");
    std::fs::write(&source, c).unwrap();
    let output = Command::new("clang")
        .args(["-O3", "-fuse-ld=lld"])
        .arg(source)
        .arg("target/release/libsolar_system.a")
        .args(["-lm", "-lpthread", "-ldl", "-o"])
        .arg(&binary)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let output = Command::new(binary).output().unwrap();
    assert!(output.status.success(), "{output:?}");
}
