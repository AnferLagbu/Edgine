//! 核心转储决策 trait — 策略-机制分离接口
//!
//! T-06 同构: 核心转储策略由 services 实现, framework 仅保留中断/异常帧
//! 寄存器快照 (机制) 与信号投递路径上的 sink 调用点.
//!
//! ## 设计
//!
//! - trait 定义在 framework, 实现在 services (100% safe Rust)
//! - framework 提供默认回退实现 (`FallbackCoredumpSink`), 早期启动阶段使用
//! - services 在 `init()` 中通过 `register_coredump_sink()` 注册实现
//! - framework 额外提供 `read_interrupt_regs()` 作为 POD 寄存器快照机制,
//!   使 services 无需 unsafe 即可读取中断帧

use crate::framework::sync::OnceLock;

/// 核心转储接口 — services 实现, framework 调用
///
/// 实现在 services 侧完成 ELF core 文件的构造与写入;
/// framework 仅在信号默认动作为 `Core` 时调用本接口.
pub trait CoredumpSink: Send + Sync {
    /// 对指定进程执行核心转储
    ///
    /// `pid` 为目标进程, `sig` 为触发信号, `frame_addr` 为中断帧地址
    /// (可为 0, 表示无帧信息). 返回是否成功写入转储文件.
    fn coredump(&self, pid: u32, sig: u8, frame_addr: u64) -> bool;
}

// ============================================================================
// 默认回退实现 (早期启动阶段, services 尚未注册时使用)
// ============================================================================

/// 框架内建回退实现 — 不执行任何转储
pub struct FallbackCoredumpSink;

impl CoredumpSink for FallbackCoredumpSink {
    fn coredump(&self, _pid: u32, _sig: u8, _frame_addr: u64) -> bool {
        false
    }
}

// ============================================================================
// 全局实现注册表
// ============================================================================

static COREDUMP_SINK: OnceLock<&'static dyn CoredumpSink> = OnceLock::new();

/// 注册核心转储实现
///
/// services 在 `init()` 中调用. 仅允许注册一次, 重复注册返回 `Err(旧实现)`.
///
/// # Errors
/// 当实现已注册时, 返回 `Err`, 其中携带已注册的旧实现指针.
pub fn register_coredump_sink(
    sink: &'static dyn CoredumpSink,
) -> Result<(), &'static dyn CoredumpSink> {
    COREDUMP_SINK.set(sink)
}

/// 获取当前核心转储实现
///
/// 若 services 尚未注册, 返回默认回退实现.
pub fn current_coredump_sink() -> &'static dyn CoredumpSink {
    COREDUMP_SINK
        .get()
        .copied()
        .unwrap_or(&FallbackCoredumpSink)
}

// ============================================================================
// 中断/异常帧寄存器快照 (机制, 供 services 免 unsafe 读取)
// ============================================================================

/// 中断帧寄存器快照 (按 Linux prstatus regset 顺序排列)
///
/// x86_64 有效长度 27, aarch64 有效长度 34; 通过 [`as_slice`](Self::as_slice)
/// 取得有效视图.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegSnapshot {
    regs: [u64; 34],
    len: usize,
}

impl RegSnapshot {
    /// 有效寄存器视图 (x86_64 27 项 / aarch64 34 项)
    pub fn as_slice(&self) -> &[u64] {
        &self.regs[..self.len]
    }
}

