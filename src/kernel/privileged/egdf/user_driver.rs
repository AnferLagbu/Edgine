use crate::klog_info;
use crate::klog_warn;
use crate::privileged::egdf::{DeviceState, EGDFProto};
use crate::privileged::egdf::{
    NodeId, PropertyValue, devtree_clear_user_mapped, devtree_clear_user_mapped_by_pid,
    devtree_get_node, devtree_get_user_mapped, devtree_set_user_mapped,
};
use crate::privileged::mm::get_vmm;
use crate::privileged::mm::{MmStruct, Vma, VmaType};
use crate::privileged::mm::{PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};
use crate::privileged::proc::{
    process_dec_ref, process_exists, process_get_cr3, process_get_pwm, process_try_inc_ref,
};
use crate::privileged::sgeg::engine;
use crate::privileged::sgeg::{
    CAP_DOMAIN_DEVICE, DEVICE_CAP_BIND, DEVICE_CAP_IRQ, DEVICE_CAP_MMIO,
};
use crate::privileged::sgeg::{CapBits, CapDomain};

const MAX_MMIO_SIZE: usize = 256 * 1024 * 1024;

pub struct UserDriverError {
    code: i32,
}

impl UserDriverError {
    pub const fn new(code: i32) -> Self {
        Self { code }
    }
    pub const fn code(&self) -> i32 {
        self.code
    }
}

pub const ERR_OK: i32 = 0;
pub const ERR_NOT_FOUND: i32 = -1;
pub const ERR_NOT_AUTHORIZED: i32 = -2;
pub const ERR_INVALID_STATE: i32 = -3;
pub const ERR_NO_MMIO: i32 = -4;
pub const ERR_PID_MISMATCH: i32 = -5;
pub const ERR_OOM: i32 = -6;

