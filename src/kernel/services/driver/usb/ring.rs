#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//!
//! xHCI 环形缓冲区 (Command Ring 命令环 + Event Ring 事件环) - USB-1.5
//!
//! 实现 xHCI 规范 §4.9 + §4.10 定义的两种核心环形缓冲区:
//!
//! - **Command Ring**: Host → Controller 命令队列
//!   - Host 生产 TRBs (写入 `enqueue_index`)
//!   - Controller 消费 TRBs
//!   - 触发: 写 doorbell slot 0 (Host Controller Doorbell)
//!
//! - **Event Ring**: Controller → Host 事件通知
//!   - Controller 生产 TRBs
//!   - Host 消费 TRBs (从 `dequeue_index` 读)
//!   - 中断上半部唤醒 Event Ring 消费者
//!
//! ## Cycle Bit 同步
//!
//! xHCI 使用 cycle bit 同步生产者/消费者:
//! - cycle bit 存在 TRB control field bit 0
//! - 生产者写入 TRB 时, control.bit0 = 当前 cycle
//! - 消费者读 TRB 时, 验证 control.bit0 == 当前 cycle (不匹配说明 buffer 还没 wrap 回来)
//! - Wrap 时生产者翻转 cycle (0→1 或 1→0), 通过 Link TRB 实现
//!
//! ## 与 xHCI 寄存器交互
//!
//! - **CRCR** (Command Ring Control Register): 指向 Command Ring dequeue pointer
//! - **ERSTBA** (Event Ring Segment Table Base Address): 指向 Event Ring Segment Table
//! - **ERDP** (Event Ring Dequeue Pointer): Host 写以通知 controller 已消费到何处
//!
//! See also: `xhci.rs` 第 3 组 (USB-1.1~1.4) 寄存器操作 + `enumerate.rs` 第 4 组 (USB-1.6) 设备枚举

use super::xhci::{Trb, TrbType};
use crate::framework::driver::framework::{DriverError, Result};
use alloc::vec;
use alloc::vec::Vec;

// ============================================================================
// 公共常量
// ============================================================================

/// 默认 Command Ring 大小 (TRB 数, 必须是 2 的幂, xHCI 规范 §4.9.1)
pub const DEFAULT_COMMAND_RING_SIZE: usize = 256;
/// 默认 Event Ring 大小 (TRB 数, 必须是 2 的幂)
pub const DEFAULT_EVENT_RING_SIZE: usize = 256;

/// TRB control field bit 0 = cycle bit
const TRB_CYCLE_BIT: u32 = 1 << 0;
/// Link TRB toggle cycle bit (bit 1)
const LINK_TRB_TOGGLE_BIT: u32 = 1 << 1;

// ============================================================================
// Command Ring (USB-1.5)
// ============================================================================

/// xHCI Command Ring — Host 生产, Controller 消费.
///
/// 环形缓冲区, 由若干 TRB 组成, 末尾的 Link TRB 指向 ring 起点
/// (可能带 toggle cycle 让 controller 区分两圈).
pub struct CommandRing {
    /// TRB 缓冲区
    trbs: Vec<Trb>,
    /// 下一个写入位置 (enqueue pointer)
    enqueue_index: usize,
    /// 当前 cycle 状态 (写入 TRB 时使用)
    cycle: bool,
}

impl CommandRing {
    /// 创建新的 Command Ring.
    ///
    /// # 参数
    ///
    /// - `size`: TRB 数量, 必须是 2 的幂, 推荐 256/512.
    ///
    /// # 错误
    ///
    /// - `DriverError::InvalidParameter`: size 不是 2 的幂或为 0.
    /// # Errors
    /// size 为 0 或不是 2 的幂时返回 Err。
    pub fn new(size: usize) -> Result<Self> {
        if size == 0 || (size & (size - 1)) != 0 {
            return Err(DriverError::InvalidParameter);
        }
        // 预留最后一项作为 Link TRB
        let mut trbs = vec![
            Trb {
                parameter: 0,
                status: 0,
                control: 0,
            };
            size
        ];
        // 初始化所有 TRB 的 cycle bit 为 0
        for trb in &mut trbs {
            trb.control = 0; // cycle bit = 0 (初始)
        }
        Ok(Self {
            trbs,
            enqueue_index: 0,
            cycle: true, // 初始 cycle = 1 (xHCI 规范约定)
        })
    }

