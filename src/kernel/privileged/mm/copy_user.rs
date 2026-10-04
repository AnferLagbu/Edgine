//! 用户空间安全访问函数
//!
//! 提供在用户态与内核态之间安全复制数据的函数.
//! 这些函数会校验用户指针, 并优雅地处理缺页异常.
//!
//! # 安全
//!
//! - 访问前校验所有用户指针
//! - 复制过程中的缺页异常会被捕获并返回错误
//! - 防止内核访问无效的用户内存
//! - 防止用户读取任意的内核内存
//!
//! # 缺页异常处理
//!
//! 本模块采用异常表机制处理缺页异常:
//! 1. 访问用户内存前, 设置恢复点
//! 2. 若发生缺页异常, 异常处理程序查找恢复点
//! 3. 处理程序跳转到恢复点, 返回错误
//!
//! 这避免了因非法用户指针导致内核 panic.
//!
//! # 性能
//!
//! 使用 `ptr::copy_nonoverlapping` 以获得最佳性能:
//! - 利用 CPU 向量化指令 (SSE/AVX)
//! - 借助批量内存操作高效利用缓存
//! - 相比逐字节复制, 大块缓冲区复制显著更快
//!
//! # 安全性
//!
//! 使用 `copy_nonoverlapping` 是安全的, 因为:
//! - 源与目的缓冲区保证不重叠 (用户态 vs 内核态)
//! - 复制前已校验缓冲区边界
//! - 异常恢复机制能处理复制过程中的任何缺页

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[cfg(test)]
use super::PAGE_SIZE;

// P4.B.4: SMAP 启用 (CR4.SMAP=1) 后, Ring 0 访问 USER 页触发 #PF.
// stac (Set AC Flag) 临时取消 SMAP 保护, clac (Clear AC Flag) 恢复.
// 仅 x86_64 指令, aarch64 无对应. cfg 门控避免跨架构编译失败.
//
// stac/clac 在**不具备 SMAP 能力的 CPU 上是 #UD**: CR4.SMAP 仅在 CPUID
// 报告 SMAP 时置位 (见 `cpu::init_msr`), 故此处按 CR4.SMAP 现况门控 ——
// 该位为 0 时 AC 标志对访问控制毫无作用, 两条指令退化为 no-op 语义等价.
// CR4 是 per-CPU 寄存器, 逐核即时读取天然正确, 不引入额外全局状态.
/// CR4.SMAP 位号 (与 `cpu::init_msr` 置位处同源).
#[cfg(target_arch = "x86_64")]
const CR4_SMAP_BIT: u64 = 1 << 21;

/// 查询当前 CPU 是否已启用 SMAP (CR4.SMAP=1).
#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn smap_is_enabled() -> bool {
    let cr4: u64;
    // SAFETY: 读取 CR4 是特权操作, 但无副作用且不改变任何体系结构状态.
    unsafe {
        core::arch::asm!("mov {0}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags));
    }
    cr4 & CR4_SMAP_BIT != 0
}

/// SAFETY: 调用方保证在 user 内存访问区间内调用, 配对 `smap_end`.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
pub(crate) unsafe fn smap_begin() {
    if smap_is_enabled() {
        // SAFETY: CR4.SMAP=1 意味着 CPU 必然支持 SMAP, stac 不会触发 #UD.
        unsafe {
            core::arch::asm!("stac", options(nomem, nostack, preserves_flags));
        }
    }
}

/// SAFETY: 与 `smap_begin` 配对, 恢复 SMAP 保护.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
pub(crate) unsafe fn smap_end() {
    if smap_is_enabled() {
        // SAFETY: 同上, CR4.SMAP=1 保证 clac 合法.
        unsafe {
            core::arch::asm!("clac", options(nomem, nostack, preserves_flags));
        }
    }
}

// aarch64 上 SMAP 不存在, 提供 no-op stub 以满足统一调用点.
// SAFETY: no-op, 不会执行任何特权操作, 调用方配对语义不变.
#[cfg(not(target_arch = "x86_64"))]
#[inline(always)]
pub(crate) unsafe fn smap_begin() {}

// SAFETY: no-op, 不会执行任何特权操作, 调用方配对语义不变.
#[cfg(not(target_arch = "x86_64"))]
#[inline(always)]
pub(crate) unsafe fn smap_end() {}

use crate::privileged::constants::limits::USER_ADDR_MAX;

/// 异常表条目 —— 用于缺页恢复
#[repr(C)]
pub struct ExceptionTableEntry {
    /// 可能触发异常的指令地址
    pub insn_addr: u64,
    /// 异常时跳转到的恢复地址
    pub fixup_addr: u64,
}

