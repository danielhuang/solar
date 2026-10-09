//! Only allocations executed on every loop iteration justify forced unrolling.

use std::process::Command;

#[test]
fn allocation_unrolling_requires_an_unconditional_direct_allocation() {
    let directory = tempdir::TempDir::new("solar-unroll").unwrap();
    let allocation = "%object = call ptr @sol_alloc_class_0(i64 8, i64 8, ptr null)";
    for (name, preheader, body, tail, eligible) in [
        ("unconditional", "", allocation, "br label %latch", true),
        (
            "generic",
            "",
            "%object = call ptr @sol_alloc(i64 %n, i64 8, ptr null)",
            "br label %latch",
            true,
        ),
        ("no_allocation", "", "", "br label %latch", false),
        ("preheader_only", allocation, "", "br label %latch", false),
        (
            "conditional",
            "",
            "br i1 %flag, label %allocate, label %latch\nallocate:\n%object = call ptr @sol_alloc_class_0(i64 8, i64 8, ptr null)",
            "br label %latch",
            false,
        ),
        (
            "early_exit",
            "",
            "br i1 %flag, label %exit, label %allocate\nallocate:\n%object = call ptr @sol_alloc_class_0(i64 8, i64 8, ptr null)",
            "br label %latch",
            false,
        ),
        (
            "nested",
            "",
            "br label %inner\ninner:\n%j = phi i64 [0, %body], [%nextj, %allocate]\n%more = icmp ult i64 %j, %n\nbr i1 %more, label %allocate, label %latch\nallocate:\n%object = call ptr @sol_alloc_class_0(i64 8, i64 8, ptr null)\n%nextj = add i64 %j, 1",
            "br label %inner",
            false,
        ),
    ] {
        let input = directory.path().join("input.ll");
        let output = directory.path().join("output.ll");
        std::fs::write(
            &input,
            format!(
                r#"
declare ptr @sol_alloc_class_0(i64, i64, ptr)
declare ptr @sol_alloc(i64, i64, ptr)
define void @solar_example(i64 %n, i1 %flag, ptr %out) {{
entry:
  {preheader}
  br label %header
header:
  %i = phi i64 [0, %entry], [%next, %latch]
  %continue = icmp ult i64 %i, %n
  br i1 %continue, label %body, label %exit
body:
  {body}
  {tail}
latch:
  store volatile i64 %i, ptr %out
  %next = add i64 %i, 1
  br label %header
exit:
  ret void
}}
"#
            ),
        )
        .unwrap();
        let result = Command::new("opt")
            .arg(format!("-load-pass-plugin={}", env!("SOLAR_WB_PLUGIN")))
            .args([
                "-passes=function(loop-simplify),solar-allocation-unroll,verify",
                "-S",
            ])
            .arg(&input)
            .arg("-o")
            .arg(&output)
            .output()
            .unwrap();
        assert!(result.status.success(), "{name}: {result:?}");
        let ir = std::fs::read_to_string(&output).unwrap();
        let latch = ir
            .split("latch:")
            .nth(1)
            .unwrap()
            .split("exit:")
            .next()
            .unwrap();
        let id = latch
            .split("!llvm.loop ")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        let metadata = ir
            .lines()
            .find(|line| line.starts_with(&format!("{id} =")))
            .unwrap();
        let hint_id = metadata.rsplit(", ").next().unwrap().trim_end_matches('}');
        let hint = ir
            .lines()
            .find(|line| line.starts_with(&format!("{hint_id} =")))
            .unwrap();
        assert!(
            hint.contains(if eligible {
                "llvm.loop.unroll.count"
            } else {
                "llvm.loop.unroll.disable"
            }),
            "{name}: {ir}"
        );
        if name == "nested" {
            assert!(ir.contains("!{!\"llvm.loop.unroll.count\", i32 8}"), "{ir}");
        }
        // Exercise LLVM's actual unroller, not only our metadata selection.
        let result = Command::new("opt")
            .args(["-passes=function(loop-unroll),verify", "-S"])
            .arg(&output)
            .arg("-o")
            .arg(&input)
            .output()
            .unwrap();
        assert!(result.status.success(), "{name}: {result:?}");
        let unrolled = std::fs::read_to_string(&input).unwrap();
        let calls = unrolled.matches("call ptr @sol_alloc").count();
        if eligible || name == "nested" {
            assert!(calls >= 8, "{name}: {unrolled}");
        } else {
            assert_eq!(
                calls,
                usize::from(name != "no_allocation"),
                "{name}: {unrolled}"
            );
        }
    }
}
