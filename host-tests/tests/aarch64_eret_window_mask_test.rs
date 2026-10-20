// SPDX-License-Identifier: MPL-2.0
// aarch64 异常返回序列 ERET-return-window 屏蔽回归测试 (P-32 flaky 死机根因).
//
// 根因 (取证闭环): `handle_el1h_irq` / `handle_el1h_sync` / `el0_return` 的出口
//   序列 `msr elr_el1, x0` → `msr spsr_el1, x1` → … → `eret` 之间, 若 PSTATE.I
//   处于放行态 (do_softirq 开中断 / run_pending_resched 上下文切换往返所致),
//   一条异步异常可在此窗口被硬件取走, 把刚写入的 ELR_EL1/SPSR_EL1 覆写成嵌套
//   现场 ⇒ 外层 `ERET` 返回错误上下文 → illegal ERET 级联 (实测崩溃 PC 恒落此
//   窗口; 16 轮 QEMU silent_dead 形). 修复: 返回序列在写 ELR/SPSR **之前**即
//   `msr daifset, #0xf` 全程屏蔽异步异常, 挂起项留待 `ERET` 依 SPSR 恢复投递.
//
// 装配面契约 (本文件锁定, 防回归): 三处出口从 handler 调用返回点到终止 `eret`
//   之间必须含 `msr daifset, #0xf`, 且其位置早于 `msr elr_el1, x0` —— 保证屏蔽
//   先于返回寄存器建立, 窗口不可被插足. 运行期判据仍由 QEMU 承担.

use std::fs;

const EXC: &str = "../src/kernel/privileged/arch/aarch64/exception.rs";

fn read() -> String {
    fs::read_to_string(EXC).unwrap_or_else(|e| panic!("read {EXC}: {e}"))
}

/// 压缩连续空白为单空格, 使断言不受列对齐影响.
fn norm(src: &str) -> String {
    src.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 取从 `start` 起、到首个 `end` 之前 (不含) 的源码.
fn slice_between(src: &str, start: &str, end: &str) -> String {
    let i = src
        .find(start)
        .unwrap_or_else(|| panic!("未找到 {start:?}"));
    let rest = &src[i..];
    let j = rest.find(end).unwrap_or_else(|| panic!("未找到 {end:?}"));
    rest[..j].to_string()
}

/// 校验单个出口窗口: `msr daifset, #0xf` 必须存在, 且早于 `msr elr_el1, x0`.
fn assert_mask_before_ret(win: &str, label: &str) {
    let masked = win
        .find("msr daifset, #0xf")
        .unwrap_or_else(|| panic!("{label} 返回窗口必须 `msr daifset, #0xf` 屏蔽异步异常"));
    let wr_elr = win
        .find("msr elr_el1, x0")
        .unwrap_or_else(|| panic!("{label} 未找到 `msr elr_el1, x0`"));
    assert!(
        masked < wr_elr,
        "{label} 的屏蔽必须早于写 ELR_EL1 (否则返回寄存器仍可被窗口内异常覆写)"
    );
}

/// `handle_el1h_irq` 出口: 从 `bl irq_handler` 到首个终止 `eret`.
#[test]
fn test_el1h_irq_return_masks_before_eret() {
    let src = read();
    let win = norm(&slice_between(&src, "bl   irq_handler\n", "\n    eret"));
    assert_mask_before_ret(&win, "handle_el1h_irq");
}

/// `handle_el1h_sync` 出口: 从 `bl sync_exception_handler` 到首个终止 `eret`.
#[test]
fn test_el1h_sync_return_masks_before_eret() {
    let src = read();
    let win = norm(&slice_between(
        &src,
        "bl   sync_exception_handler\n",
        "\n    eret",
    ));
    assert_mask_before_ret(&win, "handle_el1h_sync");
}

/// `el0_return` 出口: 从 `el0_return:` 到末尾终止 `eret`.
#[test]
fn test_el0_return_masks_before_eret() {
    let src = read();
    let win = norm(&slice_between(&src, "el0_return:", "\n    eret"));
    assert_mask_before_ret(&win, "el0_return");
}
