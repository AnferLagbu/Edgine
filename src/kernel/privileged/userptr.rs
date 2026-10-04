//! `UserPtr` — 安全封装用户态裸指针的读写 (TCB)
//!
//! ## 设计目标
//!
//! 在框内核架构中，C-ABI 入口 (`#[no_mangle] fn`) 接收来自 syscall 层或用户态的裸指针。
//! 这些裸指针必须在 TCB (privileged) 中转换为安全的 Rust 类型，然后传递给 functions 层。
//!
//! `UserReadPtr` / `UserWritePtr` 封装 `unsafe { from_raw_parts }` 操作，
//! 将不安全转换集中在 privileged，对外暴露安全 API。
//!
//! ## 与 Asterinas OSTD 的关系
//!
//! Asterinas 的 `UserSpace` trait 提供 `read_val`/`write_val` 等方法，
//! 本模块是轻量等价物：仅做指针→切片的生命周期安全转换，不主动拷贝数据。
//!
//! ## 安全契约（调用方责任）
//!
//! 构造 `UserReadPtr` / `UserWritePtr` 仍需要 `unsafe`，调用方必须确保：
//! 1. 指针指向有效的用户态虚拟地址
//! 2. 地址范围内存已映射且可读（/可写）
//! 3. 在切片使用期间，该内存不会被释放或重新映射
//!
//! 在 C-ABI 层级（syscall dispatcher → VFS api），这些条件由 asm stub + 页表机制保证。
//!
//! ## 使用示例
//!
//! ```rust,ignore
//! // 当前 (unsafe 散布):
//! let buf_slice = unsafe { core::slice::from_raw_parts_mut(buf, count as usize) };
//! fs.read(fd, buf_slice, offset);
//!
//! // 框内核 (unsafe 集中在构造):
//! let mut user_buf = unsafe { UserWritePtr::new(buf, count as usize) };
//! fs.read(fd, user_buf.as_mut_slice(), offset);
//! ```

use core::slice;

use crate::privileged::mm::copy_user::{smap_begin, smap_end};

/// 用户态只读字节指针
///
/// 封装 `*const u8` → `&[u8]` 的 unsafe 转换。
/// 构造时 unsafe，使用 (`as_slice()`) 时 safe。
pub struct UserReadPtr {
    ptr: *const u8,
    len: usize,
}

impl UserReadPtr {
    /// 从裸指针构造。
    ///
    /// # Safety
    ///
    /// 调用方必须确保 `ptr` 指向至少 `len` 字节的有效、已映射、可读的用户态内存，
    /// 且在返回的 `UserReadPtr` 存活期间该内存不会被释放或重新映射。
    pub unsafe fn new(ptr: *const u8, len: usize) -> Self {
        // SAFETY: 此函数标记为 `unsafe fn`, 构造时调用方必须保证 (按 doc comment):
        //   1. `ptr` 是非空且已对齐 (`*const u8` 对齐要求为 1, 总是满足)
        //   2. `ptr..ptr+len` 全部在用户态页表映射中, 可读
        //   3. 该内存区域在 `UserReadPtr` 存活期间不会被释放/重新映射/unmap
        // (典型路径: `UserMode::validate_user_buf` 在 syscall 入口已做检查)
        Self { ptr, len }
    }

    /// 从裸指针安全构造 (先经范围校验)。
    ///
    /// 内部先执行 `validate_user_buf` 地址范围校验, 通过后构造 `UserReadPtr`。
    /// 相比 `new`, 本方法在构造时即拦截非法地址 (NULL / 越界), 调用方无需
    /// 自行校验。注意: 范围校验不验证页表映射存在性, 与 `new` 的契约一致
    /// (映射保证由调用方/页表机制承担, 见模块 doc)。
    ///
    /// # 返回
    /// 指针为 NULL 或超出用户地址空间范围时返回 `None`。
    pub fn checked_new(ptr: *const u8, len: usize) -> Option<Self> {
        if !validate_user_buf(ptr as u64, len as u64) {
            return None;
        }
        // SAFETY: validate_user_buf 已校验 ptr 非空且 [ptr, ptr+len) 在用户地址
        // 空间范围内; 与 `new` 的构造契约一致 (页表映射存在性由调用方/页表机制保证)。
        Some(unsafe { Self::new(ptr, len) })
    }

    /// 以不可变字节切片形式访问用户内存。
    ///
    /// 此方法是安全的，因为构造时的 `unsafe` 契约已保证了指针有效性。
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: 构造时的 unsafe 契约保证 ptr + len 是有效的用户态内存
        // `slice::from_raw_parts` 要求: 1) ptr 对齐 (u8 = 1, ok), 2) len 字节全可读,
        // 3) ptr 非空 (由 new() 调用方保证), 4) 期间无并发写 (与 Rust `&` 借用保证对齐)
        unsafe { slice::from_raw_parts(self.ptr, self.len) }
    }

    /// 检查指针是否为空。
    pub fn is_null(&self) -> bool {
        self.ptr.is_null()
    }

    /// 获取字节长度。
    pub fn len(&self) -> usize {
        self.len
    }

    /// 长度为零
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// 用户态可写字节指针
///
/// 封装 `*mut u8` → `&mut [u8]` 的 unsafe 转换。
/// 构造时 unsafe，使用 (`as_mut_slice()`) 时 safe。
pub struct UserWritePtr {
    ptr: *mut u8,
    len: usize,
}

