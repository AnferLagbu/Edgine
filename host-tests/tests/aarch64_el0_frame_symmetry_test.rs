// SPDX-License-Identifier: MPL-2.0
// aarch64 EL0 异常帧"压帧 / 恢复"对称性回归测试.
//
// 缺陷 1 (既有, 本轮修复): `handle_el0_irq` 压帧只覆盖 x0-x19/x30, 而共用的
//   `el0_return` 会从帧内恢复 x0-x29 ⇒ EL0 期中断返回后, 用户 x20-x29 被内核栈
//   残留值覆盖 (该路径在既有用例中未触发, 属潜在缺陷). 修复: 与 `handle_el0_sync`
//   同集压帧.
//
// 契约 2 (L1-03b): 出入口改为「先切 TTBR 再压/读帧」后内核栈在切表后不可达, 故
//   x3/x4 (切表序列的 scratch) 必须经 KPTI 暂存槽 (偏移 8/16) 中转,
//   且 `el0_return` 不得再从帧内直接恢复 x3/x4. 同时入口无空闲 GPR, 必须借
//   `TPIDRRO_EL0` 中转自举.
//
// 契约 3 (KPTI-PCPU-01, DECISION-084): 暂存槽与 user_ttbr0 属**每核**活跃值,
//   置于按核数组 `KPTI_CPU_GLOBALS`, 槽基址由 `TPIDR_EL1` 提供 (上电路径按核绑定);
//   汇编不得再以 `{kpti_globals}` + 偏移 24/40/48 访问活跃值 (单实例会在双核并发
//   EL0 时跨核覆盖). boot 期只读字段 (tramp_ttbr1 偏移 0 / kernel_ttbr1 偏移 8 /
//   kernel_ttbr0 偏移 16) 仍走 `{kpti_globals}`.
//
// 运行期判据仍由 QEMU 承担 (EL0 → SVC → `el0_return` 往返, 见 qemu_boot_test.sh);
// 本文件只锁定装配面契约, 防回归.

use std::fs;

const EXC: &str = "../src/kernel/privileged/arch/aarch64/exception.rs";

fn read() -> String {
    fs::read_to_string(EXC).unwrap_or_else(|e| panic!("read {EXC}: {e}"))
}