/// 从内核中断/异常帧地址读取寄存器快照 (已按 prstatus 顺序映射)
///
/// `frame_addr == 0` 时返回 `None`.
#[cfg(target_arch = "x86_64")]
pub fn read_interrupt_regs(frame_addr: u64) -> Option<RegSnapshot> {
    if frame_addr == 0 {
        return None;
    }
    // SAFETY: frame_addr 由信号投递路径保证指向有效的 InterruptFrame;
    //         以下均按值读取字段, 不构造 packed 字段的引用, 满足对齐要求.
    let frame = unsafe { &*(frame_addr as *const crate::framework::idt::InterruptFrame) };

    let mut regs = [0u64; 34];
    // Linux x86_64 prstatus regset 顺序:
    // 索引 0..14 为通用寄存器 (GPR), 15 orig_rax, 16 rip, 17 cs, 18 rflags, 19 rsp, 20 ss,
    // 21..26 fs_base/gs_base/ds/es/fs/gs (简化填 0)
    regs[0] = frame.r15;
    regs[1] = frame.r14;
    regs[2] = frame.r13;
    regs[3] = frame.r12;
    regs[4] = frame.rbp;
    regs[5] = frame.rbx;
    regs[6] = frame.r11;
    regs[7] = frame.r10;
    regs[8] = frame.r9;
    regs[9] = frame.r8;
    regs[10] = frame.rax;
    regs[11] = frame.rcx;
    regs[12] = frame.rdx;
    regs[13] = frame.rsi;
    regs[14] = frame.rdi;
    regs[15] = frame.rax; // orig_rax (简化: = rax)
    regs[16] = frame.rip;
    regs[17] = frame.cs;
    regs[18] = frame.rflags;
    regs[19] = frame.rsp;
    regs[20] = frame.ss;
    Some(RegSnapshot { regs, len: 27 })
}

/// 从内核异常帧地址读取寄存器快照 (已按 prstatus 顺序映射)
///
/// `frame_addr == 0` 时返回 `None`.
#[cfg(target_arch = "aarch64")]
pub fn read_interrupt_regs(frame_addr: u64) -> Option<RegSnapshot> {
    if frame_addr == 0 {
        return None;
    }
    // SAFETY: frame_addr 由信号投递路径保证指向有效的 ExceptionFrame;
    //         以下均按值读取字段.
    let frame =
        unsafe { &*(frame_addr as *const crate::framework::arch::exception::ExceptionFrame) };

    let mut regs = [0u64; 34];
    regs[0] = frame.x0;
    regs[1] = frame.x1;
    regs[2] = frame.x2;
    regs[3] = frame.x3;
    regs[4] = frame.x4;
    regs[5] = frame.x5;
    regs[6] = frame.x6;
    regs[7] = frame.x7;
    regs[8] = frame.x8;
    regs[9] = frame.x9;
    regs[10] = frame.x10;
    regs[11] = frame.x11;
    regs[12] = frame.x12;
    regs[13] = frame.x13;
    regs[14] = frame.x14;
    regs[15] = frame.x15;
    regs[16] = frame.x16;
    regs[17] = frame.x17;
    regs[18] = frame.x18;
    regs[19] = frame.x19;
    regs[20] = frame.x20;
    regs[21] = frame.x21;
    regs[22] = frame.x22;
    regs[23] = frame.x23;
    regs[24] = frame.x24;
    regs[25] = frame.x25;
    regs[26] = frame.x26;
    regs[27] = frame.x27;
    regs[28] = frame.x28;
    regs[29] = frame.x29; // FP
    regs[30] = frame.x30; // LR
    regs[31] = frame.sp;
    regs[32] = frame.elr; // PC
    regs[33] = frame.spsr; // PSTATE
    Some(RegSnapshot { regs, len: 34 })
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;

    #[test]
    fn test_read_interrupt_regs_null_frame() {
        assert!(read_interrupt_regs(0).is_none());
    }

    #[test]
    fn test_read_interrupt_regs_x86_64_mapping() {
        let frame = crate::framework::idt::InterruptFrame::new_test_frame(13, 0xDEAD_BEEF, 0x33);
        let addr = core::ptr::from_ref(&frame) as u64;
        let snap = read_interrupt_regs(addr).expect("snapshot");
        let regs = snap.as_slice();
        assert_eq!(regs.len(), 27);
        assert_eq!(regs[16], 0xDEAD_BEEF); // rip
        assert_eq!(regs[17], 0x33); // cs
        assert_eq!(regs[18], 0x202); // rflags
        assert_eq!(regs[20], 0x10); // ss
    }
}