impl UserWritePtr {
    /// 从裸指针构造。
    ///
    /// # Safety
    ///
    /// 调用方必须确保 `ptr` 指向至少 `len` 字节的有效、已映射、可写的用户态内存，
    /// 且在返回的 `UserWritePtr` 存活期间该内存不会被释放或重新映射。
    pub unsafe fn new(ptr: *mut u8, len: usize) -> Self {
        // SAFETY: 同 `UserReadPtr::new`, 但需可写 (非只读); 调用方需额外保证:
        //   1. 内存映射权限含 W (页表 PTE_RW=1)
        //   2. 在 `UserWritePtr` 存活期间不会有其他 writer (独占 `&mut` 借用)
        Self { ptr, len }
    }

    /// 从裸指针安全构造 (先经范围校验)。
    ///
    /// 内部先执行 `validate_user_buf` 地址范围校验, 通过后构造 `UserWritePtr`。
    /// 相比 `new`, 本方法在构造时即拦截非法地址 (NULL / 越界), 调用方无需
    /// 自行校验。注意: 范围校验不验证页表映射存在性, 与 `new` 的契约一致
    /// (映射保证由调用方/页表机制承担, 见模块 doc)。
    ///
    /// # 返回
    /// 指针为 NULL 或超出用户地址空间范围时返回 `None`。
    pub fn checked_new(ptr: *mut u8, len: usize) -> Option<Self> {
        if !validate_user_buf(ptr as u64, len as u64) {
            return None;
        }
        // SAFETY: validate_user_buf 已校验 ptr 非空且 [ptr, ptr+len) 在用户地址
        // 空间范围内; 与 `new` 的构造契约一致 (页表映射存在性由调用方/页表机制保证)。
        Some(unsafe { Self::new(ptr, len) })
    }

    /// 以可变字节切片形式访问用户内存。
    ///
    /// 此方法是安全的，因为构造时的 `unsafe` 契约已保证了指针有效性。
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: 构造时的 unsafe 契约保证 ptr + len 是有效的可写用户态内存
        // `&mut self` 提供独占借用, 保证构造到 as_mut_slice 之间无其他路径
        // 同时持有 `&mut` 引用, 因此 `from_raw_parts_mut` 安全。
        unsafe { slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    #[expect(
        clippy::ptr_cast_constness,
        reason = "ptr_cast_constness: *mut T as *const T 是已知安全 (Rust 2024 可用 ptr.cast_const 或 &raw const; 当前优先 expect"
    )]
    /// 以不可变字节切片形式访问用户内存。
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: 构造时的 unsafe 契约保证 ptr + len 是有效的用户态内存
        // 从 `*mut u8` cast 到 `*const u8` 保持对齐 (1), 借用规则防止并发写
        unsafe { slice::from_raw_parts(self.ptr as *const u8, self.len) }
    }

    /// 检查指针是否为空。
    pub fn is_null(&self) -> bool {
        self.ptr.is_null()
    }

    /// 获取字节长度。
    pub fn len(&self) -> usize {
        self.len
    }

    /// 长度为零
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// 用户态结构体指针（可写）
///
/// 封装 `*mut T` → `&mut T` 的 unsafe 转换，用于 stat/fstat/getdents 等
/// 需要向用户态 struct 写入完整结构的场景。
pub struct UserRefMut<T> {
    ptr: *mut T,
}

impl<T> UserRefMut<T> {
    /// 从裸指针构造。
    ///
    /// # Safety
    ///
    /// 调用方必须确保 `ptr` 指向有效的、已映射、可写的用户态 `T` 实例。
    pub unsafe fn new(ptr: *mut T) -> Self {
        // SAFETY: 调用方必须保证 (按 doc comment):
        //   1. `ptr` 已对齐到 `align_of::<T>()`
        //   2. `ptr` 指向的用户态内存已映射, 含 W 权限
        //   3. 大小至少 `size_of::<T>()`
        //   4. 期间无其他 writer (独占 `&mut` 借用)
        Self { ptr }
    }

    /// 获取对用户态结构体的可变引用。
    ///
    /// 此方法是安全的，因为构造时的 `unsafe` 契约已保证了指针有效性。
    // 2026-09-11 实测: as_mut 触发 should_implement_trait, 需豁免
    #[allow(clippy::should_implement_trait)]
    pub fn as_mut(&mut self) -> &mut T {
        // SAFETY: 构造时的 unsafe 契约保证 ptr 是有效的可写 T
        // `&mut self` 提供独占借用, 保证 `&mut *self.ptr` 期间无其他引用
        unsafe { &mut *self.ptr }
    }

