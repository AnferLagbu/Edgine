// SPDX-License-Identifier: MPL-2.0
// ISSUE-RT-005 回归测试: aarch64 EL0 中断可达 (用户态可被抢占).
//
// 缺陷 (既有, 本轮修复): `enter_user` 把 `SPSR_EL1` 写成 `0x3C0` (EL0t + DAIF
//   全屏蔽, **I=1**) ⇒ EL0 全程屏蔽 IRQ, EL0 IRQ 向量入口 (`handle_el0_irq`)
//   运行期不可达; 用户态既无定时器中断也无跨核 SGI, 与 x86_64 用户态
//   `RFLAGS.IF=1` 语义不一致 (无时间片抢占)。
// 修复: 进入 / 恢复 EL0 的 PSTATE 清 I 位 (`0x340`), 使 EL0 可收 IRQ 被抢占。
//
// 契约 (本轮锁定):
//   1. `enter_user` 写入的 EL0 `SPSR_EL1` 为 `0x340` (M[3:0]==0 且 I 位==0),
//      即与 `0x3C0` 相比只清 I 位;
//   2. x86_64 用户态 iretq 帧 `push 0x202` (IF=1) 为跨架构对齐基准 (IF = bit9);
//   3. 调度恢复的 EL0 首次进入路径从 ctx `@112` 恢复 SPSR (不硬编码低值),
//      值来源为 `proc_save_user_regs_aarch64` 写入的硬件 `f.spsr`;
//   4. 三个 EL0 PSTATE 相关源文件不得再出现 `0x3C0`;
//   5. EL0 来源 IRQ 里程碑字符串存在 (QEMU fail-closed 断言的锚点)。
//
// 运行期判据仍由 QEMU 承担 (观察到 `IRQ: delivered from EL0`, 见
// qemu_boot_test.sh aarch64 分支); 本文件只锁定装配面契约, 防回归。

use std::fs;

const A64_MOD: &str = "../src/kernel/privileged/arch/aarch64/mod.rs";
const A64_CTX: &str = "../src/kernel/privileged/arch/aarch64/context.rs";
const A64_EXC: &str = "../src/kernel/privileged/arch/aarch64/exception.rs";
const A64_PROC: &str = "../src/kernel/privileged/proc/proc_ops.rs";
const X86_MOD: &str = "../src/kernel/privileged/arch/x86_64/mod.rs";

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// 压缩连续空白为单空格, 使断言不受列对齐影响.
fn norm(src: &str) -> String {
    src.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 取从 `start` 到 `end` 之间的源码 (不含 `end`)。
fn slice_between(src: &str, start: &str, end: &str) -> String {
    let i = src
        .find(start)
        .unwrap_or_else(|| panic!("未找到 {start:?}"));
    let rest = &src[i..];
    let j = rest.find(end).unwrap_or_else(|| panic!("未找到 {end:?}"));
    rest[..j].to_string()
}

/// 1. `enter_user` 的 EL0 SPSR 必须清 I 位 (0x340), 且语义为 EL0t。
#[test]
fn test_enter_user_el0_spsr_has_irq_unmasked() {
    let src = read(A64_MOD);
    let body = slice_between(&src, "fn enter_user", "unsafe {");
    assert!(
        body.contains("let spsr: u64 = 0x340;"),
        "enter_user 必须以 0x340 进 EL0 (清 I 位 ⇒ EL0 可被抢占); 见 ISSUE-RT-005"
    );
    // 位域自检: EL0t (M[3:0]==0) 且 I(bit7)==0; D/A/F 仍屏蔽。
    assert_eq!(0x340u64 & 0xF, 0, "0x340 必须为 EL0t (M[3:0]==0)");
    assert_eq!(0x340u64 & (1 << 7), 0, "0x340 必须清 I 位 (bit7)");
    assert_ne!(0x340u64 & (1 << 9), 0, "0x340 保留 D 屏蔽 (bit9)");
    assert_ne!(0x340u64 & (1 << 8), 0, "0x340 保留 A 屏蔽 (bit8)");
    assert_ne!(0x340u64 & (1 << 6), 0, "0x340 保留 F 屏蔽 (bit6)");
    // 与修复前的 0x3C0 相比, 差异必须恰为 I 位 (0x80) 被清除。
    assert_eq!(
        0x3C0u64 ^ 0x340u64,
        1 << 7,
        "0x3C0 → 0x340 的唯一差异必须是被清除的 I 位"
    );
}

/// 2. x86_64 用户态 iretq 帧 `push 0x202` (IF=1) 是跨架构对齐基准。
#[test]
fn test_x86_64_user_rflags_enables_interrupts() {
    let src = read(X86_MOD);
    assert!(
        src.contains("push 0x202"),
        "x86_64 用户态 iretq 帧必须 push 0x202 (RFLAGS.IF=1)"
    );
    assert_ne!(0x202u64 & (1 << 9), 0, "0x202 必须置 IF (bit9)");
}

/// 3. 调度恢复的 EL0 首次进入路径从 ctx @112 恢复 SPSR (不硬编码 EL0 常量)。
#[test]
fn test_ctx_el0_entry_restores_spsr_from_ctx() {
    let src = read(A64_CTX);
    let el0 = norm(&slice_between(&src, ".Lctx_enter_el0:", "br   x11"));
    assert!(
        el0.contains(&norm("ldr x2, [x1, #112]")) && el0.contains(&norm("msr spsr_el1, x2")),
        "EL0 首次进入路径必须从 ctx @112 恢复 SPSR (来源为硬件 f.spsr, 而非硬编码)"
    );
    assert!(
        !el0.contains("0x3C0"),
        "EL0 首次进入路径不得硬编码 0x3C0 (会再度屏蔽 EL0 中断)"
    );
}

/// 4. EL0 PSTATE 传播链路: `proc_save_user_regs_aarch64` 把硬件 `f.spsr` 写入 ctx.fs。
#[test]
fn test_proc_save_user_regs_propagates_hw_spsr() {
    let src = read(A64_PROC);
    let f = slice_between(&src, "pub fn proc_save_user_regs_aarch64", "\n}");
    assert!(
        f.contains("ctx.fs = f.spsr"),
        "proc_save_user_regs_aarch64 必须把硬件 f.spsr 传播到 ctx.fs (EL0 PSTATE 来源)"
    );
}

/// 5. 三个 EL0 PSTATE 相关源文件不得再残留 0x3C0 (回归锁)。
#[test]
fn test_no_legacy_irq_masked_el0_pstate_remains() {
    for path in [A64_MOD, A64_CTX, A64_PROC] {
        let src = read(path);
        assert!(
            !src.contains("0x3C0"),
            "{path} 不得再出现 0x3C0 (EL0 全程屏蔽 IRQ 的旧值); 见 ISSUE-RT-005"
        );
    }
}

/// 6. EL0 来源 IRQ 里程碑字符串存在 (QEMU fail-closed 断言的锚点)。
#[test]
fn test_el0_irq_milestone_present() {
    let src = read(A64_EXC);
    assert!(
        src.contains("IRQ: delivered from EL0"),
        "handle_irq 必须保留 EL0 来源 IRQ 里程碑 (QEMU 断言锚点, 防断言漂移)"
    );
}
