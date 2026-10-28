//! `NVMe` wire 类型层 — privileged 机制保留面 (DECISION-H storage 专项 3 号子步)
//!
//! 控制器业务已整体迁 `functions::driver::storage::nvme` (functions 权威);
//! 本文件仅保留 wire 类型 (命令/完成项/操作码) 供 functions 驱动与
//! privileged safe wrapper (提交/排空) 共用。
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

#![deny(missing_docs)]

// ============================================================================
// NVMe 命令定义
// ============================================================================

/// Admin/I/O 队列深度 (wire 编码共用: create_cq/create_sq 的大小字段编码,
/// 亦为 privileged wrapper `NVME_QD` 与 functions 队列分配的唯一来源)
pub(super) const QUEUE_DEPTH: usize = 64;

/// Admin 命令操作码
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NvmeAdminOpcode {
    /// 删除提交队列
    DeleteSq = 0x00,
    /// 创建提交队列
    CreateSq = 0x01,
    /// 读取日志页
    GetLogPage = 0x02,
    /// 删除完成队列
    DeleteCq = 0x04,
    /// 创建完成队列
    CreateCq = 0x05,
    /// 识别控制器或命名空间
    Identify = 0x06,
    /// 中止指定命令
    Abort = 0x08,
    /// 设置控制器特性
    SetFeatures = 0x09,
    /// 读取控制器特性
    GetFeatures = 0x0A,
}

/// NVM I/O 命令操作码
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NvmeNvmOpcode {
    /// 刷新写缓存
    Flush = 0x00,
    /// 写入数据块
    Write = 0x01,
    /// 读取数据块
    Read = 0x02,
}

/// `NVMe` 命令 (64字节)
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct NvmeCommand {
    /// 操作码 (Admin 或 NVM 命令集)
    pub opcode: u8,
    /// 命令标志位 (FUSE / PSDT 等)
    pub flags: u8,
    /// 命令标识符 (主机分配, 完成项回带)
    pub cid: u16,
    /// 命名空间标识符 (Admin 命令为 0)
    pub nsid: u32,
    /// 命令双字 2 (命令特有)
    pub cdw2: u32,
    /// 命令双字 3 (命令特有)
    pub cdw3: u32,
    /// 元数据指针 (仅带元数据命令使用)
    pub mptr: u64,
    /// 物理区域页 1 (数据缓冲区首地址)
    pub prp1: u64,
    /// 物理区域页 2 (第二页或 PRP 列表)
    pub prp2: u64,
    /// 命令双字 10 (命令特有)
    pub cdw10: u32,
    /// 命令双字 11 (命令特有)
    pub cdw11: u32,
    /// 命令双字 12 (命令特有)
    pub cdw12: u32,
    /// 命令双字 13 (命令特有)
    pub cdw13: u32,
    /// 命令双字 14 (命令特有)
    pub cdw14: u32,
    /// 命令双字 15 (命令特有)
    pub cdw15: u32,
}

impl NvmeCommand {
    /// 创建全零命令 (调用方随后按操作码填写各字段)
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
            cdw10: (slba & 0xFFFF_FFFF) as u32,
            cdw11: ((slba >> 32) & 0xFFFF_FFFF) as u32,
            cdw12: (u32::from(nlb) - 1) & 0xFFFF, // NLB = #blocks - 1
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        }
    }

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
            cdw10: (slba & 0xFFFF_FFFF) as u32,
            cdw11: ((slba >> 32) & 0xFFFF_FFFF) as u32,
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
            cdw10: ((crate::privileged::lib::low_u32_from_usize(QUEUE_DEPTH) - 1) << 16)
                | u32::from(qid),
            cdw11: (u32::from(irq_vector) << 16) | (1 | 2), // [31:16]=vector [2:0]=PC+IEN
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        }
    }

    /// 创建 Create I/O Submission Queue 命令
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
            cdw10: ((crate::privileged::lib::low_u32_from_usize(QUEUE_DEPTH) - 1) << 16)
                | u32::from(qid),
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
    /// 完成双字 0 (命令特有结果, 如读命令的状态信息)
    pub cdw0: u32,
    /// 保留字段
    pub rsvd1: u32,
    /// 提交队列头指针
    pub sqhd: u16,
    /// 提交队列标识符
    pub sqid: u16,
    /// 命令标识符 (与提交时的 cid 对应)
    pub cid: u16,
    /// 状态字段 (bit0 为阶段标记, 其余为状态码)
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
    /// 提取状态码 (status 位 `[30:17]`)
    pub fn status_code(&self) -> u16 {
        (self.status >> 1) & 0x7FF
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 状态码为 0 即命令成功完成
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