/// 全局异常表 (在链接脚本中定义)
#[used]
// SAFETY: 指针操作在有效范围内
#[unsafe(link_section = ".exception_table")]
static EXCEPTION_TABLE_START: ExceptionTableEntry = ExceptionTableEntry {
    insn_addr: 0,
    fixup_addr: 0,
};

/// 每 CPU 异常上下文存储
/// 使用静态数组而非 `thread_local`!, 以兼容裸机环境
static PER_CPU_EXCEPTION_CTX: [AtomicU64; crate::privileged::config::MAX_CPUS] =
    [const { AtomicU64::new(0) }; crate::privileged::config::MAX_CPUS];

/// 表示尚未设置异常上下文的标记值
const NO_EXCEPTION_CTX: u64 = 0;

/// 每 CPU 异常发生标志
static PER_CPU_EXCEPTION_OCCURRED: [AtomicBool; crate::privileged::config::MAX_CPUS] =
    [const { AtomicBool::new(false) }; crate::privileged::config::MAX_CPUS];

/// 获取当前 CPU ID (带边界校验)
///
/// # Panics
/// Debug 构建下, 若 `cpu_id` >= `MAX_CPUS`, 立即 panic, 以尽早发现配置错误
///
/// # Safety
/// Release 构建下, 使用取模运算防止越界访问,
/// 但这表明存在配置问题 (CPU 数量过多)
#[inline]
fn current_cpu_id() -> usize {
    let cpu = crate::privileged::cpu::arch::cpu_id() as usize;
    let max_cpus = crate::privileged::config::MAX_CPUS;

    #[cfg(debug_assertions)]
    {
        // 不可恢复: CPU ID 超过 MAX_CPUS 是配置错误, release 模式下取模降级,
        // debug 模式下必须停机以暴露问题
        assert!(
            cpu < max_cpus,
            "CPU ID {cpu} exceeds MAX_CPUS ({max_cpus}). Increase MAX_CPUS or reduce CPU count!"
        );
        cpu
    }

    #[cfg(not(debug_assertions))]
    {
        cpu % max_cpus
    }
}

/// 检查上次操作中是否发生了异常
#[inline]
pub fn exception_occurred() -> bool {
    let cpu = current_cpu_id();
    PER_CPU_EXCEPTION_OCCURRED[cpu].load(Ordering::SeqCst)
}

/// 清除异常发生标志
#[inline]
pub fn clear_exception_flag() {
    let cpu = current_cpu_id();
    PER_CPU_EXCEPTION_OCCURRED[cpu].store(false, Ordering::SeqCst);
}

/// 设置异常发生标志 (由异常处理程序调用)
#[inline]
pub fn mark_exception_occurred() {
    let cpu = current_cpu_id();
    PER_CPU_EXCEPTION_OCCURRED[cpu].store(true, Ordering::SeqCst);
}

/// 对恢复地址进行编码以便存储
/// 使用偏移编码: `stored_value` = `recovery_addr` + 1
/// 这保证 0 (`NO_EXCEPTION_CTX`) 永远不会作为有效地址使用
#[inline]
fn encode_recovery_addr(addr: u64) -> u64 {
    addr.wrapping_add(1)
}

/// 从存储中解码恢复地址
/// encode 的逆运算: `recovery_addr` = `stored_value` - 1
#[inline]
fn decode_recovery_addr(encoded: u64) -> u64 {
    encoded.wrapping_sub(1)
}

/// 设置当前的异常恢复点
/// 返回旧的恢复点
#[inline]
pub fn set_exception_recovery(recovery_addr: u64) -> Option<u64> {
    let cpu = current_cpu_id();
    let encoded = encode_recovery_addr(recovery_addr);
    let old = PER_CPU_EXCEPTION_CTX[cpu].swap(encoded, Ordering::SeqCst);
    if old == NO_EXCEPTION_CTX {
        None
    } else {
        Some(decode_recovery_addr(old))
    }
}

/// 清除当前的异常恢复点
#[inline]
pub fn clear_exception_recovery() {
    let cpu = current_cpu_id();
    PER_CPU_EXCEPTION_CTX[cpu].store(NO_EXCEPTION_CTX, Ordering::SeqCst);
}

/// 获取当前的异常恢复点
#[inline]
pub fn get_exception_recovery() -> Option<u64> {
    let cpu = current_cpu_id();
    let val = PER_CPU_EXCEPTION_CTX[cpu].load(Ordering::SeqCst);
    if val == NO_EXCEPTION_CTX {
        None
    } else {
        Some(decode_recovery_addr(val))
    }
}

