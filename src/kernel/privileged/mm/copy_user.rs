//! 用户空间安全访问函数
//!
//! 提供在用户态与内核态之间安全复制数据的函数.
//! 这些函数会校验用户指针, 并在进入用户内存访问前完成权限预检.
//!
//! # 安全
//!
//! - 访问前校验所有用户指针
//! - 读路径逐页确认已映射; 写路径逐页确认可写 (COW 只读页主动破 COW)
//! - 预检通过后才执行 store/load, 用户传非法指针返回 `KernelError::Fault`
//! - 防止内核访问无效的用户内存
//! - 防止用户读取任意的内核内存
//!
//! # 缺页语义
//!
//! 本模块不再依赖“访问中捕获异常再跳恢复点”: 那类恢复点无法安全跨函数返回.
//! 改为**写前/读前预检** (`userptr::validate_user_writable` / `validate_user_buf`),
//! 把用户可控的缺页在进入 store 前消掉. x86_64 `CR0.WP=1` 下核态写 COW 只读页
//! 曾经直接 `#PF` 停机 (UEFI U-1), 预检是根因修复; #PF 处理器只保留 COW 竞态窄兜底.
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
//! - 访问前的逐页预检保证不会在复制中触发用户缺页

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
#[cfg(all(target_arch = "x86_64", not(feature = "host-test")))]
const CR4_SMAP_BIT: u64 = 1 << 21;

