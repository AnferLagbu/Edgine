// SPDX-License-Identifier: MPL-2.0
// KPTI-18a 回归测试: aarch64 上下文切换的 FPCR/FPSR 落点.
//
// 缺陷: `context_switch_asm` 曾把 FPCR/FPSR 存到 offset 640/648 —— 那是
// `fpu_state` 中 q31 的高 16 字节 (q31 = 640..656), 造成双向污染:
//   保存侧: FPCR/FPSR 覆盖 V31 的高 16 字节
//   恢复侧: 把 V31 残值当 FPCR/FPSR 写回 (msr fpcr/fpsr)
//
// 权威布局 (`ProcessContext`, 见 src/kernel/framework/proc/types.rs):
//   fpu_state[64] @ 144..656 / fpcr @ 656 / fpsr @ 664 / extra_regs @ 672
//
// 验收: 保存与恢复两侧必须使用 656/664, 且汇编中不得出现 640/648.

use std::fs;

const CTX: &str = "../src/kernel/framework/arch/aarch64/context.rs";

#[test]
fn test_aarch64_fpcr_fpsr_use_dedicated_fields() {
    let src = fs::read_to_string(CTX).unwrap_or_else(|e| panic!("read {CTX}: {e}"));
    let start = src.find("global_asm!").expect("必须存在 global_asm!");
    let asm = &src[start..];

    // 保存侧 (x0 = prev): fpcr@656 / fpsr@664
    assert!(
        asm.contains("[x0, #656]") && asm.contains("[x0, #664]"),
        "保存侧必须落在 fpcr(@656) / fpsr(@664)"
    );
    // 恢复侧 (x1 = next): 同一落点
    assert!(
        asm.contains("[x1, #656]") && asm.contains("[x1, #664]"),
        "恢复侧必须读 fpcr(@656) / fpsr(@664)"
    );
    // 禁止回退到 fpu_state[62]/[63]
    assert!(
        !asm.contains("#640]") && !asm.contains("#648]"),
        "不得使用 640/648: 那是 q31 的高 16 字节, 会双向污染 V31 与 FPCR/FPSR"
    );
}