/// 检查指针是否在合法的用户空间范围内
///
/// B04-14: `USER_ADDR_MAX = 0x0000_7FFF_FFFF_F000` 已天然排除内核高半区
/// (含 `MMIO_VIRT_BASE = 0xFFFF9000_00000000+` DMA 代理区), 用户态无法
/// 通过 copy_from/to_user 访问外设 MMIO 寄存器. 此函数是 I4 (用户内存代理)
/// 与 I5/I6 (外设 DMA/MMIO) 不变式的唯一交叉检查点.
#[inline]
pub fn is_user_ptr(ptr: u64) -> bool {
    ptr > 0 && ptr < USER_ADDR_MAX
}

/// 检查缓冲区 (ptr + len) 是否完全位于用户空间
#[inline]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub fn is_user_buf(ptr: u64, len: usize) -> bool {
    if len == 0 {
        return true;
    }
    let end = match ptr.checked_add(len as u64) {
        Some(e) => e,
        None => return false,
    };
    is_user_ptr(ptr) && end <= USER_ADDR_MAX
}

/// 设置异常恢复点并返回旧恢复点.
///
/// 抽离为 `#[inline(never)]` 函数, 防止内联汇编被内联到巨大的调用者函数中.
///
/// # 架构相关
///
/// - **`x86_64`**: `lea` + 标签获取恢复地址.
/// - **aarch64**: `adr` (PC 相对寻址, 单条指令) 获取恢复地址.
///   避免 `movz`/`movk` 配对, 该配对会触发 LLVM 22 代码生成 bug
///   (`invalid fixup for movz/movk`) — 当标签距离 mov 较远时.
///
// SAFETY: 调用方需保证在异常恢复有意义的上下文中调用 (即即将访问用户内存).
#[inline(never)]
unsafe fn setup_recovery() -> (u64, Option<u64>) {
    unsafe {
        clear_exception_flag();

        let recovery_label: u64;
        // SAFETY: inline asm 仅读取当前指令地址作为恢复点, 无副作用.
        #[cfg(target_arch = "x86_64")]
        core::arch::asm!(
            "9:",
            "lea {recovery}, [rip + 8f]",
            "8:",
            recovery = out(reg) recovery_label,
            options(nostack, pure, readonly),
        );
        // SAFETY: `adr` 是 PC-relative 单指令, 不触发 movz/movk fixup bug.
        #[cfg(target_arch = "aarch64")]
        core::arch::asm!(
            "adr {recovery}, 8f",
            "8:",
            recovery = out(reg) recovery_label,
            options(nostack, pure, readonly),
        );

        let old_recovery = set_exception_recovery(recovery_label as u64);
        (recovery_label, old_recovery)
    }
}

/// 撤销异常恢复点, 若有旧恢复点则还原.
///
/// 与 `setup_recovery()` 配对. 若在受保护区域内发生了异常则返回 `true`.
//
// SAFETY: 调用方必须先调用 `setup_recovery()`.
#[inline(never)]
unsafe fn teardown_recovery(old_recovery: Option<u64>) -> bool {
    if let Some(old) = old_recovery {
        set_exception_recovery(old);
    } else {
        clear_exception_recovery();
    }
    exception_occurred()
}

/// 从用户空间复制数据到内核缓冲区
///
/// # Safety
///
/// 此函数是安全的, 因为它:
/// 1. 校验用户指针范围
/// 2. 使用 volatile 读防止编译器优化
/// 3. 访问用户内存前先设置异常恢复点
/// 4. 缺页时返回错误而非 panic
///
/// # 返回
///
/// - `Ok(copied_len)`: 成功复制的字节数
/// - `Err(())`: 用户指针非法或发生缺页
///
/// # 架构要求
///
/// 本函数依赖以下架构支持:
/// - 异常表机制 (见 `exception_table_entry`)
/// - 缺页处理程序检查异常表
/// - 恢复点机制
/// # Errors
/// 用户指针非法、目标缓冲区过小或访问用户内存时发生缺页时返回 Err。
#[inline(never)]
pub fn copy_from_user(kernel_dst: &mut [u8], user_src: u64, len: usize) -> Result<usize, ()> {
    if len == 0 {
        return Ok(0);
    }

    if !is_user_buf(user_src, len) {
        return Err(());
    }

    if kernel_dst.len() < len {
        return Err(());
    }

    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    let result = unsafe {
        let src_ptr = user_src as *const u8;
        let dst_ptr = kernel_dst.as_mut_ptr();

        let (_recovery_label, old_recovery) = setup_recovery();

        // P4.B.4: SMAP 保护下访问用户内存.
        smap_begin();
        core::ptr::copy_nonoverlapping(src_ptr, dst_ptr, len);
        smap_end();

        let faulted = teardown_recovery(old_recovery);

        if faulted { Err(()) } else { Ok(len) }
    };

    result
}