/// 查询当前 CPU 是否已启用 SMAP (CR4.SMAP=1).
#[cfg(all(target_arch = "x86_64", not(feature = "host-test")))]
#[inline]
fn smap_is_enabled() -> bool {
    let cr4: u64;
    // SAFETY: 读取 CR4 是特权操作, 但无副作用且不改变任何体系结构状态.
    unsafe {
        core::arch::asm!("mov {0}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags));
    }
    cr4 & CR4_SMAP_BIT != 0
}

/// host-test 构建运行在宿主用户态: 读 CR4 是特权指令 (SIGSEGV),
/// 且宿主进程没有内核 SMAP 语义, 故恒报未启用 (`smap_begin` / `smap_end` 退化为 no-op).
#[cfg(all(target_arch = "x86_64", feature = "host-test"))]
#[inline]
fn smap_is_enabled() -> bool {
    false
}

/// SAFETY: 调用方保证在 user 内存访问区间内调用, 配对 `smap_end`.
#[cfg(target_arch = "x86_64")]
#[inline]
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
#[inline]
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
#[inline]
pub(crate) unsafe fn smap_begin() {}

// SAFETY: no-op, 不会执行任何特权操作, 调用方配对语义不变.
#[cfg(not(target_arch = "x86_64"))]
#[inline]
pub(crate) unsafe fn smap_end() {}

use crate::privileged::constants::limits::USER_ADDR_MAX;
use crate::privileged::error::KernelError;

/// 与 I5/I6 (外设 DMA/MMIO) 不变式的唯一交叉检查点.
#[inline]
#[must_use]
pub fn is_user_ptr(ptr: u64) -> bool {
    ptr > 0 && ptr < USER_ADDR_MAX
}

/// 检查缓冲区 (ptr + len) 是否完全位于用户空间
#[inline]
#[must_use]
pub fn is_user_buf(ptr: u64, len: usize) -> bool {
    if len == 0 {
        return true;
    }
    let Some(end) = ptr.checked_add(len as u64) else {
        return false;
    };
    is_user_ptr(ptr) && end <= USER_ADDR_MAX
}

/// 从用户空间复制数据到内核缓冲区
///
/// # Safety
///
/// 此函数是安全的, 因为它:
/// 1. 校验用户指针范围
/// 2. 逐页确认用户区间已映射后才访问
/// 3. 非法指针返回 `KernelError::Fault`, 不触发核态缺页
///
/// # 返回
///
/// - `Ok(copied_len)`: 成功复制的字节数
/// - `Err(KernelError::Fault)`: 用户指针非法
/// # Errors
/// 用户指针非法、目标缓冲区过小或访问用户内存时发生缺页时返回 Err。
#[inline(never)]
pub fn copy_from_user(
    kernel_dst: &mut [u8],
    user_src: u64,
    len: usize,
) -> Result<usize, KernelError> {
    if len == 0 {
        return Ok(0);
    }

    if !is_user_buf(user_src, len)
        || !crate::privileged::userptr::validate_user_buf(user_src, len as u64)
    {
        return Err(KernelError::Fault);
    }

    if kernel_dst.len() < len {
        return Err(KernelError::Fault);
    }

    // SAFETY: `is_user_buf(user_src, len)` 已确认 [user_src, user_src+len) 全落在
    // 用户地址空间且逐页已映射, `kernel_dst.len() >= len` 已检查; 源为用户页、
    // 目的为内核 slice, 两区不相交; 预检通过后访问不再触发缺页.
    unsafe {
        smap_begin();
        core::ptr::copy_nonoverlapping(user_src as *const u8, kernel_dst.as_mut_ptr(), len);
        smap_end();
    }
    Ok(len)
}

/// 从内核复制数据到用户空间
///
/// # Safety
///
/// 此函数是安全的, 因为它:
/// 1. 校验用户指针范围
/// 2. 逐页确认用户区间已映射且可写 (COW 只读页先破 COW)
/// 3. 非法/只读指针返回 `KernelError::Fault`, 不触发核态缺页
///
/// # 返回
///
/// - `Ok(copied_len)`: 成功复制的字节数
/// - `Err(KernelError::Fault)`: 用户指针非法
/// # Errors
/// 用户指针非法、源缓冲区过小或访问用户内存时发生缺页时返回 Err。
#[inline(never)]
pub fn copy_to_user(user_dst: u64, kernel_src: &[u8], len: usize) -> Result<usize, KernelError> {
    if len == 0 {
        return Ok(0);
    }

    if !is_user_buf(user_dst, len)
        || !crate::privileged::userptr::validate_user_writable(user_dst, len as u64)
    {
        return Err(KernelError::Fault);
    }

    if kernel_src.len() < len {
        return Err(KernelError::Fault);
    }

    // SAFETY: `is_user_buf` + `validate_user_writable` 已确认 [user_dst, user_dst+len)
    // 全落用户空间、逐页已映射且可写 (COW 只读页已在预检中破), `kernel_src.len() >= len`;
    // 源为内核 slice、目的为用户页, 两区不相交; 预检通过后访问不再触发缺页.
    unsafe {
        smap_begin();
        core::ptr::copy_nonoverlapping(kernel_src.as_ptr(), user_dst as *mut u8, len);
        smap_end();
    }
    Ok(len)
}

/// 从用户空间复制以 NUL 结尾的字符串
///
/// # 返回
///
/// - `Ok(string)`: 复制出的字符串 (不含 NUL 终止符)
/// - `Err(KernelError::Fault)`: 指针非法、未以 NUL 结尾或过长
/// # Errors
/// 指针非法、超过最大长度或字符串非 UTF-8 时返回 Err。
#[inline(never)]
pub fn copy_string_from_user(
    user_str: u64,
    max_len: usize,
) -> Result<alloc::string::String, KernelError> {
    if !is_user_ptr(user_str) {
        return Err(KernelError::Fault);
    }

    let mut bytes = alloc::vec::Vec::with_capacity(max_len);
    let mut faulted = false;
    let mut validated_page = u64::MAX;

    // SAFETY: 起点已确认在用户地址空间; 每跨入新页前先用 `validate_user_buf`
    // 证明该页已映射, 预检通过后才读, 因此不会触发缺页; 目标 `bytes` 独立于用户页.
    unsafe {
        smap_begin();
        for i in 0..max_len {
            let addr = user_str.saturating_add(i as u64);
            let page = addr & !(PAGE_SIZE - 1);
            if page != validated_page {
                if !crate::privileged::userptr::validate_user_buf(addr, 1) {
                    faulted = true;
                    break;
                }
                validated_page = page;
            }
            let byte = core::ptr::read_volatile(addr as *const u8);
            if byte == 0 {
                break;
            }
            bytes.push(byte);
        }
        smap_end();
    }

    if faulted || bytes.len() == max_len {
        Err(KernelError::Fault)
    } else {
        alloc::string::String::from_utf8(bytes).map_err(|_| KernelError::Fault)
    }
}

/// 清除用户空间中某段区域 (填充 0)
///
/// # 返回
///
/// - `Ok(cleared_len)`: 已清零的字节数
/// - `Err(KernelError::Fault)`: 用户指针非法
/// # Errors
/// 用户指针非法或访问用户内存时发生缺页时返回 Err。
#[inline(never)]
pub fn clear_user(user_ptr: u64, len: usize) -> Result<usize, KernelError> {
    if len == 0 {
        return Ok(0);
    }

    if !is_user_buf(user_ptr, len)
        || !crate::privileged::userptr::validate_user_writable(user_ptr, len as u64)
    {
        return Err(KernelError::Fault);
    }

    // SAFETY: `is_user_buf` + `validate_user_writable` 已确认区间全落用户空间、
    // 逐页已映射且可写; 预检通过后写零不再触发缺页.
    unsafe {
        smap_begin();
        core::ptr::write_bytes(user_ptr as *mut u8, 0, len);
        smap_end();
    }
    Ok(len)
}

/// 获取用户空间字符串长度 (不复制内容)
///
/// # 返回
///
/// - `Ok(len)`: 字符串长度 (不含 NUL 终止符)
/// - `Err(KernelError::Fault)`: 指针非法或在 `max_len` 内未找到 NUL 终止符
/// # Errors
/// 指针非法或在最大长度内未找到 NUL 终止符时返回 Err。
#[inline(never)]
pub fn strlen_user(user_str: u64, max_len: usize) -> Result<usize, KernelError> {
    if !is_user_ptr(user_str) {
        return Err(KernelError::Fault);
    }

    let mut found_len = None;
    let mut faulted = false;
    let mut validated_page = u64::MAX;

    // SAFETY: 每跨入新页前先验证该页已映射, 预检通过后才逐字节读; 不会触发缺页.
    unsafe {
        smap_begin();
        for i in 0..max_len {
            let addr = user_str.saturating_add(i as u64);
            let page = addr & !(PAGE_SIZE - 1);
            if page != validated_page {
                if !crate::privileged::userptr::validate_user_buf(addr, 1) {
                    faulted = true;
                    break;
                }
                validated_page = page;
            }
            let byte = core::ptr::read_volatile(addr as *const u8);
            if byte == 0 {
                found_len = Some(i);
                break;
            }
        }
        smap_end();
    }

    if faulted {
        Err(KernelError::Fault)
    } else if let Some(len) = found_len {
        Ok(len)
    } else {
        Err(KernelError::Fault)
    }
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
