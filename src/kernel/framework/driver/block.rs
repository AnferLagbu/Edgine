//! 块设备抽象层
//!
//! 提供统一的 `BlockDevice` trait, 由所有存储驱动 (ATA, AHCI, NVMe, virtio-blk) 实现.
//!
//! ## Chitin 统一架构
//!
//! Chitin 是唯一的设备驱动框架. 块设备通过 `proto_block::register_block_device`
//! 注册到 Chitin, NestFS 通过 `chitin_blk_read/write` 直接 I/O.
//!
//! `BlockDevice` trait 定义在 chitin (设备框架) 中, 本模块 re-export.
//! `hdd_*` 函数提供向后兼容的 Chitin 代理.

use crate::framework::error::{KernelError, KernelResult};
use crate::framework::sync::IrqSpinLock as Mutex;
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering, fence};

// ── BlockDevice Trait (定义在 chitin, 此处 re-export) ──

pub use crate::framework::chitin::BlockDevice;

// ── SMP 安全基础设施 ──
//
// 保留 REGISTRY 用于 safe_unregister 的设备移除协议.
// I/O 主路径已迁移至 Chitin (chitin_blk_read/write).

static REGISTRY: Mutex<Vec<Option<Mutex<Box<dyn BlockDevice>>>>> = Mutex::new(Vec::new());
static DEVICE_NAMES: Mutex<Vec<Option<&'static str>>> = Mutex::new(Vec::new());
static IO_REFS: Mutex<Vec<AtomicU32>> = Mutex::new(Vec::new());
static REMOVING: Mutex<Vec<AtomicBool>> = Mutex::new(Vec::new());

pub fn register_named(name: &'static str, dev: Box<dyn BlockDevice>) -> usize {
    let mut list = REGISTRY.lock();
    let idx = list.len();
    list.push(Some(Mutex::new(dev)));
    drop(list);
    DEVICE_NAMES.lock().push(Some(name));
    IO_REFS.lock().push(AtomicU32::new(0));
    REMOVING.lock().push(AtomicBool::new(false));
    idx
}

pub fn register(dev: Box<dyn BlockDevice>) -> usize {
    register_named("unknown", dev)
}

pub fn with_device<R>(idx: usize, f: impl FnOnce(&mut dyn BlockDevice) -> R) -> Option<R> {
    let reg = REGISTRY.lock();
    if idx >= reg.len() {
        return None;
    }
    let slot = reg[idx].as_ref()?;
    let mut dev = slot.lock();
    Some(f(&mut **dev))
}

pub fn safe_unregister(idx: usize) -> Option<Box<dyn BlockDevice>> {
    {
        let reg = REGISTRY.lock();
        if idx >= reg.len() {
            return None;
        }
        if reg[idx].is_none() {
            return None;
        }
        let removing = REMOVING.lock();
        if idx < removing.len() {
            removing[idx].store(true, Ordering::Release);
        }
    }
    fence(Ordering::SeqCst);

    loop {
        let refs = IO_REFS.lock();
        let current = if idx < refs.len() {
            refs[idx].load(Ordering::Acquire)
        } else {
            0
        };
        drop(refs);
        if current == 0 {
            break;
        }
        core::hint::spin_loop();
    }

    let removed = {
        let mut reg = REGISTRY.lock();
        if idx >= reg.len() {
            return None;
        }
        match reg[idx].take() {
            // SAFETY: 设备注册表按槽位互斥访问, 取出时槽位已置 None, 无并发持有。
            Some(m) => unsafe { m.into_inner() },
            None => return None,
        }
    };

    {
        let mut names = DEVICE_NAMES.lock();
        if idx < names.len() {
            names[idx] = None;
        }
    }

    Some(removed)
}

pub fn is_removing(idx: usize) -> bool {
    let removing = REMOVING.lock();
    if idx >= removing.len() {
        return true;
    }
    removing[idx].load(Ordering::Acquire)
}

pub fn io_refcount(idx: usize) -> u32 {
    let refs = IO_REFS.lock();
    if idx >= refs.len() {
        return 0;
    }
    refs[idx].load(Ordering::Acquire)
}

pub fn unregister(idx: usize) -> Option<Box<dyn BlockDevice>> {
    safe_unregister(idx)
}

pub fn mark_removed(idx: usize) {
    let reg = REGISTRY.lock();
    if idx >= reg.len() {
        return;
    }
    let removing = REMOVING.lock();
    if idx < removing.len() {
        removing[idx].store(true, Ordering::Release);
    }
    fence(Ordering::SeqCst);
}