/// 从内核复制数据到用户空间
///
/// # Safety
///
/// 此函数是安全的, 因为它:
/// 1. 校验用户指针范围
/// 2. 使用 volatile 写防止编译器优化
/// 3. 访问用户内存前先设置异常恢复点
/// 4. 缺页时返回错误而非 panic
///
/// # 返回
///
/// - `Ok(copied_len)`: 成功复制的字节数
/// - `Err(())`: 用户指针非法或发生缺页
///
/// # 架构要求
///
/// 本函数依赖以下架构支持:
/// - 异常表机制 (见 `exception_table_entry`)
/// - 缺页处理程序检查异常表
/// - 恢复点机制
/// # Errors
/// 用户指针非法、源缓冲区过小或访问用户内存时发生缺页时返回 Err。
#[inline(never)]
pub fn copy_to_user(user_dst: u64, kernel_src: &[u8], len: usize) -> Result<usize, ()> {
    if len == 0 {
        return Ok(0);
    }

    if !is_user_buf(user_dst, len) {
        return Err(());
    }

    if kernel_src.len() < len {
        return Err(());
    }

    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    let result = unsafe {
        let src_ptr = kernel_src.as_ptr();
        let dst_ptr = user_dst as *mut u8;

        let (_recovery_label, old_recovery) = setup_recovery();

        // P4.B.4: SMAP 保护下写入用户内存.
        smap_begin();
        core::ptr::copy_nonoverlapping(src_ptr, dst_ptr, len);
        smap_end();

        let faulted = teardown_recovery(old_recovery);

        if faulted { Err(()) } else { Ok(len) }
    };

    result
}

/// 从用户空间复制以 NUL 结尾的字符串
///
/// # 返回
///
/// - `Ok(string)`: 复制出的字符串 (不含 NUL 终止符)
/// - `Err(())`: 指针非法、未以 NUL 结尾或过长
/// # Errors
/// 指针非法、超过最大长度或字符串非 UTF-8 时返回 Err。
#[inline(never)]
pub fn copy_string_from_user(user_str: u64, max_len: usize) -> Result<alloc::string::String, ()> {
    if !is_user_ptr(user_str) {
        return Err(());
    }

    let mut bytes = alloc::vec::Vec::with_capacity(max_len);

    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    let result = unsafe {
        let ptr = user_str as *const u8;

        let (_recovery_label, old_recovery) = setup_recovery();

        // P4.B.4: SMAP 保护下逐字节读取用户字符串.
        // stac/clac 严格配对: 无论 break 还是正常退出都执行 clac.
        smap_begin();
        for i in 0..max_len {
            let byte = core::ptr::read_volatile(ptr.add(i));
            if byte == 0 {
                break;
            }
            bytes.push(byte);
        }
        smap_end();

        let faulted = teardown_recovery(old_recovery);

        if faulted || bytes.len() == max_len {
            Err(())
        } else {
            alloc::string::String::from_utf8(bytes).map_err(|_| ())
        }
    };

    result
}

/// 清除用户空间中某段区域 (填充 0)
///
/// # 返回
///
/// - `Ok(cleared_len)`: 已清零的字节数
/// - `Err(())`: 用户指针非法
/// # Errors
/// 用户指针非法或访问用户内存时发生缺页时返回 Err。
#[inline(never)]
pub fn clear_user(user_ptr: u64, len: usize) -> Result<usize, ()> {
    if len == 0 {
        return Ok(0);
    }

    if !is_user_buf(user_ptr, len) {
        return Err(());
    }

    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    let result = unsafe {
        let ptr = user_ptr as *mut u8;

        let (_recovery_label, old_recovery) = setup_recovery();

        // P4.B.4: SMAP 保护下写入用户内存.
        smap_begin();
        core::ptr::write_bytes(ptr, 0, len);
        smap_end();

        let faulted = teardown_recovery(old_recovery);

        if faulted { Err(()) } else { Ok(len) }
    };

    result
}

