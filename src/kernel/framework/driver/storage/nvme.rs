//! `NVMe` wire 类型层 — framework 机制保留面 (DECISION-H storage 专项 3 号子步)
//!
//! 控制器业务已整体迁 `services::driver::storage::nvme` (services 权威);
//! 本文件仅保留 wire 类型 (命令/完成项/操作码) 供 services 驱动与
//! framework safe wrapper (提交/排空) 共用。
//!
//! `NVMe` 驱动 (`NVMe` Driver)
//!
//! `提供NVMe` (Non-Volatile Memory Express) SSD支持：
//! - **`PCIe接口`**: `高速PCIe总线连接`
//! - **DMA读写**: Admin队列 + I/O队列提交
//! - **PRP寻址**: 物理区域页寻址
//! - **命名空间**: 多命名空间支持
//!
//! ## 硬件规格
//!
//! ```text
//! NVMe Controller:
//! ├── PCIe Configuration Space
//! ├── Controller Registers (BAR0)
//! │   ├── CAP, VS, INTMS, INTMC
//! │   ├── CC, CSTS, NSSR
//! │   ├── AQA, ASQ, ACQ
//! │   └── Doorbell Registers
//! │       ├── SQ0TDBL (Admin SQ Tail)
//! │       └── CQ0HDBL (Admin CQ Head)
//! └── Queue Pairs (DMA allocated)
//!     ├── Admin SQ / CQ
//!     └── I/O SQ / CQ
//! ```
//!
//! # Safety
//! NVMe驱动涉及PCIe配置、MMIO寄存器和DMA操作。

// ============================================================================
// NVMe 命令定义
// ============================================================================

/// Admin/I/O 队列深度 (wire 编码共用: create_cq/create_sq 的大小字段编码,
/// 亦为 framework wrapper `NVME_QD` 与 services 队列分配的唯一来源)
pub(super) const QUEUE_DEPTH: usize = 64;

/// Admin 命令操作码
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NvmeAdminOpcode {
    DeleteSq = 0x00,
    CreateSq = 0x01,
    GetLogPage = 0x02,
    DeleteCq = 0x04,
    CreateCq = 0x05,
    Identify = 0x06,
    Abort = 0x08,
    SetFeatures = 0x09,
    GetFeatures = 0x0A,
}

/// NVM I/O 命令操作码
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NvmeNvmOpcode {
    Flush = 0x00,
    Write = 0x01,
    Read = 0x02,
}

/// `NVMe` 命令 (64字节)
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct NvmeCommand {
    pub opcode: u8,
    pub flags: u8,
    pub cid: u16,
    pub nsid: u32,
    pub cdw2: u32,
    pub cdw3: u32,
    pub mptr: u64, // MPTR (metadata 指针, 仅带元数据命令使用)
    pub prp1: u64, // PRP1
    pub prp2: u64, // PRP2
    pub cdw10: u32,
    pub cdw11: u32,
    pub cdw12: u32,
    pub cdw13: u32,
    pub cdw14: u32,
    pub cdw15: u32,
}

impl NvmeCommand {
    pub fn new() -> Self {
        Self {
            opcode: 0,
            flags: 0,
            cid: 0,
            nsid: 0,
            cdw2: 0,
            cdw3: 0,
            mptr: 0,
            prp1: 0,
            prp2: 0,
            cdw10: 0,
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        }
    }