/// 压缩连续空白为单空格, 使断言不受列对齐影响.
fn norm(src: &str) -> String {
    src.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 取从 `start` 到 `end` 之间的源码 (不含 `end` 所在位置之后的内容).
fn slice_between(src: &str, start: &str, end: &str) -> String {
    let i = src
        .find(start)
        .unwrap_or_else(|| panic!("未找到 {start:?}"));
    let rest = &src[i..];
    let j = rest.find(end).unwrap_or_else(|| panic!("未找到 {end:?}"));
    rest[..j].to_string()
}

/// 抽出入口的压帧指令段 (从入口标号到 `mrs x0, elr_el1` 之前).
fn frame_push(src: &str, entry: &str) -> String {
    norm(&slice_between(src, entry, "mrs  x0, elr_el1"))
}

/// 1. 两个 EL0 入口必须压出**同一集合**的寄存器 (x0-x29 按 stp 对 + x30 单存).
#[test]
fn test_el0_entries_push_same_frame() {
    let src = read();
    for entry in ["handle_el0_sync:", "handle_el0_irq:"] {
        let push = frame_push(&src, entry);
        for n in (0..=28).step_by(2) {
            let want = norm(&format!("stp x{n}, x{}, [sp, #(8 * {n})]", n + 1));
            assert!(
                push.contains(&want),
                "{entry} 必须压入 {want} —— 否则 el0_return 会从帧内读到栈残留值"
            );
        }
        assert!(
            push.contains(&norm("str x30, [sp, #(8 * 30)]")),
            "{entry} 必须压入 x30"
        );
        assert_eq!(
            push.matches("[sp, #(8 *").count(),
            16,
            "{entry} 的压帧引用数须为 16 (15 对 stp + x30), 实得 {}",
            push.matches("[sp, #(8 *").count()
        );
    }
}

/// 2. L1-03b + KPTI-PCPU-01 入口契约: 借 `TPIDRRO_EL0` 中转 x3, 以 `TPIDR_EL1`
///    定位本核槽, 经暂存槽 (8/16) 保住 x3/x4, 且在**切表之后**才压帧.
#[test]
fn test_el0_entries_relay_via_tpidrro_and_stash() {
    let src = read();
    for entry in ["handle_el0_sync:", "handle_el0_irq:"] {
        let window = norm(&slice_between(&src, entry, "mov  x0, sp"));
        assert!(
            window.contains(&norm("msr tpidrro_el0, x3")),
            "{entry} 必须借 TPIDRRO_EL0 中转用户 x3 (入口无空闲 GPR)"
        );
        assert!(
            window.contains(&norm("mrs x3, tpidr_el1")),
            "{entry} 必须以 TPIDR_EL1 取本核 KPTI 槽基址 (KPTI-PCPU-01)"
        );
        assert!(
            window.contains(&norm("str x4, [x3, #8]"))
                && window.contains(&norm("str x4, [x3, #16]")),
            "{entry} 必须把用户 x3/x4 存入本核槽暂存槽 (偏移 8/16)"
        );
        assert!(
            window.contains(&norm("ldr x4, [x3, #8]"))
                && window.contains(&norm("ldr x3, [x3, #16]")),
            "{entry} 必须在切表后从本核槽取回用户 x3/x4"
        );
        assert!(
            !window.contains(&norm("str x4, [x3, #40]"))
                && !window.contains(&norm("str x4, [x3, #48]")),
            "{entry} 不得再用单实例 KPTI_GLOBALS 暂存槽 (偏移 40/48): 双核并发会跨核覆盖"
        );
        // 先切 TTBR 再压帧: 压帧引用必须晚于 ttbr1_el1 切换.
        let i_ttbr1 = window
            .find("msr ttbr1_el1, x4")
            .unwrap_or_else(|| panic!("{entry} 未切 TTBR1"));
        let i_push = window
            .find("sub sp, sp, #(8 * 35)")
            .unwrap_or_else(|| panic!("{entry} 未压帧"));
        assert!(
            i_ttbr1 < i_push,
            "{entry} 必须先切 TTBR1 再压帧 (切表前内核栈不可达)"
        );
    }
}

/// 3. `el0_return` 出口契约: 以 `TPIDR_EL1` 定位本核槽, 先把帧内 x3/x4 搬进
///    暂存槽 (偏移 8/16), 切表后从暂存槽取回; 不得再从帧内直接恢复 x3/x4
///    (会被切表序列的 scratch 值覆盖).
#[test]
fn test_el0_return_restores_x3_x4_from_stash() {
    let src = read();
    let ret = norm(&slice_between(&src, "el0_return:", "eret"));
    assert!(
        ret.contains(&norm("mrs x3, tpidr_el1")),
        "el0_return 必须以 TPIDR_EL1 取本核 KPTI 槽基址 (KPTI-PCPU-01)"
    );
    assert!(
        ret.contains(&norm("str x4, [x3, #8]")) && ret.contains(&norm("str x4, [x3, #16]")),
        "el0_return 必须先把帧内 x4/x3 存入本核槽暂存槽 (切表后内核栈不可达)"
    );
    assert!(
        ret.contains(&norm("ldr x4, [x3, #8]")) && ret.contains(&norm("ldr x3, [x3, #16]")),
        "el0_return 必须在切表后从本核槽暂存槽取回 x3/x4"
    );
    for forbidden in ["ldp x2, x3, [sp, #(8 * 2)]", "ldp x4, x5, [sp, #(8 * 4)]"] {
        assert!(
            !ret.contains(&norm(forbidden)),
            "el0_return 不得用 {:?} 直接恢复 x3/x4 (切表后内核栈不可达)",
            forbidden
        );
    }
    // 帧内 x3/x4 的暂存必须早于 x0-x29 的恢复.
    let i_stash = ret
        .find("[x3, #16]")
        .expect("el0_return 未使用本核槽暂存槽 #16");
    let i_restore = ret
        .find("ldr x5, [sp, #(8 * 5)]")
        .expect("el0_return 未恢复 x5");
    assert!(
        i_stash < i_restore,
        "el0_return 必须先暂存 x3/x4 再恢复其余寄存器"
    );
}
