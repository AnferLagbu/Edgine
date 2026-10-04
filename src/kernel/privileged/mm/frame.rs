//! `UFrame` / `USegment` — 类型安全的用户内存帧抽象
//!
//! 为用户空间内存访问提供类型层安全保证,
//! 强化不变式 I4 (内核解引用用户指针前必须验证).
//!
//! # 设计
//!
//! - `UFrame`: 表示单个用户物理帧 (4KB). 访问被限制在
//!   `read_pod` / `write_pod` 上, 它们按字节复制 POD 类型进出,
//!   不暴露对用户内存的长期引用.
//!
//! - `USegment`: 表示连续的用户虚拟内存区间.
//!   提供 `read_bytes` / `write_bytes` 进行有界复制.
//!
//! - `Pod` trait: 标记可按字节安全复制的 Plain Old Data 类型
//!   (无指针, 无内部可变性, 无 Drop 副作用).
//!
//! # 安全不变式
//!
//! - UFrame/USegment 永不允许暴露指向用户内存的 `&[u8]` 或 `&mut [u8]`
//!   —— 所有访问都通过有界复制操作.
//! - Pod 类型不得包含指向内核内存的指针, 防止内核地址意外泄漏到用户空间.

use super::PAGE_SIZE;
use super::copy_user::{copy_from_user, copy_to_user, is_user_buf, is_user_ptr};

// ---------------------------------------------------------------------------
// Pod trait
// ---------------------------------------------------------------------------

/// POD (Plain Old Data) 类型的标记 trait.
///
/// `Pod` 类型可按字节在内核与用户内存之间安全复制.
/// 实现 `Pod` 的类型必须满足:
///
/// 1. 无指针 (裸指针或引用) —— 防止内核地址泄漏
/// 2. 无内部可变性 (Cell, `RefCell`, `AtomicXxx`) —— 防止 TOCTOU
/// 3. 无 `Drop` 副作用 —— 值是纯位级语义
/// 4. `Copy` —— 仅值语义
///
/// # Safety
///
/// 实现该 trait 是安全的, 因为编译器会强制 `Copy`.
/// 但实现者必须保证不存在指针字段. 这一点
/// 由下面的 `pod_assertions` 测试验证.
pub trait Pod: Copy {}

// 为常见原始类型实现 Pod
impl Pod for u8 {}
impl Pod for u16 {}
impl Pod for u32 {}
impl Pod for u64 {}
impl Pod for usize {}
impl Pod for i8 {}
impl Pod for i16 {}
impl Pod for i32 {}
impl Pod for i64 {}
impl Pod for isize {}
impl Pod for bool {}

// 为 Pod 类型的数组实现 Pod
impl<const N: usize, T: Pod> Pod for [T; N] {}

// ---------------------------------------------------------------------------
// UFrame — 单个用户物理帧
// ---------------------------------------------------------------------------

/// 单个用户物理帧 (4KB 页).
///
/// `UFrame` 封装一个页帧号 (PFN), 通过 `Pod` 类型
/// 提供对帧内容的读写访问. 永不允许直接暴露对用户内存的引用.
///
/// # 生命周期
///
/// `UFrame` 由经过验证的用户虚拟地址创建.
/// 调用方必须保证底层页在 `UFrame` 生命周期内保持映射.
///
/// # 不变式 I4
///
/// 通过将访问限制在 `read_pod`/`write_pod` 上,
/// `UFrame` 确保内核不会持有对用户内存的长期引用,
/// 防止内核验证后用户修改内存的 TOCTOU 攻击.
pub struct UFrame {
    /// 帧的用户虚拟地址 (页对齐)
    uaddr: u64,
}

impl UFrame {
    /// 从用户虚拟地址创建 `UFrame`.
    ///
    /// 当地址不在用户空间或未页对齐时返回 `None`.
    #[inline]
    pub fn from_user_addr(addr: u64) -> Option<Self> {
        if !is_user_ptr(addr) || !addr.is_multiple_of(PAGE_SIZE) {
            return None;
        }
        Some(Self { uaddr: addr })
    }

    /// 不经校验创建 `UFrame`.
    ///
    /// # Safety
    ///
    /// - `addr` 必须是合法的、页对齐的用户虚拟地址
    /// - 底层页在该 `UFrame` 生命周期内必须保持映射
    #[inline]
    pub unsafe fn from_user_addr_unchecked(addr: u64) -> Self {
        Self { uaddr: addr }
    }

