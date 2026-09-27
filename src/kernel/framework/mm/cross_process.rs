//! 跨进程用户内存安全代理
//!
//! 为 `process_vm_readv` / `process_vm_writev` 等需要访问**其他进程**地址空间的
//! 系统调用提供安全拷贝原语, 是安全不变式 I4 (用户内存只能经 framework 安全代理
//! 访问) 在跨进程场景的落地实现.
//!
//! ## 机制
//!
//! 与同进程的 [`super::copy_user::copy_from_user`] / `copy_to_user` 不同, 跨进程
//! 访问不能依赖当前 CR3, 故:
//! 1. 用 `vmm::translate_in_pml4(target_cr3, va)` 把目标进程虚拟地址逐页翻译为物理地址;
//! 2. 经 HHDM 别名 (`PhysAddr::to_virt()`, 即 `phys + KERNEL_BASE`) 访问该物理页.
//!
//! 之所以**不切换 CR3**: KPTI 下切到目标用户 CR3 执行内核代码不安全 (需同步切换
//! 内核栈与临时映射); 而 HHDM 对用户物理页的映射 `U/S=0` 属内核映射, 不受 SMAP
//! 拦截, 也无需异常表恢复点 —— 页未映射时翻译直接返回 `None`, 根本不会解引用
//! 非法指针.
//!
//! ## 写方向的权限校验
//!
//! 写目标进程内存时, 必须校验叶子表项可写 ([`super::PageTranslation::writable`]);
//! 只读页 (如共享 COW 页) 返回 `Err(())`, 避免内核绕过用户态写保护语义去改内容.
//!
//! SIMPLIFIED: 不打断 COW 只读页 (不触发写时复制), 写只读页一律返回 `Err(())`;
//! 影响面为「对目标进程只读页的远程写会 EFAULT 而非成功并 COW」; 若后续需对齐
//! Linux 的 `FOLL_WRITE` 语义, 应在此接入页面 COW 分裂后再写入.

use super::{PAGE_SIZE, VirtAddr, get_vmm, is_user_buf};

/// 从目标进程的用户内存读取 `len` 字节到内核缓冲区.
///
/// # 参数
///
/// - `target_cr3`: 目标进程页表根物理地址 (来自 `proc::process_get_cr3`); `0` 视为非法.
/// - `user_src`: 目标进程内的源虚拟地址.
/// - `kernel_dst`: 内核侧目的缓冲区, 长度必须 `>= len`.
/// - `len`: 期望读取的字节数.
///
/// # 返回
///
/// - `Ok(len)`: 成功读取的字节数.
/// - `Err(())`: 目标 CR3 非法 / 地址越界 / 目标页未映射 / 内核缓冲区过小.
///
/// # Errors
/// 目标 CR3 非法、地址越界、目标页未映射或内核缓冲区过小时返回 `Err(())`.
pub fn copy_from_user_in_mm(
    target_cr3: u64,
    user_src: u64,
    kernel_dst: &mut [u8],
    len: usize,
) -> Result<usize, ()> {
    if len == 0 {
        return Ok(0);
    }
    if kernel_dst.len() < len {
        return Err(());
    }
    // SAFETY: `kernel_dst` 长度已校验 >= len, 仅取裸指针供内部按方向分派;
    // 读取方向下内核侧为**目的**缓冲区, 写入不会越界.
    copy_in_mm(target_cr3, user_src, kernel_dst.as_mut_ptr(), len, false)
}

/// 把内核缓冲区内容写入目标进程的用户内存.
///
/// # 参数
///
/// - `target_cr3`: 目标进程页表根物理地址 (来自 `proc::process_get_cr3`); `0` 视为非法.
/// - `user_dst`: 目标进程内的目的虚拟地址.
/// - `kernel_src`: 内核侧源缓冲区, 长度必须 `>= len`.
/// - `len`: 期望写入的字节数.
///
/// # 返回
///
/// - `Ok(len)`: 成功写入的字节数.
/// - `Err(())`: 目标 CR3 非法 / 地址越界 / 目标页未映射或只读 / 内核缓冲区过小.
///
/// # Errors
/// 目标 CR3 非法、地址越界、目标页未映射/只读或内核缓冲区过小时返回 `Err(())`.
pub fn copy_to_user_in_mm(
    target_cr3: u64,
    user_dst: u64,
    kernel_src: &[u8],
    len: usize,
) -> Result<usize, ()> {
    if len == 0 {
        return Ok(0);
    }
    if kernel_src.len() < len {
        return Err(());
    }
    // SAFETY: `kernel_src` 长度已校验 >= len, 仅取裸指针以统一方向分派;
    // 写入方向下内核侧为**源**且只读, 内部实现不会修改其内容.
    copy_in_mm(
        target_cr3,
        user_dst,
        kernel_src.as_ptr().cast_mut(),
        len,
        true,
    )
}