    /// 检查指针是否为空。
    pub fn is_null(&self) -> bool {
        self.ptr.is_null()
    }
}

// ============================================================================
// 用户指针验证 — 从 syscall::raw 提取到统一入口
// ============================================================================

use crate::privileged::constants::limits::USER_ADDR_MAX;

/// 验证单个用户态指针是否在合法范围.
#[inline]
pub fn validate_user_ptr(ptr: u64) -> bool {
    ptr > 0 && ptr < USER_ADDR_MAX
}

/// 验证用户态缓冲区 [ptr, ptr+len) 是否完全在用户空间.
///
/// # B03-21 修复
/// 之前 `len == 0` 直接 `return true`, `validate_user_buf(0, 0)` 返回 true
/// 但 0 是 NULL 指针, 绕过 NULL 检查。现在 `len == 0` 时仍校验 `ptr != 0`。
/// 语义: 零长度缓冲区只接受 NULL 之外的合法用户指针 (与 Linux copy_from_user
/// 行为一致 — 长度 0 时仍要求非 NULL 指针)。
#[inline]
pub fn validate_user_buf(ptr: u64, len: u64) -> bool {
    if len == 0 {
        // 零长度但要求 ptr 非 NULL (避免 (NULL, 0) 绕过校验)
        return ptr != 0;
    }
    validate_user_ptr(ptr) && ptr + len <= USER_ADDR_MAX
}

/// Safe 包装: 向用户空间写入一个 Copy 结构体.
///
/// 内部先 `validate_user_buf` 验证, 再 `ptr::write_unaligned` 写入.
/// P4.B.5: SMAP 保护下用 stac/clac 包裹 `write_unaligned` (Linux 内核惯用模式).
pub fn write_struct_to_user<T: Copy>(dst_ptr: u64, src: &T) -> bool {
    if dst_ptr == 0 {
        return false;
    }
    let size = core::mem::size_of::<T>() as u64;
    if !validate_user_buf(dst_ptr, size) {
        return false;
    }
    // SAFETY: validate_user_buf 已验证 dst_ptr 指向的 user 缓冲
    // 至少有 size_of::<T>() 字节可写, 且 src 持有有效 T 值.
    // P4.B.5: SMAP 启用时, write_unaligned 必须 stac/clac 包裹.
    unsafe {
        smap_begin();
        core::ptr::write_unaligned(dst_ptr as *mut T, *src);
        smap_end();
    }
    true
}

/// Safe 包装: 从用户空间读取一个 Copy 结构体.
///
/// 内部先 `validate_user_buf` 验证, 再 `ptr::read_unaligned` 读取.
/// P4.B.5: SMAP 保护下用 stac/clac 包裹 `read_unaligned`.
pub fn read_struct_from_user<T: Copy>(src_ptr: u64, dst: &mut T) -> bool {
    if src_ptr == 0 {
        return false;
    }
    let size = core::mem::size_of::<T>() as u64;
    if !validate_user_buf(src_ptr, size) {
        return false;
    }
    // SAFETY: validate_user_buf 已验证 src_ptr 指向的 user 缓冲
    // 至少有 size_of::<T>() 字节可读.
    // P4.B.5: SMAP 启用时, read_unaligned 必须 stac/clac 包裹.
    unsafe {
        smap_begin();
        *dst = core::ptr::read_unaligned(src_ptr as *const T);
        smap_end();
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_checked_new_read_rejects_null() {
        assert!(UserReadPtr::checked_new(core::ptr::null(), 1).is_none());
        assert!(UserReadPtr::checked_new(core::ptr::null(), 0).is_none());
    }

    #[test]
    fn test_checked_new_read_rejects_out_of_range() {
        // [USER_ADDR_MAX-1, +2) 越过用户地址空间上限
        let p = (USER_ADDR_MAX - 1) as *const u8;
        assert!(UserReadPtr::checked_new(p, 2).is_none());
    }

    #[test]
    fn test_checked_new_read_accepts_valid() {
        // 零长度但非 NULL: 合法 (与 validate_user_buf 语义一致)
        let p = 0x1000 as *const u8;
        assert!(UserReadPtr::checked_new(p, 0).is_some());
        assert!(UserReadPtr::checked_new(p, 128).is_some());
    }

    #[test]
    fn test_checked_new_write_rejects_null() {
        assert!(UserWritePtr::checked_new(core::ptr::null_mut(), 1).is_none());
    }

    #[test]
    fn test_checked_new_write_accepts_valid() {
        let p = 0x2000 as *mut u8;
        assert!(UserWritePtr::checked_new(p, 64).is_some());
    }
}