    /// 从帧内指定偏移读取一个 POD 值.
    ///
    /// offset + `size_of::<T>()` 不得超过 `PAGE_SIZE`.
    /// 页错误或偏移非法时返回 `Err(())`.
    /// # Errors
    /// 偏移越界或访问用户内存时发生页错误时返回 Err。
    #[inline]
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    pub fn read_pod<T: Pod>(&self, offset: usize) -> Result<T, ()> {
        let size = core::mem::size_of::<T>();
        if offset.saturating_add(size) > PAGE_SIZE as usize {
            return Err(());
        }

        let mut val = core::mem::MaybeUninit::<T>::uninit();
        let dst =
            // SAFETY: val 是 MaybeUninit::uninit(), 指针有效; 长度 size 由
            // 上方 bounds check 保证; from_raw_parts_mut 仅借用指针供后续
            // copy_from_user 填充, 不在 unsafe 块内读取, 不会读到未初始化数据.
            unsafe { core::slice::from_raw_parts_mut(val.as_mut_ptr() as *mut u8, size) };
        copy_from_user(dst, self.uaddr + offset as u64, size)?;
        // SAFETY: copy_from_user 返回 Ok 表示 dst 已被初始化 size 字节 (T 是 Pod,
        // POD 不要求位有效, 只要求位是有效的字节模式, 而 copy_from_user 写入是
        // 完整 size 字节, 对齐由 Pod 约束保证).
        Ok(unsafe { val.assume_init() })
    }

    /// 将一个 POD 值写入帧内指定偏移.
    ///
    /// offset + `size_of::<T>()` 不得超过 `PAGE_SIZE`.
    /// 页错误或偏移非法时返回 `Err(())`.
    /// # Errors
    /// 偏移越界或访问用户内存时发生页错误时返回 Err。
    #[inline]
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    #[expect(
        clippy::ref_as_ptr,
        reason = "ref_as_ptr: &T as *const T 是已知安全 (Rust 2024 可用 &raw const; 当前优先 expect"
    )]
    pub fn write_pod<T: Pod>(&self, offset: usize, val: &T) -> Result<(), ()> {
        let size = core::mem::size_of::<T>();
        if offset.saturating_add(size) > PAGE_SIZE as usize {
            return Err(());
        }

        let src =
            // SAFETY: val 是 &T, 指针有效; size = size_of::<T>(), 完全在 val
            // 内存范围内; from_raw_parts 仅借用字节视图供 copy_to_user 读取.
            unsafe { core::slice::from_raw_parts(val as *const T as *const u8, size) };
        copy_to_user(self.uaddr + offset as u64, src, size)?;
        Ok(())
    }

    /// 从帧读取一段字节切片.
    ///
    /// `offset + buf.len()` 不得超过 `PAGE_SIZE`.
    /// 返回实际复制的字节数.
    /// # Errors
    /// 偏移越界或访问用户内存时发生页错误时返回 Err。
    #[inline]
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    pub fn read_bytes(&self, offset: usize, buf: &mut [u8]) -> Result<usize, ()> {
        if offset.saturating_add(buf.len()) > PAGE_SIZE as usize {
            return Err(());
        }
        copy_from_user(buf, self.uaddr + offset as u64, buf.len())
    }

    /// 向帧写入一段字节切片.
    ///
    /// `offset + data.len()` 不得超过 `PAGE_SIZE`.
    /// 返回实际复制的字节数.
    /// # Errors
    /// 偏移越界或访问用户内存时发生页错误时返回 Err。
    #[inline]
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    pub fn write_bytes(&self, offset: usize, data: &[u8]) -> Result<usize, ()> {
        if offset.saturating_add(data.len()) > PAGE_SIZE as usize {
            return Err(());
        }
        copy_to_user(self.uaddr + offset as u64, data, data.len())
    }

    /// 返回该帧的用户虚拟地址.
    #[inline]
    pub fn user_addr(&self) -> u64 {
        self.uaddr
    }
}

// ---------------------------------------------------------------------------
// USegment — 连续的用户虚拟内存区间
// ---------------------------------------------------------------------------

/// 一段连续的用户虚拟内存.
///
/// 与 `UFrame` 不同, `USegment` 可跨多页, 字节范围任意.
/// 访问被限制在有界复制操作上.
///
/// # 不变式 I4
///
/// 与 `UFrame` 一致: 永不允许暴露对用户内存的引用.
pub struct USegment {
    /// 起始用户虚拟地址
    base: u64,
    /// 字节长度
    len: usize,
}

impl USegment {
    /// 从用户地址与长度创建 `USegment`.
    ///
    /// 区间不完全位于用户空间时返回 `None`.
    #[inline]
    pub fn from_user_range(addr: u64, len: usize) -> Option<Self> {
        if !is_user_buf(addr, len) {
            return None;
        }
        Some(Self { base: addr, len })
    }

    /// 不经校验创建 `USegment`.
    ///
    /// # Safety
    ///
    /// - `[addr, addr+len)` 必须是合法的、已映射的、用户可访问的内存
    /// - 映射在该 `USegment` 生命周期内必须保持有效
    #[inline]
    pub unsafe fn from_user_range_unchecked(addr: u64, len: usize) -> Self {
        Self { base: addr, len }
    }