    #[expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
    )]
    /// 创建读命令
    pub fn read(nsid: u32, slba: u64, nlb: u16, prp1: u64) -> Self {
        Self {
            opcode: NvmeNvmOpcode::Read as u8,
            flags: 0,
            cid: 0,
            nsid,
            cdw2: 0,
            cdw3: 0,
            mptr: 0,
            prp1,
            prp2: 0,
            cdw10: (slba & 0xFFFFFFFF) as u32,
            cdw11: ((slba >> 32) & 0xFFFFFFFF) as u32,
            cdw12: (u32::from(nlb) - 1) & 0xFFFF, // NLB = #blocks - 1
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        }
    }

    #[expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
    )]
    /// 创建写命令
    pub fn write(nsid: u32, slba: u64, nlb: u16, prp1: u64) -> Self {
        Self {
            opcode: NvmeNvmOpcode::Write as u8,
            flags: 0,
            cid: 0,
            nsid,
            cdw2: 0,
            cdw3: 0,
            mptr: 0,
            prp1,
            prp2: 0,
            cdw10: (slba & 0xFFFFFFFF) as u32,
            cdw11: ((slba >> 32) & 0xFFFFFFFF) as u32,
            cdw12: (u32::from(nlb) - 1) & 0xFFFF,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        }
    }

    /// 创建 Identify 命令
    pub fn identify(nsid: u32, cns: u8, prp1: u64) -> Self {
        Self {
            opcode: NvmeAdminOpcode::Identify as u8,
            flags: 0,
            cid: 0,
            nsid,
            cdw2: 0,
            cdw3: 0,
            mptr: 0,
            prp1,
            prp2: 0,
            cdw10: u32::from(cns), // CNS (Controller/Namespace)
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        }
    }

    /// 创建 Create I/O Completion Queue 命令
    // 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
    #[expect(clippy::cast_possible_truncation)]
    /// 创建 Create I/O Completion Queue 命令
    ///
    /// `irq_vector` 是 MSI-X Table 中的向量索引（0-based，由 NVMe 设备用作
    /// `msix_notify(pci, vector)` 的索引）。MSI-X enable 后已为该 CQ 分配了
    /// 一个 IDT 向量，driver 需在 cdw11 高 16 位写入与 MSI-X Table entry 一致的
    /// 索引；否则 QEMU 投递向量与 IDT 实际入口不对应 → 中断丢失。
    pub fn create_cq(qid: u16, cq_phys: u64, irq_vector: u16) -> Self {
        Self {
            opcode: NvmeAdminOpcode::CreateCq as u8,
            flags: 0,
            cid: 0,
            nsid: 0,
            cdw2: 0,
            cdw3: 0,
            mptr: 0,
            prp1: cq_phys,
            prp2: 0,
            cdw10: ((QUEUE_DEPTH as u32 - 1) << 16) | u32::from(qid),
            cdw11: (u32::from(irq_vector) << 16) | (1 | 2), // [31:16]=vector [2:0]=PC+IEN
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        }
    }

    /// 创建 Create I/O Submission Queue 命令
    // 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
    #[expect(clippy::cast_possible_truncation)]
    pub fn create_sq(qid: u16, cqid: u16, sq_phys: u64) -> Self {
        Self {
            opcode: NvmeAdminOpcode::CreateSq as u8,
            flags: 0,
            cid: 0,
            nsid: 0,
            cdw2: 0,
            cdw3: 0,
            mptr: 0,
            prp1: sq_phys,
            prp2: 0,
            cdw10: ((QUEUE_DEPTH as u32 - 1) << 16) | u32::from(qid),
            cdw11: u32::from(cqid) << 16 | 1, // CQID | PC
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        }
    }
}

/// `NVMe` 完成队列条目 (16字节)
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct NvmeCompletion {
    pub cdw0: u32,
    pub rsvd1: u32,
    pub sqhd: u16,
    pub sqid: u16,
    pub cid: u16,
    pub status: u16,
}

impl NvmeCompletion {
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 阶段标记匹配 = 完成
    pub fn is_completed(&self, phase: u16) -> bool {
        (self.status & 0x01) == phase
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn status_code(&self) -> u16 {
        (self.status >> 1) & 0x7FF
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn is_success(&self) -> bool {
        self.status_code() == 0
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nvme_command_read() {
        let cmd = NvmeCommand::read(1, 0, 1, 0x1000);
        // NvmeCommand 是 packed 结构: 多字节字段取引用会触发 E0793, 故按值取出再断言.
        assert_eq!({ cmd.opcode }, NvmeNvmOpcode::Read as u8);
        assert_eq!({ cmd.nsid }, 1);
        assert_eq!({ cmd.cdw12 }, 0); // NLB-1 = 0
    }

    #[test]
    fn test_nvme_command_write() {
        let cmd = NvmeCommand::write(1, 100, 8, 0x2000);
        assert_eq!({ cmd.opcode }, NvmeNvmOpcode::Write as u8);
        assert_eq!({ cmd.cdw10 }, 100);
        assert_eq!({ cmd.cdw12 }, 7); // 8 NLB -> 7
    }

    #[test]
    fn test_nvme_completion() {
        let mut cq = NvmeCompletion {
            cdw0: 0,
            rsvd1: 0,
            sqhd: 0,
            sqid: 0,
            cid: 0,
            status: 0x0001, // Phase=1, Status=0
        };
        assert!(cq.is_completed(1));
        assert!(cq.is_success());

        cq.status = 0x0003; // Phase=1, Status=1
        assert!(!cq.is_success());
        assert_eq!(cq.status_code(), 1);
    }

    #[test]
    fn test_nvme_command_identify() {
        // cns = 0x01 (Identify Controller, 控制器识别命令)
        let cmd = NvmeCommand::identify(0, 0x01, 0x3000);
        assert_eq!({ cmd.opcode }, NvmeAdminOpcode::Identify as u8);
        assert_eq!({ cmd.cdw10 }, 0x01);
        assert_eq!({ cmd.prp1 }, 0x3000);
    }

    #[test]
    fn test_nvme_command_create_cq() {
        let cmd = NvmeCommand::create_cq(1, 0x5000, 0);
        assert_eq!({ cmd.opcode }, NvmeAdminOpcode::CreateCq as u8);
        // cdw10 布局: ((depth-1) << 16) | qid = (63 << 16) | 1
        assert_eq!({ cmd.cdw10 }, (63 << 16) | 1);
        // cdw11 高 16 位为 MSI-X 向量索引, 低 3 位 PC+IEN
        assert_eq!({ cmd.cdw11 }, 3);
    }

    #[test]
    fn test_command_sizes() {
        assert_eq!(core::mem::size_of::<NvmeCommand>(), 64);
        assert_eq!(core::mem::size_of::<NvmeCompletion>(), 16);
    }
}