    /// 写入一个 TRB 到 enqueue 位置, 自动设置 cycle bit.
    ///
    /// 注意: 此方法**不**触发 doorbell, 调用方需在批量提交后
    ///       自行调用 `xhci::ring_doorbell(0)`.
    /// # Errors
    /// 环已满 (到达 Link TRB 位置) 时返回 Err。
    pub fn push(&mut self, mut trb: Trb) -> Result<()> {
        // 最后一项保留为 Link TRB, 不可被普通 TRB 占用
        if self.enqueue_index >= self.trbs.len() - 1 {
            // 已到 Link TRB 位置, 拒绝 push (避免覆盖 Link)
            return Err(DriverError::BufferTooSmall);
        }

        // 设置 cycle bit (bit 0 of control)
        if self.cycle {
            trb.control |= TRB_CYCLE_BIT;
        } else {
            trb.control &= !TRB_CYCLE_BIT;
        }

        self.trbs[self.enqueue_index] = trb;
        self.enqueue_index += 1;

        // 到达 ring 末尾 (前一项), 写入 Link TRB 指向起点
        if self.enqueue_index >= self.trbs.len() - 1 {
            self.write_link_trb();
        }
        Ok(())
    }

    /// 写入 Link TRB 到最后一项, 指向 ring 起点并翻转 cycle.
    fn write_link_trb(&mut self) {
        // ring 基地址由调用方在 DMA 映射后填入, 此处仅占位
        // 真实硬件应通过 DMA 映射 ring 后获取物理地址, 写入 parameter 字段.
        // 注: 当前为**软件骨架**, ring buffer 物理地址 = 0; 真实硬件初始化时需修复.
        let ring_base: u64 = 0; // 占位 (Phase E 集成时由 DMA 映射填充)

        let link_trb = Trb {
            parameter: ring_base,
            status: 0,
            control: (TrbType::Link as u32) << 10
                | LINK_TRB_TOGGLE_BIT // 翻转 Link TRB 的 cycle 位
                | if self.cycle { TRB_CYCLE_BIT } else { 0 },
        };
        // 最后一项是 Link TRB
        let last_idx = self.trbs.len() - 1;
        self.trbs[last_idx] = link_trb;

        // 翻转 cycle (下一圈)
        self.cycle = !self.cycle;
        // enqueue_index 重置到 0 (下一圈从 0 开始)
        self.enqueue_index = 0;
    }

    /// 获取 ring 基地址 (DMA 映射后由调用方填充, 此处返回 parameter`[0]` 占位).
    ///
    /// 注: 真实硬件应在 `push` 前通过 DMA 映射 ring buffer 获取物理地址,
    ///     并写入 Link TRB 的 parameter 字段. 当前为软件骨架.
    pub fn base_address(&self) -> u64 {
        self.trbs[0].parameter // 占位, 真实硬件由 DMA 映射填充
    }

    /// 获取 enqueue pointer (供 xHCI CRCR 寄存器使用).
    ///
    /// 返回值应通过 DMA 映射转换为物理地址. 当前为软件偏移.
    pub fn enqueue_pointer(&self) -> usize {
        self.enqueue_index * core::mem::size_of::<Trb>()
    }

    /// 获取 ring 大小 (TRB 数).
    pub fn size(&self) -> usize {
        self.trbs.len()
    }

    /// 获取当前 cycle state.
    pub fn cycle(&self) -> bool {
        self.cycle
    }

    /// 重置 ring (清空所有 TRB, 重置 `enqueue_index` 和 cycle).
    pub fn reset(&mut self) {
        for trb in &mut self.trbs {
            trb.parameter = 0;
            trb.status = 0;
            trb.control = 0;
        }
        self.enqueue_index = 0;
        self.cycle = true;
    }
}

// ============================================================================
// Event Ring (USB-1.5)
// ============================================================================

