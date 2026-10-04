//! Legacy 8259A PIC 假性 IRQ 检测契约测试 (I-25)
//!
//! ## B08-20 处置 (2026-09-06): host 不可测, 平行实现已移除
//!
//! 原镜像对象为 `privileged/idt/idt.rs::detect_spurious_8259_irq` (I-25) 的
//! 位运算真值表 + EOI 决策. 评估结论: **内核该函数 host 不可测**, 原因:
//!
//! 1. `detect_spurious_8259_irq` 为**私有** `fn` (非 pub), host-tests 无法引用;
//! 2. 判定依赖 `read_8259_isr(slave)` 经 OCW3=0x0B 读取硬件 ISR 寄存器
//!    (unsafe 端口 I/O), 无法在 host 环境模拟;
//! 3. EOI 决策逻辑内联于 idt.rs 中断分发路径, 依赖全局 `SPURIOUS_IRQ_COUNT`
//!    + `port_outb` 硬件写, 同样不可 host 测.
//!
//! 按 B08-20/21 消并规则, 依赖 MMIO/硬件 I/O 的镜像对象不保留平行实现:
//! 本地 `is_spurious_8259_irq` 真值表镜像与 EOI 决策模拟已删除.
//! 该契约的真实覆盖由 QEMU 集成测试 (中断路径) 承担.
//! 待内核将纯位运算判定提炼为 pub 纯函数后可恢复 host 侧验证 (记录待办).