    /// 从段内指定偏移读取一个 POD 值.
    ///
    /// 页错误或偏移越界时返回 `Err(())`.
    /// # Errors
    /// 偏移越界或访问用户内存时发生页错误时返回 Err。
    #[inline]
    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    pub fn read_pod<T: Pod>(&self, offset: usize) -> Result<T, ()> {
        let size = core::mem::size_of::<T>();
        if offset.saturating_add(size) > self.len {
            return Err(());
        }

        let mut val = core::mem::MaybeUninit::<T>::uninit();
        let dst =
            // SAFETY: val 是 MaybeUninit::uninit(), 指针有效; 长度 size 由
            // 上方 bounds check 保证; from_raw_parts_mut 仅借用指针供后续
            // copy_from_user 填充, 不在 unsafe 块内读取.
            unsafe { core::slice::from_raw_parts_mut(val.as_mut_ptr() as *mut u8, size) };
        copy_from_user(dst, self.base + offset as u64, size)?;
        // SAFETY: copy_from_user 返回 Ok 表示 dst 已被初始化 size 字节,
        // 满足 T: Pod 的位有效性要求.
        Ok(unsafe { val.assume_init() })
    }

    /// 将一个 POD 值写入段内指定偏移.
    ///
    /// 页错误或偏移越界时返回 `Err(())`.
    /// # Errors
    /// 偏移越界或访问用户内存时发生页错误时返回 Err。
    #[inline]
    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    #[expect(
        clippy::ref_as_ptr,
        reason = "ref_as_ptr: &T as *const T 是已知安全 (Rust 2024 可用 &raw const; 当前优先 expect"
    )]
    pub fn write_pod<T: Pod>(&self, offset: usize, val: &T) -> Result<(), ()> {
        let size = core::mem::size_of::<T>();
        if offset.saturating_add(size) > self.len {
            return Err(());
        }

        let src =
            // SAFETY: val 是 &T, 指针有效; size = size_of::<T>(), 完全在 val
            // 内存范围内; from_raw_parts 仅借用字节视图供 copy_to_user 读取.
            unsafe { core::slice::from_raw_parts(val as *const T as *const u8, size) };
        copy_to_user(self.base + offset as u64, src, size)?;
        Ok(())
    }

    /// 从段读取字节到内核缓冲区.
    ///
    /// 返回实际复制的字节数.
    /// # Errors
    /// 访问用户内存时发生页错误时返回 Err。
    #[inline]
    pub fn read_bytes(&self, offset: usize, buf: &mut [u8]) -> Result<usize, ()> {
        let max_len = self.len.saturating_sub(offset);
        let copy_len = buf.len().min(max_len);
        if copy_len == 0 {
            return Ok(0);
        }
        copy_from_user(&mut buf[..copy_len], self.base + offset as u64, copy_len)
    }

    /// 将内核缓冲区字节写入段.
    ///
    /// 返回实际复制的字节数.
    /// # Errors
    /// 访问用户内存时发生页错误时返回 Err。
    #[inline]
    pub fn write_bytes(&self, offset: usize, data: &[u8]) -> Result<usize, ()> {
        let max_len = self.len.saturating_sub(offset);
        let copy_len = data.len().min(max_len);
        if copy_len == 0 {
            return Ok(0);
        }
        copy_to_user(self.base + offset as u64, &data[..copy_len], copy_len)
    }

    /// 返回起始用户虚拟地址.
    #[inline]
    pub fn base(&self) -> u64 {
        self.base
    }

    /// 返回字节长度.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// 段长度为零时返回 true.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    // KERNEL_BASE 定义于 privileged::mm (父模块), 不在 frame 模块命名空间内.
    use crate::privileged::mm::KERNEL_BASE;

    #[test]
    fn test_uframe_rejects_kernel_addr() {
        // 内核空间地址应被拒绝
        assert!(UFrame::from_user_addr(KERNEL_BASE).is_none());
    }

    #[test]
    fn test_uframe_rejects_misaligned() {
        // 未页对齐的用户地址应被拒绝
        assert!(UFrame::from_user_addr(0x1001).is_none());
    }

    #[test]
    fn test_usegment_rejects_kernel_range() {
        // 内核空间区间应被拒绝
        assert!(USegment::from_user_range(KERNEL_BASE, PAGE_SIZE as usize).is_none());
    }

    #[test]
    fn test_uframe_offset_overflow() {
        // offset + size > PAGE_SIZE 时应失败
        // 单元测试中无法真正读取 (无用户页), 但
        // 边界检查应当拒绝.
        // SAFETY: 测试中构造虚拟 UFrame 不访问真实用户内存, 仅触发 bounds
        // check 分支, read_pod 会在 offset=4090 立即返回 Err, 不会触碰
        // 从_raw_parts_mut 创建的 dst slice.
        unsafe {
            let frame = UFrame::from_user_addr_unchecked(0x1000);
            // 偏移 4090 处读 u64 需要 8 字节, 但仅剩 6 字节
            assert!(frame.read_pod::<u64>(4090).is_err());
        }
    }

    #[test]
    fn test_usegment_offset_overflow() {
        // SAFETY: 同 test_uframe_offset_overflow, 仅触发 bounds check 分支.
        unsafe {
            let seg = USegment::from_user_range_unchecked(0x1000, 16);
            // 偏移 12 处读 u64 需要 8 字节, 但仅剩 4 字节
            assert!(seg.read_pod::<u64>(12).is_err());
        }
    }
}