pub fn registry() -> &'static Mutex<Vec<Option<Mutex<Box<dyn BlockDevice>>>>> {
    &REGISTRY
}

pub fn count() -> usize {
    REGISTRY.lock().len()
}

// ── 多扇区辅助函数 ──

// I-20: read_sectors / write_sectors 改用 KernelResult<()>, 替代 C 风格 `return -1`.
// 现在错误类型与项目其余模块统一 (KernelError::InvalidArgument / IoError),
// 调用方能用 `?` 传播或 `.err()` 分类处理, 不再丢失错误上下文.
// 由于 Chitin 是 I/O 主路径, 这两个函数当前没有调用者; 保留以便上层
// `with_device` 风格的同步读写场景 (非中断上下文).

/// 从块设备连续读取多个扇区 (每扇区 512 字节)。
/// # Errors
/// 缓冲区长度不足以容纳读取数据或底层设备读取失败时返回 Err。
pub fn read_sectors(
    dev: &mut dyn BlockDevice,
    start: u64,
    count: u32,
    buf: &mut [u8],
) -> KernelResult<()> {
    let need = u64::from(count) * 512;
    if (buf.len() as u64) < need {
        return Err(KernelError::InvalidArgument);
    }
    let mut offset = 0usize;
    for i in 0..count {
        match dev.blk_read(start + u64::from(i), &mut buf[offset..offset + 512]) {
            n if n >= 0 => offset += 512,
            _ => return Err(KernelError::Io),
        }
    }
    Ok(())
}

/// 向块设备连续写入多个扇区 (每扇区 512 字节)。
/// # Errors
/// 缓冲区长度不足以提供待写数据或底层设备写入失败时返回 Err。
pub fn write_sectors(
    dev: &mut dyn BlockDevice,
    start: u64,
    count: u32,
    buf: &[u8],
) -> KernelResult<()> {
    let need = u64::from(count) * 512;
    if (buf.len() as u64) < need {
        return Err(KernelError::InvalidArgument);
    }
    let mut offset = 0usize;
    for i in 0..count {
        match dev.blk_write(start + u64::from(i), &buf[offset..offset + 512]) {
            n if n >= 0 => offset += 512,
            _ => return Err(KernelError::Io),
        }
    }
    Ok(())
}

// ── NestFS bridge (Chitin 代理) ──
//
// 所有 hdd_* 函数现在委托给 Chitin 统一 I/O 路径。
// 这确保 NestFS 的所有块设备访问都经过 Chitin。

pub fn hdd_read_sector(drive: u8, sector: u64, buf: &mut [u8]) -> i32 {
    crate::framework::chitin::chitin_blk_read(drive, sector, buf)
}

pub fn hdd_write_sector(drive: u8, sector: u64, buf: &[u8]) -> i32 {
    crate::framework::chitin::chitin_blk_write(drive, sector, buf)
}

pub fn hdd_is_present(drive: u8) -> bool {
    crate::framework::chitin::chitin_blk_is_present(drive)
}

pub fn hdd_total_sectors(drive: u8) -> u64 {
    crate::framework::chitin::chitin_blk_total_sectors(drive)
}

pub fn block_device_name(drive: u8) -> Option<&'static str> {
    crate::framework::chitin::chitin_blk_name(drive)
}

pub fn block_device_info(drive: u8) -> (&'static str, bool, u64) {
    crate::framework::chitin::chitin_blk_info(drive)
}

pub fn block_device_count() -> usize {
    crate::framework::chitin::chitin_blk_count()
}

pub fn block_device_list() -> Vec<(usize, &'static str, u64)> {
    let devices = crate::framework::chitin::CHITIN_DEVICES.lock();
    devices
        .iter()
        .filter(|d| d.proto == crate::framework::chitin::ChitinProto::Block)
        .enumerate()
        .map(|(i, d)| {
            let sectors = d.block_dev.as_ref().map_or(0, |bd| bd.blk_total_sectors());
            (i, d.name, sectors)
        })
        .collect()
}

pub fn block_device_state(drive: u8) -> (bool, bool, u32) {
    let idx = drive as usize;
    let present = hdd_is_present(drive);
    let removing = is_removing(idx);
    let io_count = io_refcount(idx);
    (present, removing, io_count)
}