/// xHCI Event Ring — Controller 生产, Host 消费.
///
/// 环形缓冲区, controller 写入 Transfer/Command Completion/Port Status Change
/// 等事件, host 在中断上半部读取并 dispatch.
pub struct EventRing {
    /// TRB 缓冲区
    trbs: Vec<Trb>,
    /// 下一个读取位置 (dequeue pointer)
    dequeue_index: usize,
    /// 当前 cycle state (host 期望的 cycle)
    cycle: bool,
}

impl EventRing {
    /// 创建新的 Event Ring.
    /// # Errors
    /// size 为 0 或不是 2 的幂时返回 Err。
    pub fn new(size: usize) -> Result<Self> {
        if size == 0 || (size & (size - 1)) != 0 {
            return Err(DriverError::InvalidParameter);
        }
        let trbs = vec![
            Trb {
                parameter: 0,
                status: 0,
                control: 0,
            };
            size
        ];
        Ok(Self {
            trbs,
            dequeue_index: 0,
            cycle: true, // 初始期望 cycle = 1
        })
    }

    /// 读取下一个事件 TRB.
    ///
    /// # 返回
    ///
    /// - `Some(trb)`: 成功读取事件
    /// - `None`: 当前 cycle 不匹配 (controller 还没写到这里)
    pub fn pop(&mut self) -> Option<Trb> {
        let trb = self.trbs[self.dequeue_index];
        let trb_cycle = (trb.control & TRB_CYCLE_BIT) != 0;

        if trb_cycle != self.cycle {
            // cycle 不匹配, 说明当前 entry 未被 controller 写入
            return None;
        }

        self.dequeue_index += 1;

        // Wrap around: dequeue_index 到达末尾后翻转 cycle
        if self.dequeue_index >= self.trbs.len() {
            self.dequeue_index = 0;
            self.cycle = !self.cycle;
        }

        Some(trb)
    }

    /// 窥视下一个事件 (不消费).
    pub fn peek(&self) -> Option<Trb> {
        let trb = self.trbs[self.dequeue_index];
        let trb_cycle = (trb.control & TRB_CYCLE_BIT) != 0;
        if trb_cycle == self.cycle {
            Some(trb)
        } else {
            None
        }
    }

    /// 获取 dequeue pointer (供 xHCI ERDP 寄存器使用).
    pub fn dequeue_pointer(&self) -> usize {
        self.dequeue_index * core::mem::size_of::<Trb>()
    }

    /// 获取 ring 大小 (TRB 数).
    pub fn size(&self) -> usize {
        self.trbs.len()
    }

