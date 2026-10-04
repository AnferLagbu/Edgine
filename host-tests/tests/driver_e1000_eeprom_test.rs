//! driver: e1000 EEPROM 读取集成测试
//!
//! 追踪: I-40
//! SPDX-License-Identifier: MPL-2.0
//!
//! ## B08-21 处置 (2026-09-06): host 不可测, 平行实现已移除
//!
//! 原镜像对象为 `privileged/driver/net/e1000.rs::eeprom_read` /
//! `read_mac_address` (I-40) 的 EERD 寄存器状态机 + 魔数 + MAC 小端组装.
//! 评估结论: **内核该部分 host 不可测**, 原因:
//!
//! 1. `eeprom_read` / `read_mac_address` 均为**私有** `fn` (非 pub), host-tests
//!    无法引用;
//! 2. 两者依赖 `E1000Io` (privileged iomem::IoMem 的 MMIO 封装), 寄存器访问
//!    走真实 MMIO 读写, host 环境无法以 MockIoMem 注入 (MockIoMem 只是 host
//!    侧自建的寄存器模拟, 无法挂到内核私有 MMIO 路径);
//! 3. 真实硬件路径 (e1000-real-hw) 下 EERD 轮询含 `spin_loop` 超时 + MMIO 访问.
//!
//! 按 B08-20/21 消并规则, 依赖 MMIO/私有符号的镜像对象不保留平行实现:
//! 本地 `MockIoMem` / `EerdState` / `eeprom_read_real` / `eeprom_read_qemu` /
//! `mac_from_eeprom_words` 复刻与全部用例已删除 (QEMU 兼容路径 + EERD 状态机 +
//! MAC 字节序). 该契约的真实覆盖由 QEMU 集成测试 (网卡驱动路径) 承担.
//! 待内核将 MAC 字节组装 (word → 6 字节) 提炼为 pub 纯函数后可恢复 host 侧
//! 验证 (记录待办).
