//! # 设备快照机制
//!
//! 用于 FREG Soft Reset (BSR) 时捕获和恢复设备状态。

use core::sync::atomic::{AtomicU32, Ordering};

use crate::framework::sync::IrqSpinLock;
pub const MAX_DEVICE_SNAPSHOTS: usize = 16;
pub const MAX_REGISTERS_PER_DEVICE: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum DeviceType {
    Unknown = 0,
    Keyboard = 1,
    Serial = 2,
    Timer = 3,
    Network = 4,
    Storage = 5,
    Display = 6,
}

impl DeviceType {
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::Keyboard,
            2 => Self::Serial,
            3 => Self::Timer,
            4 => Self::Network,
            5 => Self::Storage,
            6 => Self::Display,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RegisterState {
    pub offset: u32,
    pub value: u32,
}

impl Default for RegisterState {
    fn default() -> Self {
        Self {
            offset: 0,
            value: 0,
        }
    }
}

#[derive(Debug)]
pub struct DeviceSnapshot {
    pub device_id: u64,
    pub device_type: DeviceType,
    pub name: &'static str,
    pub mmio_base: u64,
    pub registers: [RegisterState; MAX_REGISTERS_PER_DEVICE],
    pub register_count: usize,
    pub priority: u32,
    pub flags: u32,
}

impl DeviceSnapshot {
    pub const fn new(
        device_id: u64,
        device_type: DeviceType,
        name: &'static str,
        mmio_base: u64,
        priority: u32,
    ) -> Self {
        const DEFAULT_REG: RegisterState = RegisterState {
            offset: 0,
            value: 0,
        };
        Self {
            device_id,
            device_type,
            name,
            mmio_base,
            registers: [DEFAULT_REG; MAX_REGISTERS_PER_DEVICE],
            register_count: 0,
            priority,
            flags: 0,
        }
    }

    pub fn add_register(&mut self, offset: u32, value: u32) {
        if self.register_count < MAX_REGISTERS_PER_DEVICE {
            self.registers[self.register_count] = RegisterState { offset, value };
            self.register_count += 1;
        }
    }

    pub fn clear(&mut self) {
        self.register_count = 0;
        self.flags = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.register_count == 0
    }

    pub fn capture(&mut self, read_fn: fn(u64, u32) -> u32) {
        for i in 0..self.register_count {
            let offset = self.registers[i].offset;
            self.registers[i].value = read_fn(self.mmio_base, offset);
        }
        self.flags |= SNAPSHOT_FLAG_CAPTURED;
    }

    pub fn restore(&self, write_fn: fn(u64, u32, u32)) -> bool {
        if self.mmio_base == 0 {
            return false;
        }
        for i in 0..self.register_count {
            let reg = &self.registers[i];
            write_fn(self.mmio_base, reg.offset, reg.value);
        }
        true
    }
}

pub const SNAPSHOT_FLAG_CAPTURED: u32 = 0x01;
pub const SNAPSHOT_FLAG_VALID: u32 = 0x02;
pub const SNAPSHOT_FLAG_RESTORED: u32 = 0x04;

#[derive(Debug)]
pub struct DeviceSnapshotRegistry {
    snapshots: [Option<DeviceSnapshot>; MAX_DEVICE_SNAPSHOTS],
    count: usize,
    init_captured: AtomicU32,
}

impl DeviceSnapshotRegistry {
    pub const fn new() -> Self {
        const NONE: Option<DeviceSnapshot> = None;
        Self {
            snapshots: [NONE; MAX_DEVICE_SNAPSHOTS],
            count: 0,
            init_captured: AtomicU32::new(0),
        }
    }

    pub fn register(&mut self, snapshot: DeviceSnapshot) -> bool {
        if self.count >= MAX_DEVICE_SNAPSHOTS {
            return false;
        }
        self.snapshots[self.count] = Some(snapshot);
        self.count += 1;
        true
    }

    pub fn unregister(&mut self, device_id: u64) -> bool {
        for i in 0..self.count {
            if let Some(ref snap) = self.snapshots[i] {
                if snap.device_id == device_id {
                    self.snapshots[i] = None;
                    if i < self.count - 1 {
                        self.snapshots[i] = self.snapshots[self.count - 1].take();
                    }
                    self.count -= 1;
                    return true;
                }
            }
        }
        false
    }

    pub fn get_mut(&mut self, device_id: u64) -> Option<&mut DeviceSnapshot> {
        for i in 0..self.count {
            if let Some(ref snap) = self.snapshots[i] {
                if snap.device_id == device_id {
                    return self.snapshots[i].as_mut();
                }
            }
        }
        None
    }

    pub fn get(&self, device_id: u64) -> Option<&DeviceSnapshot> {
        for i in 0..self.count {
            if let Some(ref snap) = self.snapshots[i] {
                if snap.device_id == device_id {
                    return Some(snap);
                }
            }
        }
        None
    }

    pub fn capture_all_init(&mut self, read_fn: fn(u64, u32) -> u32) {
        for i in 0..self.count {
            if let Some(ref mut snap) = self.snapshots[i] {
                snap.capture(read_fn);
            }
        }
        self.init_captured.store(1, Ordering::SeqCst);
    }