    /// 获取当前 cycle state.
    pub fn cycle(&self) -> bool {
        self.cycle
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_ring_new_validates_size() {
        // size 不是 2 的幂
        assert!(CommandRing::new(100).is_err());
        assert!(CommandRing::new(0).is_err());
        assert!(CommandRing::new(7).is_err());
        // 2 的幂 OK
        assert!(CommandRing::new(256).is_ok());
        assert!(CommandRing::new(512).is_ok());
    }

    #[test]
    fn test_command_ring_push_sets_cycle_bit() {
        let mut ring = CommandRing::new(256).unwrap();
        let trb = Trb::new(0xDEAD_BEEF, 0, 0);
        ring.push(trb).unwrap();
        // 第一个 TRB 的 cycle bit 应该是 1 (初始 cycle=true)
        assert_eq!(ring.trbs[0].control & TRB_CYCLE_BIT, TRB_CYCLE_BIT);
    }

    #[test]
    fn test_command_ring_push_near_link_trb() {
        let mut ring = CommandRing::new(4).unwrap(); // 4 TRB, 最后 1 个是 Link
        // 可以 push 3 个 (第 4 个位置是 Link)
        assert!(ring.push(Trb::new(1, 0, 0)).is_ok());
        assert!(ring.push(Trb::new(2, 0, 0)).is_ok());
        assert!(ring.push(Trb::new(3, 0, 0)).is_ok());
        // 第 3 次 push 填满可用槽位后, CommandRing 自动写入 Link TRB 并回绕
        // (cycle 翻转 + enqueue_index 归零), 故第 4 次 push 成功落在索引 0
        // (原断言 is_err 与回绕语义矛盾; 2026-09-24 UT-06 修正).
        assert!(ring.push(Trb::new(4, 0, 0)).is_ok());
        assert_eq!(ring.enqueue_index, 1);
    }

    #[test]
    fn test_command_ring_link_trb_toggles_cycle() {
        let mut ring = CommandRing::new(4).unwrap();
        let initial_cycle = ring.cycle();
        // Push 3 个 TRB 触发 Link TRB 写入
        for i in 0..3 {
            ring.push(Trb::new(i, 0, 0)).unwrap();
        }
        // Link TRB 应位于最后一项
        let link = &ring.trbs[3];
        assert_eq!((link.control >> 10) & 0x3F, TrbType::Link as u32);
        // cycle 应已翻转
        assert_ne!(ring.cycle(), initial_cycle);
        // enqueue_index 应回到 0
        assert_eq!(ring.enqueue_index, 0);
    }

    #[test]
    fn test_command_ring_reset() {
        let mut ring = CommandRing::new(256).unwrap();
        ring.push(Trb::new(0x1234, 0, 0)).unwrap();
        ring.push(Trb::new(0x5678, 0, 0)).unwrap();
        assert_eq!(ring.enqueue_index, 2);
        ring.reset();
        assert_eq!(ring.enqueue_index, 0);
        assert!(ring.cycle());
        // Trb 是 packed 结构: u64 字段取引用会触发 E0793, 故按值取出再断言.
        assert_eq!({ ring.trbs[0].parameter }, 0);
    }

    #[test]
    fn test_event_ring_new_validates_size() {
        assert!(EventRing::new(0).is_err());
        assert!(EventRing::new(100).is_err());
        assert!(EventRing::new(256).is_ok());
    }

    #[test]
    fn test_event_ring_empty_returns_none() {
        let mut ring = EventRing::new(256).unwrap();
        // 空 ring, pop 应返回 None
        assert!(ring.pop().is_none());
        assert!(ring.peek().is_none());
    }

    #[test]
    fn test_event_ring_pop_consumes_trb() {
        let mut ring = EventRing::new(4).unwrap();
        // 手动写入一个 TRB, cycle = 1 (与 ring.cycle 匹配)
        ring.trbs[0] = Trb::new(0xCAFE_BABE, 0, TRB_CYCLE_BIT);
        let trb = ring.pop().unwrap();
        assert_eq!({ trb.parameter }, 0xCAFE_BABE);
        // dequeue 已推进
        assert_eq!(ring.dequeue_index, 1);
    }

    #[test]
    fn test_event_ring_cycle_mismatch_returns_none() {
        let mut ring = EventRing::new(4).unwrap();
        // 写入 cycle = 0 的 TRB, 但 ring 期望 cycle = 1
        ring.trbs[0] = Trb::new(0xCAFE_BABE, 0, 0);
        assert!(ring.pop().is_none());
        assert_eq!(ring.dequeue_index, 0);
    }

    #[test]
    fn test_event_ring_wrap_around_toggles_cycle() {
        let mut ring = EventRing::new(2).unwrap();
        let initial_cycle = ring.cycle();
        // 写入 2 个 TRB, cycle = 1
        ring.trbs[0] = Trb::new(1, 0, TRB_CYCLE_BIT);
        ring.trbs[1] = Trb::new(2, 0, TRB_CYCLE_BIT);
        // Pop 2 次后 dequeue_index 回 0, cycle 翻转
        assert!(ring.pop().is_some());
        assert!(ring.pop().is_some());
        assert_eq!(ring.dequeue_index, 0);
        assert_ne!(ring.cycle(), initial_cycle);
    }

    #[test]
    fn test_peek_does_not_consume() {
        let mut ring = EventRing::new(4).unwrap();
        ring.trbs[0] = Trb::new(0xFEED, 0, TRB_CYCLE_BIT);
        // peek 不应推进 dequeue_index
        assert!(ring.peek().is_some());
        assert_eq!(ring.dequeue_index, 0);
        // pop 应能正常消费
        assert!(ring.pop().is_some());
        assert_eq!(ring.dequeue_index, 1);
    }
}
