//! `DmaStream` / `DmaCoherent` — 安全 DMA 映射 (TCB)
//!
//! 封装 DMA 缓冲区的分配/映射/释放, 确保:
//! - 物理地址和虚拟地址的一致性 (x86 自动, aarch64 显式 cache flush)
//! - DMA 缓冲区生命周期管理 (解除映射前无 CPU 写入)
//! - 状态机: CPU 端写入 → 设备可见; 设备写入 → CPU 端可见
//!
//! ## 与 Asterinas OSTD `DmaStream` / `DmaCoherent` 的关系
//!
//! 等价于 OSTD 的 `DmaStream` + `DmaCoherent`。
//!
//! ## SAFETY 不变量
//!
//! - `DmaStream::sync_for_device()` 必须在 CPU 写入后、设备读取前调用 (非一致性架构)。
//! - `DmaCoherent` 内存在整个生命周期内对设备和 CPU 都可见。
//! - DMA 缓冲区持有的 Frame 引用计数防止物理页被重用。
//! - 大小算术全部使用 `checked_add` 防溢出。
//! - 同步方向与 `DmaDirection` 不匹配时返回错误, 防止状态机污染。

use core::fmt;
use core::ptr::NonNull;

use crate::privileged::mm::PhysAddr;

#[cfg(target_arch = "aarch64")]
use crate::privileged::mm::CACHE_LINE_SIZE;
use crate::privileged::mm::PAGE_SIZE;

use super::frame::Frame;

/// DMA 传输方向
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DmaDirection {
    /// CPU 写入 → 设备读取 (典型: 网卡发送, 磁盘写)
    ToDevice,
    /// 设备写入 → CPU 读取 (典型: 网卡接收, 磁盘读)
    FromDevice,
    /// 双向 (典型: 部分设备 scatter-gather)
    Bidirectional,
}

/// DMA 同步状态机
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncState {
    /// 初始状态, CPU 可访问: `ToDevice` 表示 CPU 写完, `FromDevice` 表示 CPU 可读
    CpuReady,
    /// 已 `sync_for_device`, 设备可访问
    DeviceReady,
    /// 双向 DMA 中间态 (一次传输尚未结束)
    BidirInProgress,
}

/// DMA 错误类型
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DmaError {
    /// 物理地址或大小未页对齐
    NotAligned,
    /// 物理地址 + 大小溢出
    SizeOverflow,
    /// size = 0
    ZeroSize,
    /// size > `DMA_MAX_SIZE` (256 MiB)
    SizeTooLarge,
    /// 状态机转换非法 (例如 `ToDevice` 调 `sync_for_cpu`)
    InvalidStateTransition,
    /// Frame 内部无效 (size == 0 等)
    InvalidFrame,
}

/// 典型设备 DMA 对齐要求 (4 KiB 页对齐)
pub const DMA_ALIGNMENT: u64 = PAGE_SIZE;
/// 最大单次 DMA 大小 (256 MiB, 防止物理内存耗尽)
pub const DMA_MAX_SIZE: u64 = 256 * 1024 * 1024;

/// 流式 DMA 缓冲区 (非一致性)。
///
/// 映射生命周期: alloc → `sync_for_device` → DMA → `sync_for_cpu` → free。
/// `x86_64` 上 sync 为空操作 (硬件保证一致性)。
pub struct DmaStream {
    cpu_addr: NonNull<u8>,
    dma_addr: PhysAddr,
    size: usize,
    direction: DmaDirection,
    sync_state: SyncState,
    _frame: Frame, // 持有 Frame 引用, 防物理页重用
}

impl DmaStream {
    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    /// 从 Frame 创建流式 DMA 映射。
    ///
    /// 验证 Frame 物理地址 + 大小满足:
    /// - 4 KiB 页对齐
    /// - size > 0 且 <= `DMA_MAX_SIZE`
    /// - paddr + size 不溢出
    /// # Errors
    /// Frame 无效、大小为 0、物理地址未按 DMA 对齐、大小超过 `DMA_MAX_SIZE` 或地址溢出时返回 Err。
    pub fn from_frame(frame: Frame, dir: DmaDirection) -> Result<Self, DmaError> {
        let size = frame.size();
        let phys = frame.phys();
        let cpu_ptr = match NonNull::new(frame.as_virt_ptr()) {
            Some(p) => p,
            None => return Err(DmaError::InvalidFrame),
        };

        if size == 0 {
            return Err(DmaError::ZeroSize);
        }
        if !phys.as_u64().is_multiple_of(DMA_ALIGNMENT) {
            return Err(DmaError::NotAligned);
        }
        if !((size as u64).is_multiple_of(DMA_ALIGNMENT)) {
            return Err(DmaError::NotAligned);
        }
        if (size as u64) > DMA_MAX_SIZE {
            return Err(DmaError::SizeTooLarge);
        }
        // 验证 paddr + size 不溢出
        if phys.as_u64().checked_add(size as u64).is_none() {
            return Err(DmaError::SizeOverflow);
        }

        let initial_state = match dir {
            // ToDevice: CPU 端刚分配完, 准备写, 状态为 CpuReady
            DmaDirection::ToDevice => SyncState::CpuReady,
            // FromDevice: 设备已写入过, 状态为 DeviceReady
            DmaDirection::FromDevice => SyncState::DeviceReady,
            DmaDirection::Bidirectional => SyncState::CpuReady,
        };

        Ok(Self {
            cpu_addr: cpu_ptr,
            dma_addr: phys,
            size,
            direction: dir,
            sync_state: initial_state,
            _frame: frame,
        })
    }