    pub fn restore_all(&self, write_fn: fn(u64, u32, u32)) -> (usize, usize) {
        let mut success = 0usize;
        let mut failed = 0usize;

        let mut sorted_indices: [usize; MAX_DEVICE_SNAPSHOTS] = [0; MAX_DEVICE_SNAPSHOTS];
        for i in 0..self.count {
            sorted_indices[i] = i;
        }

        for i in 1..self.count {
            let mut j = i;
            while j > 0 {
                let prev_prio = self.snapshots[sorted_indices[j - 1]]
                    .as_ref()
                    .map_or(0, |snap| snap.priority);
                let curr_prio = self.snapshots[sorted_indices[j]]
                    .as_ref()
                    .map_or(0, |snap| snap.priority);
                if prev_prio <= curr_prio {
                    break;
                }
                sorted_indices.swap(j - 1, j);
                j -= 1;
            }
        }

        for &idx in &sorted_indices[..self.count] {
            if let Some(ref snap) = self.snapshots[idx] {
                if snap.restore(write_fn) {
                    success += 1;
                } else {
                    failed += 1;
                }
            }
        }

        (success, failed)
    }

    pub fn count(&self) -> usize {
        self.count
    }

    pub fn is_init_captured(&self) -> bool {
        self.init_captured.load(Ordering::SeqCst) == 1
    }

    #[expect(
        clippy::iter_without_into_iter,
        reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
    )]
    pub fn iter(&self) -> DeviceSnapshotIter<'_> {
        DeviceSnapshotIter {
            registry: self,
            index: 0,
        }
    }
}

pub struct DeviceSnapshotIter<'a> {
    registry: &'a DeviceSnapshotRegistry,
    index: usize,
}

impl<'a> Iterator for DeviceSnapshotIter<'a> {
    type Item = &'a DeviceSnapshot;

    fn next(&mut self) -> Option<Self::Item> {
        while self.index < self.registry.count {
            if let Some(ref snap) = self.registry.snapshots[self.index] {
                self.index += 1;
                return Some(snap);
            }
            self.index += 1;
        }
        None
    }
}

pub static DEVICE_SNAPSHOTS: IrqSpinLock<DeviceSnapshotRegistry> =
    IrqSpinLock::new(DeviceSnapshotRegistry::new());

pub fn snapshot_register_device(
    device_id: u64,
    device_type: DeviceType,
    name: &'static str,
    mmio_base: u64,
    priority: u32,
) -> bool {
    let mut registry = DEVICE_SNAPSHOTS.lock();
    registry.register(DeviceSnapshot::new(
        device_id,
        device_type,
        name,
        mmio_base,
        priority,
    ))
}

pub fn snapshot_unregister_device(device_id: u64) -> bool {
    let mut registry = DEVICE_SNAPSHOTS.lock();
    registry.unregister(device_id)
}

pub fn snapshot_capture_init(read_fn: fn(u64, u32) -> u32) {
    let mut registry = DEVICE_SNAPSHOTS.lock();
    registry.capture_all_init(read_fn);
}

pub fn snapshot_restore_all(write_fn: fn(u64, u32, u32)) -> (usize, usize) {
    let registry = DEVICE_SNAPSHOTS.lock();
    registry.restore_all(write_fn)
}

pub fn snapshot_is_init_captured() -> bool {
    DEVICE_SNAPSHOTS.lock().is_init_captured()
}

#[cfg(feature = "kernel_test")]
pub mod tests {
    // J-01 (2026-09-08): wildcard_imports 清理 — 显式列出本模块使用的 super 符号
    use super::{DeviceSnapshot, DeviceSnapshotRegistry, DeviceType};

    pub fn test_snapshot_basic() -> bool {
        let mut snap = DeviceSnapshot::new(1, DeviceType::Timer, "test_timer", 0xF000, 10);
        snap.add_register(0x00, 0x1234);
        snap.add_register(0x04, 0x5678);
        snap.register_count == 2
    }

    pub fn test_registry_register() -> bool {
        let mut registry = DeviceSnapshotRegistry::new();
        let snap = DeviceSnapshot::new(1, DeviceType::Keyboard, "kbd", 0x60, 5);
        registry.register(snap);
        registry.count() == 1
    }

    pub fn test_registry_priority_order() -> bool {
        // J-01 (2026-09-08): 嵌套辅助 fn 移至函数顶部 (items_after_statements 清理)
        fn dummy_write(_base: u64, _offset: u32, _value: u32) {}
        let mut registry = DeviceSnapshotRegistry::new();
        registry.register(DeviceSnapshot::new(
            1,
            DeviceType::Timer,
            "timer",
            0xF000,
            10,
        ));
        registry.register(DeviceSnapshot::new(2, DeviceType::Keyboard, "kbd", 0x60, 5));
        registry.register(DeviceSnapshot::new(
            3,
            DeviceType::Serial,
            "serial",
            0x3F8,
            8,
        ));

        let (success, failed) = registry.restore_all(dummy_write);
        success == 3 && failed == 0
    }
}