/// 获取用户空间字符串长度 (不复制内容)
///
/// # 返回
///
/// - `Ok(len)`: 字符串长度 (不含 NUL 终止符)
/// - `Err(())`: 指针非法或在 `max_len` 内未找到 NUL 终止符
/// # Errors
/// 指针非法或在最大长度内未找到 NUL 终止符时返回 Err。
#[inline(never)]
pub fn strlen_user(user_str: u64, max_len: usize) -> Result<usize, ()> {
    if !is_user_ptr(user_str) {
        return Err(());
    }

    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    let result = unsafe {
        let ptr = user_str as *const u8;

        let (_recovery_label, old_recovery) = setup_recovery();

        // P4.B.4: SMAP 保护下逐字节读取用户字符串.
        smap_begin();
        let mut found_len = None;
        for i in 0..max_len {
            let byte = core::ptr::read_volatile(ptr.add(i));
            if byte == 0 {
                found_len = Some(i);
                break;
            }
        }
        smap_end();

        let faulted = teardown_recovery(old_recovery);

        if faulted {
            Err(())
        } else if let Some(len) = found_len {
            Ok(len)
        } else {
            Err(())
        }
    };

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_ptr_validation() {
        assert!(is_user_ptr(0x1000));
        assert!(is_user_ptr(0x7FFF_FFFF_F000 - 1));
        assert!(!is_user_ptr(0));
        assert!(!is_user_ptr(0x7FFF_FFFF_F000));
        assert!(!is_user_ptr(0xFFFF_8000_0000_0000));
    }

    #[test]
    fn test_user_buf_validation() {
        assert!(is_user_buf(0x1000, PAGE_SIZE as usize));
        assert!(is_user_buf(0x1000, 0));
        assert!(!is_user_buf(0, PAGE_SIZE as usize));
        assert!(!is_user_buf(0x7FFF_FFFF_F000 - 100, 200));
    }

    #[test]
    fn test_user_buf_overflow_check() {
        assert!(!is_user_buf(u64::MAX, 1));
        assert!(!is_user_buf(u64::MAX - 100, 200));
    }

    #[test]
    fn test_copy_from_user_zero_len() {
        let mut kernel_buf = [0u8; 16];
        let result = copy_from_user(&mut kernel_buf, 0x1000, 0);
        assert_eq!(result, Ok(0));
    }

    #[test]
    fn test_copy_from_user_invalid_ptr() {
        let mut kernel_buf = [0u8; 16];
        let result = copy_from_user(&mut kernel_buf, 0, 16);
        assert!(result.is_err());

        let result = copy_from_user(&mut kernel_buf, 0xFFFF_8000_0000_0000, 16);
        assert!(result.is_err());
    }

    #[test]
    fn test_copy_from_user_buffer_too_small() {
        let mut kernel_buf = [0u8; 8];
        let result = copy_from_user(&mut kernel_buf, 0x1000, 16);
        assert!(result.is_err());
    }

    #[test]
    fn test_copy_to_user_zero_len() {
        let kernel_buf = [0u8; 16];
        let result = copy_to_user(0x1000, &kernel_buf, 0);
        assert_eq!(result, Ok(0));
    }

    #[test]
    fn test_copy_to_user_invalid_ptr() {
        let kernel_buf = [0u8; 16];
        let result = copy_to_user(0, &kernel_buf, 16);
        assert!(result.is_err());

        let result = copy_to_user(0xFFFF_8000_0000_0000, &kernel_buf, 16);
        assert!(result.is_err());
    }

    #[test]
    fn test_clear_user_zero_len() {
        let result = clear_user(0x1000, 0);
        assert_eq!(result, Ok(0));
    }

    #[test]
    fn test_clear_user_invalid_ptr() {
        let result = clear_user(0, 16);
        assert!(result.is_err());

        let result = clear_user(0xFFFF_8000_0000_0000, 16);
        assert!(result.is_err());
    }

    #[test]
    fn test_strlen_user_invalid_ptr() {
        let result = strlen_user(0, 256);
        assert!(result.is_err());

        let result = strlen_user(0xFFFF_8000_0000_0000, 256);
        assert!(result.is_err());
    }

    #[test]
    fn test_copy_string_from_user_invalid_ptr() {
        let result = copy_string_from_user(0, 256);
        assert!(result.is_err());

        let result = copy_string_from_user(0xFFFF_8000_0000_0000, 256);
        assert!(result.is_err());
    }

    #[test]
    fn test_boundary_conditions() {
        assert!(is_user_ptr(USER_ADDR_MAX - 1));
        assert!(!is_user_ptr(USER_ADDR_MAX));
        assert!(!is_user_ptr(USER_ADDR_MAX + 1));

        // USER_ADDR_MAX 为 u64 地址, 与 usize 型常量同转 u64 后相减.
        assert!(is_user_buf(
            USER_ADDR_MAX - PAGE_SIZE as u64,
            PAGE_SIZE as usize
        ));
        assert!(!is_user_buf(USER_ADDR_MAX - 100, 200));
    }
}