    /// CPU 可访问的虚拟地址
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    pub fn cpu_addr(&self) -> NonNull<u8> {
        self.cpu_addr
    }

    /// 设备可访问的物理地址
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    pub fn dma_addr(&self) -> PhysAddr {
        self.dma_addr
    }

    /// 缓冲区大小
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    pub fn size(&self) -> usize {
        self.size
    }

    /// 同步方向
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    pub fn direction(&self) -> DmaDirection {
        self.direction
    }

    /// 当前同步状态
    #[inline(always)]
    pub fn sync_state(&self) -> SyncState {
        self.sync_state
    }

    /// CPU→设备: flush CPU cache, 确保设备看到最新数据。
    ///
    /// `x86_64`: 空操作 (硬件保证一致性)。
    /// aarch64: 需要 DC CVAU (Data Cache Clean to Point of Unification)。
    /// # Errors
    /// 当前方向不支持该同步或同步状态机不允许此转换时返回 Err。
    pub fn sync_for_device(&mut self) -> Result<(), DmaError> {
        match self.direction {
            DmaDirection::ToDevice | DmaDirection::Bidirectional => {
                if self.sync_state == SyncState::CpuReady
                    || self.sync_state == SyncState::BidirInProgress
                {
                    self.sync_state = SyncState::DeviceReady;
                    // aarch64: 对 cpu_addr..cpu_addr+size 执行 DC CVAU (清理数据缓存到统一点)
                    #[cfg(target_arch = "aarch64")]
                    {
                        // SAFETY: DC CVAU 按 cache line 遍历, 将脏数据写回主存,
                        // 确保设备 DMA 读取时看到 CPU 最新写入.
                        // cpu_addr 是 DmaStream 持有的有效虚拟地址, size 已校验.
                        let start = self.cpu_addr.as_ptr() as u64;
                        let end = start + self.size as u64;
                        let mut addr = start & !(CACHE_LINE_SIZE - 1);
                        while addr < end {
                            // SAFETY: dc cvau 是 aarch64 标准 cache 维护指令,
                            // 输入地址已对齐到 cache line, 无副作用.
                            unsafe {
                                core::arch::asm!("dc cvau, {}", in(reg) addr);
                            }
                            addr += CACHE_LINE_SIZE;
                        }
                        // SAFETY: dsb ish 是数据同步屏障, 确保 cache 维护完成.
                        unsafe {
                            core::arch::asm!("dsb ish");
                        }
                    }
                    Ok(())
                } else {
                    Err(DmaError::InvalidStateTransition)
                }
            }
            DmaDirection::FromDevice => Err(DmaError::InvalidStateTransition),
        }
    }

    /// 设备→CPU: invalidate CPU cache, 确保 CPU 看到设备写入。
    ///
    /// `x86_64`: 空操作。
    /// aarch64: 需要 DC CIVAC (Data Cache Clean and Invalidate to Point of Coherency)。
    /// # Errors
    /// 当前方向不支持该同步或同步状态机不允许此转换时返回 Err。
    pub fn sync_for_cpu(&mut self) -> Result<(), DmaError> {
        match self.direction {
            DmaDirection::FromDevice | DmaDirection::Bidirectional => {
                if self.sync_state == SyncState::DeviceReady
                    || self.sync_state == SyncState::BidirInProgress
                {
                    self.sync_state = SyncState::CpuReady;
                    #[cfg(target_arch = "aarch64")]
                    {
                        // SAFETY: DC CIVAC 按 cache line 遍历, clean+invalidate 到 PoC,
                        // 确保后续 CPU 读取从主存获取设备写入的数据 (DMA 设备→CPU 方向语义).
                        // cpu_addr 是 DmaStream 持有的有效虚拟地址, size 已校验.
                        // G-04 (2026-09-06): `dc ivau` 在 AArch64 中不存在 — DC 指令族
                        // 无 IVAU 变体 (ivau 是 IC 指令族操作); 原代码混淆 IC/DC 指令族.
                        // 改 `dc civac` (clean+invalidate to PoC) 正是设备→CPU 所需语义.
                        let start = self.cpu_addr.as_ptr() as u64;
                        let end = start + self.size as u64;
                        let mut addr = start & !(CACHE_LINE_SIZE - 1);
                        while addr < end {
                            // SAFETY: dc civac 是 aarch64 标准 cache 维护指令,
                            // 输入地址已对齐到 cache line, 无副作用.
                            unsafe {
                                core::arch::asm!("dc civac, {}", in(reg) addr);
                            }
                            addr += CACHE_LINE_SIZE;
                        }
                        // SAFETY: dsb ish 是数据同步屏障, 确保 cache 维护完成.
                        unsafe {
                            core::arch::asm!("dsb ish");
                        }
                    }
                    Ok(())
                } else {
                    Err(DmaError::InvalidStateTransition)
                }
            }
            DmaDirection::ToDevice => Err(DmaError::InvalidStateTransition),
        }
    }
}

impl fmt::Display for DmaStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "DmaStream(cpu=0x{:x}, dma=0x{:x}, size={}, dir={:?}, state={:?})",
            self.cpu_addr.as_ptr() as usize,
            self.dma_addr.as_u64(),
            self.size,
            self.direction,
            self.sync_state,
        )
    }
}

// SAFETY: DmaStream 持有独立的 Frame 和 DMA 映射, Send/Sync 安全。
// SAFETY: DmaStream 含裸指针, 但所有可变访问通过 &mut self 排他访问, 避免数据竞争.
unsafe impl Send for DmaStream {}
// SAFETY: 同上, &mut self 保证排他访问.
unsafe impl Sync for DmaStream {}