fn has_device_cap(pwm: u64, required: u64) -> bool {
    engine::check(pwm, CapDomain(CAP_DOMAIN_DEVICE), CapBits(required))
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 将设备树节点绑定到指定用户进程, 使该进程获得该设备的独占访问权。
/// # Errors
/// PWM 缺少 `DEVICE_CAP_BIND` 权限、进程不存在、节点不存在、节点状态非法或节点已被其他进程占用时返回 Err。
pub fn devtree_bind_user_device(
    node_id: NodeId,
    pid: u32,
    pwm: u64,
) -> Result<(), UserDriverError> {
    if !has_device_cap(pwm, DEVICE_CAP_BIND) {
        klog_warn!(
            Driver,
            "EGDF: PWM {} lacks DEVICE_CAP_BIND for node {}",
            pwm,
            node_id
        );
        return Err(UserDriverError::new(ERR_NOT_AUTHORIZED));
    }

    if !process_exists(pid) {
        return Err(UserDriverError::new(ERR_NOT_FOUND));
    }

    let node = match devtree_get_node(node_id) {
        Some(n) => n,
        None => return Err(UserDriverError::new(ERR_NOT_FOUND)),
    };

    if node.state != DeviceState::Ready {
        return Err(UserDriverError::new(ERR_INVALID_STATE));
    }

    if node.proto == EGDFProto::Bus {
        return Err(UserDriverError::new(ERR_INVALID_STATE));
    }

    if node.user_mapped.is_some() {
        return Err(UserDriverError::new(ERR_INVALID_STATE));
    }

    devtree_set_user_mapped(node_id, pid);

    klog_info!(
        Driver,
        "EGDF: device node {} bound to user pid={} pwm={}",
        node_id,
        pid,
        pwm
    );
    Ok(())
}

/// 解除设备树节点与用户进程的绑定, 并卸载进程虚拟地址空间中映射的设备 MMIO 范围。
/// # Errors
/// PWM 缺少 `DEVICE_CAP_BIND` 权限、进程不存在、节点不存在、节点映射的 PID 不匹配或节点未处于可解绑状态时返回 Err。
// 有意窄化: 用户内存代理, 指针/长度上下文保证
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub fn devtree_unbind_user_device(
    node_id: NodeId,
    pid: u32,
    pwm: u64,
    mm: &MmStruct,
) -> Result<(), UserDriverError> {
    if !has_device_cap(pwm, DEVICE_CAP_BIND) {
        return Err(UserDriverError::new(ERR_NOT_AUTHORIZED));
    }

    let node = match devtree_get_node(node_id) {
        Some(n) => n,
        None => return Err(UserDriverError::new(ERR_NOT_FOUND)),
    };

    match node.user_mapped {
        Some(mapped_pid) if mapped_pid == pid => {}
        Some(_) => return Err(UserDriverError::new(ERR_PID_MISMATCH)),
        None => return Err(UserDriverError::new(ERR_INVALID_STATE)),
    }

    let device_ranges: alloc::vec::Vec<(usize, usize)> = {
        let vmas = mm.vmas.lock();
        vmas.iter()
            .filter(|v| v.vma_type == VmaType::Device)
            .map(|v| (v.start, v.end))
            .collect()
    };

    if !device_ranges.is_empty() {
        let cr3 = {
            if !process_try_inc_ref(pid) {
                devtree_clear_user_mapped(node_id);
                return Err(UserDriverError::new(ERR_NOT_FOUND));
            }
            if let Some(c) = process_get_cr3(pid) {
                c
            } else {
                process_dec_ref(pid);
                devtree_clear_user_mapped(node_id);
                return Err(UserDriverError::new(ERR_NOT_FOUND));
            }
        };

        let vmm = get_vmm();
        for (start, end) in &device_ranges {
            let mut addr = *start;
            while addr < *end {
                vmm.unmap_page_in_table(cr3, VirtAddr(addr as u64));
                addr += PAGE_SIZE as usize;
            }
        }

        process_dec_ref(pid);

        for (start, end) in &device_ranges {
            mm.remove_range(*start, *end, cr3);
        }
    }

    devtree_clear_user_mapped(node_id);
    Ok(())
}

/// 将设备节点的 MMIO 资源映射到用户进程地址空间, 返回映射基址。
/// # Errors
/// PWM 缺少 `DEVICE_CAP_MMIO` 权限、进程或节点不存在、节点映射的 PID 不匹配、节点状态非法、缺少 reg 属性、物理地址或大小非法或内存不足时返回 Err。
// 有意窄化: 用户内存代理, 指针/长度上下文保证
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub fn devtree_map_user_device(
    node_id: NodeId,
    pid: u32,
    pwm: u64,
    mm: &MmStruct,
) -> Result<usize, UserDriverError> {
    if !has_device_cap(pwm, DEVICE_CAP_MMIO) {
        return Err(UserDriverError::new(ERR_NOT_AUTHORIZED));
    }

    let node = match devtree_get_node(node_id) {
        Some(n) => n,
        None => return Err(UserDriverError::new(ERR_NOT_FOUND)),
    };

    match node.user_mapped {
        Some(mapped_pid) if mapped_pid == pid => {}
        Some(_) => return Err(UserDriverError::new(ERR_PID_MISMATCH)),
        None => return Err(UserDriverError::new(ERR_INVALID_STATE)),
    }

    let phys_addr: u64 = match node.get_prop("reg") {
        Some(PropertyValue::U64(addr)) => *addr,
        Some(PropertyValue::U32(addr)) => u64::from(*addr),
        _ => return Err(UserDriverError::new(ERR_NO_MMIO)),
    };

    let size: u64 = match node.get_prop("size") {
        Some(PropertyValue::U64(sz)) => *sz,
        Some(PropertyValue::U32(sz)) => u64::from(*sz),
        _ => PAGE_SIZE,
    };

    if size == 0 || phys_addr == 0 {
        return Err(UserDriverError::new(ERR_NO_MMIO));
    }

    let clamped_size = (size as usize).min(MAX_MMIO_SIZE);
    if phys_addr.checked_add(clamped_size as u64).is_none() {
        return Err(UserDriverError::new(ERR_NO_MMIO));
    }
    let pages = clamped_size.div_ceil(PAGE_SIZE as usize);

    let map_base = match mm.find_free_range(pages * PAGE_SIZE as usize) {
        Some(addr) => addr,
        None => return Err(UserDriverError::new(ERR_OOM)),
    };

    let page_flags =
        PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::USER | PageFlags::CACHE_DISABLE;

    let cr3 = {
        if !process_try_inc_ref(pid) {
            return Err(UserDriverError::new(ERR_NOT_FOUND));
        }
        match process_get_cr3(pid) {
            Some(c) if c != 0 => c,
            _ => {
                process_dec_ref(pid);
                return Err(UserDriverError::new(ERR_NOT_FOUND));
            }
        }
    };

    let vmm = get_vmm();
    for i in 0..pages {
        let page_phys = PhysAddr(phys_addr + (i as u64 * PAGE_SIZE));
        let page_virt = VirtAddr((map_base + i * PAGE_SIZE as usize) as u64);
        vmm.map_page_in_table(cr3, page_virt, page_phys, page_flags);
    }

    let vma = Vma::new(
        map_base,
        map_base + pages * PAGE_SIZE as usize,
        page_flags,
        VmaType::Device,
    );
    if mm.insert_vma(vma).is_err() {
        for i in 0..pages {
            let unmap_virt = VirtAddr((map_base + i * PAGE_SIZE as usize) as u64);
            vmm.unmap_page_in_table(cr3, unmap_virt);
        }
        process_dec_ref(pid);
        return Err(UserDriverError::new(ERR_OOM));
    }

    process_dec_ref(pid);

    klog_info!(
        Driver,
        "EGDF: mapped MMIO for node {} (phys=0x{:X} size={}) → user VA=0x{:X}",
        node_id,
        phys_addr,
        clamped_size,
        map_base
    );
    Ok(map_base)
}

/// 解除设备节点的 MMIO 映射, 将指定虚拟地址范围从用户进程地址空间移除。
/// # Errors
/// PWM 缺少 `DEVICE_CAP_MMIO` 权限、进程不存在、节点不存在或节点映射的 PID 不匹配时返回 Err。
// 有意窄化: 用户内存代理, 指针/长度上下文保证
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub fn devtree_unmap_user_device(
    node_id: NodeId,
    pid: u32,
    pwm: u64,
    mm: &MmStruct,
    virt_addr: usize,
    size: usize,
) -> Result<(), UserDriverError> {
    if !has_device_cap(pwm, DEVICE_CAP_MMIO) {
        return Err(UserDriverError::new(ERR_NOT_AUTHORIZED));
    }

    let node = match devtree_get_node(node_id) {
        Some(n) => n,
        None => return Err(UserDriverError::new(ERR_NOT_FOUND)),
    };

    match node.user_mapped {
        Some(mapped_pid) if mapped_pid == pid => {}
        Some(_) => return Err(UserDriverError::new(ERR_PID_MISMATCH)),
        None => return Err(UserDriverError::new(ERR_INVALID_STATE)),
    }

    if size == 0 {
        return Ok(());
    }

    let pages = size.div_ceil(PAGE_SIZE as usize);
    if virt_addr.checked_add(size).is_none() {
        return Err(UserDriverError::new(ERR_NO_MMIO));
    }

    let cr3 = {
        if !process_try_inc_ref(pid) {
            return Err(UserDriverError::new(ERR_NOT_FOUND));
        }
        match process_get_cr3(pid) {
            Some(c) if c != 0 => c,
            _ => {
                process_dec_ref(pid);
                return Err(UserDriverError::new(ERR_NOT_FOUND));
            }
        }
    };

    let vmm = get_vmm();
    for i in 0..pages {
        let page_virt = VirtAddr((virt_addr + i * PAGE_SIZE as usize) as u64);
        vmm.unmap_page_in_table(cr3, page_virt);
    }

    mm.remove_range(virt_addr, virt_addr + pages * PAGE_SIZE as usize, cr3);

    devtree_clear_user_mapped(node_id);

    process_dec_ref(pid);

    Ok(())
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub fn egdf_forward_irq(node_id: NodeId) -> bool {
    let pid = match devtree_get_user_mapped(node_id) {
        Some(p) => p,
        None => return false,
    };

    let pwm = match process_get_pwm(pid) {
        Some(p) => p,
        None => return false,
    };

    if !has_device_cap(pwm, DEVICE_CAP_IRQ) {
        return false;
    }

    process_signal_pending_set(pid, 10);

    true
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn user_driver_bind(node_id: u32, pid: u32, pwm: u64) -> i32 {
    match devtree_bind_user_device(node_id, pid, pwm) {
        Ok(()) => 0,
        Err(e) => e.code(),
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn user_driver_unbind(node_id: u32, pid: u32, pwm: u64, mm: *const MmStruct) -> i32 {
    if mm.is_null() {
        return ERR_NOT_FOUND;
    }
    // SAFETY: `mm` 由调用方保证为有效指针; 只读访问
    let mm_ref = unsafe { &*mm };
    match devtree_unbind_user_device(node_id, pid, pwm, mm_ref) {
        Ok(()) => 0,
        Err(e) => e.code(),
    }
}

/// 向目标进程设置待处理信号 (SIGUSR1=10 for IRQ delivery).
/// 通过进程引用计数保护, 确保目标进程在信号设置期间不会被销毁。
/// 信号将在返回用户空间时由信号分发框架处理。
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn process_signal_pending_set(pid: u32, sig: u32) {
    if !process_try_inc_ref(pid) {
        return;
    }
    crate::privileged::proc::process_signal_pending_set(pid, sig);
    process_dec_ref(pid);
}

/// 进程退出时清理所有 EGDF 设备绑定
/// 遍历设备树, 清除该进程在所有节点上的 `user_mapped` 标记,
/// 防止残留标记导致设备节点永远无法被其他进程重新绑定
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
// 注意: 本函数保持 Rust ABI, 因作为 ProcessCleanupFn (Rust fn 指针)
//       注册到 privileged::process_cleanup, 不能改为 extern "C".
#[unsafe(no_mangle)]
#[expect(clippy::no_mangle_with_rust_abi)]
pub fn egdf_process_cleanup(pid: u32) {
    devtree_clear_user_mapped_by_pid(pid);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_driver_error_codes() {
        assert_ne!(ERR_OK, ERR_NOT_FOUND);
        assert_ne!(ERR_NOT_AUTHORIZED, ERR_PID_MISMATCH);
        assert_ne!(ERR_INVALID_STATE, ERR_OOM);
    }

    #[test]
    fn test_user_driver_error_display() {
        let e = UserDriverError::new(ERR_NOT_FOUND);
        assert_eq!(e.code(), ERR_NOT_FOUND);
    }
}