/// 跨进程内存拷贝核心: 逐页翻译 + HHDM 别名访问.
///
/// `write_to_target == true` 表示把 `kernel_buf[0..len]` 写入目标进程用户内存
/// (需校验目标页可写); `false` 表示从目标进程用户内存读入 `kernel_buf[0..len]`.
///
/// # Errors
///
/// - `target_cr3 == 0` 或用户地址 (含 `len`) 越出用户空间 → `Err(())`
/// - 目标页未映射 → `Err(())`
/// - 写入只读页 → `Err(())`
fn copy_in_mm(
    target_cr3: u64,
    user_addr: u64,
    kernel_buf: *mut u8,
    len: usize,
    write_to_target: bool,
) -> Result<usize, ()> {
    if len == 0 {
        return Ok(0);
    }
    if !is_user_buf(user_addr, len) {
        return Err(());
    }
    if target_cr3 == 0 {
        return Err(());
    }

    let vmm = get_vmm();
    let mut done: usize = 0;
    while done < len {
        let cur = user_addr + done as u64;
        let page_off = (cur & (PAGE_SIZE - 1)) as usize;
        let chunk = core::cmp::min(len - done, PAGE_SIZE as usize - page_off);

        let Some(t) = vmm.translate_in_pml4(target_cr3, VirtAddr(cur)) else {
            return Err(());
        };
        if write_to_target && !t.writable {
            return Err(());
        }

        // SAFETY: `t.phys` 由 `translate_in_pml4` 返回, 对应目标进程**已映射**的物理帧;
        // HHDM 别名 (`phys + KERNEL_BASE`) 是内核对全部物理内存的恒等映射, 恒存在,
        // 且其页表项 `U/S=0` 属内核映射, 不放宽 SMAP. `kernel_buf.add(done)` 满足
        // `done < len` 且调用方保证内核缓冲区长度 >= len, 故 `chunk` 字节读写均落在
        // 两个缓冲区内; 用户物理页与内核缓冲区不重叠, `copy_nonoverlapping` 成立.
        unsafe {
            let target = t.phys.to_virt().0 as *mut u8;
            if write_to_target {
                core::ptr::copy_nonoverlapping(kernel_buf.add(done), target, chunk);
            } else {
                core::ptr::copy_nonoverlapping(target, kernel_buf.add(done), chunk);
            }
        }

        done += chunk;
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_in_mm_zero_len() {
        let mut buf = [0u8; 8];
        assert_eq!(copy_from_user_in_mm(0, 0, &mut buf, 0), Ok(0));
        assert_eq!(copy_to_user_in_mm(0, 0, &buf, 0), Ok(0));
    }

    #[test]
    fn test_in_mm_invalid_user_ptr() {
        let mut buf = [0u8; 16];
        // 空指针与内核高半区地址均应被 is_user_buf 拒绝 (在触碰 CR3 之前).
        assert!(copy_from_user_in_mm(0x1000, 0, &mut buf, 16).is_err());
        assert!(copy_from_user_in_mm(0x1000, 0xFFFF_8000_0000_0000, &mut buf, 16).is_err());
    }

    #[test]
    fn test_in_mm_null_cr3() {
        let mut buf = [0u8; 16];
        // 合法用户地址但 CR3 == 0: 不得解引用页表, 直接报错.
        assert!(copy_from_user_in_mm(0, 0x1000, &mut buf, 16).is_err());
        assert!(copy_to_user_in_mm(0, 0x1000, &buf, 16).is_err());
    }

    #[test]
    fn test_in_mm_buffer_too_small() {
        let mut buf = [0u8; 8];
        assert!(copy_from_user_in_mm(0x1000, 0x1000, &mut buf, 16).is_err());
        assert!(copy_to_user_in_mm(0x1000, 0x1000, &buf, 16).is_err());
    }
}
