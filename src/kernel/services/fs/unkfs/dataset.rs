#![deny(unsafe_code)]
use crate::services::fs::unkfs::bp::NestBlockPointer;
use crate::services::fs::unkfs::dmu::{NestObjSet, NestObjType};
use crate::services::fs::unkfs::zap::NestZap;
use crate::services::sync::irq_lock::IrqSpinLock as Mutex;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

pub const HV_DS_MAX_NAME: usize = 128;
pub const HV_DS_MAX_DATASETS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NestDsState {
    Uninit = 0,
    Creating = 1,
    Active = 2,
    Destroying = 3,
    Suspended = 4,
    ReadOnly = 5,
}

#[derive(Debug, Clone)]
pub struct NestDsProps {
    pub record_size: u32,
    pub compression: u8,
    pub checksum: u8,
    pub atime: bool,
    pub sync: u8,
    pub sensitivity: u8,
    pub owner_pwm: u64,
    pub quota: u64,
    pub reservation: u64,
    pub ref_quota: u64,
    pub ref_reservation: u64,
}

impl NestDsProps {
    // 2026-09-11 实测: default 触发 should_implement_trait, 需豁免
    #[allow(clippy::should_implement_trait)]
    #[expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
    )]
    pub fn default() -> Self {
        Self {
            record_size: 131072,
            compression: 0,
            checksum: 2,
            atime: true,
            sync: 0,
            sensitivity: 0,
            owner_pwm: 0,
            quota: 0,
            reservation: 0,
            ref_quota: 0,
            ref_reservation: 0,
        }
    }
}

pub struct NestDataset {
    pub ds_id: u64,
    pub name: [u8; HV_DS_MAX_NAME],
    pub state: AtomicU8,
    pub props: NestDsProps,
    pub objset: NestObjSet,
    pub dir_zap: NestZap,
    pub xattr_zap: NestZap,
    pub parent_id: Option<u64>,
    pub child_ids: Vec<u64>,
    pub used_space: AtomicU64,
    pub ref_count: AtomicU64,
    pub birth_txg: AtomicU64,
    pub root_bp: Mutex<NestBlockPointer>,
    pub is_snapshot: bool,
    pub snapshot_origin: Option<u64>,
    pub prev_snap: Option<u64>,
    pub next_snap: Option<u64>,
    pub snap_name: [u8; HV_DS_MAX_NAME],
    pub writeable: bool,
    pub mounted: AtomicBool,
}

// SAFETY: NestDataset uses AtomicBool for mounted; other fields are plain
// SAFETY (Framekernel P2.2.2): NestDataset 全部字段自动 Send + Sync。

impl NestDataset {
    pub fn new(ds_id: u64, name: &str, owner_pwm: u64) -> Self {
        let mut n = [0u8; HV_DS_MAX_NAME];
        let b = name.as_bytes();
        let len = b.len().min(HV_DS_MAX_NAME - 1);
        n[..len].copy_from_slice(&b[..len]);
        let mut props = NestDsProps::default();
        props.owner_pwm = owner_pwm;
        Self {
            ds_id,
            name: n,
            state: AtomicU8::new(NestDsState::Creating as u8),
            props,
            objset: NestObjSet::new(),
            dir_zap: NestZap::new(),
            xattr_zap: NestZap::new(),
            parent_id: None,
            child_ids: Vec::new(),
            used_space: AtomicU64::new(0),
            ref_count: AtomicU64::new(1),
            birth_txg: AtomicU64::new(0),
            root_bp: Mutex::new(NestBlockPointer::null()),
            is_snapshot: false,
            snapshot_origin: None,
            prev_snap: None,
            next_snap: None,
            snap_name: [0; HV_DS_MAX_NAME],
            writeable: true,
            mounted: AtomicBool::new(false),
        }
    }

    pub fn get_name(&self) -> &str {
        let end = self
            .name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(HV_DS_MAX_NAME);
        core::str::from_utf8(&self.name[..end]).unwrap_or("")
    }

    pub fn init(&self, owner_pwm: u64) {
        self.objset.init(owner_pwm);
        self.state
            .store(NestDsState::Active as u8, Ordering::Release);
        self.mounted.store(true, Ordering::Release);
    }

    pub fn is_active(&self) -> bool {
        self.state.load(Ordering::Acquire) == NestDsState::Active as u8
    }

    pub fn is_writeable(&self) -> bool {
        self.writeable && self.state.load(Ordering::Acquire) == NestDsState::Active as u8
    }

    pub fn create_file(&self, name: &str, owner_pwm: u64) -> Option<u64> {
        if !self.is_writeable() {
            return None;
        }
        let obj_id = self.objset.alloc_obj(NestObjType::File, owner_pwm)?;
        self.dir_zap.insert_u64(name, obj_id);
        Some(obj_id)
    }

    pub fn create_dir(&self, name: &str, owner_pwm: u64) -> Option<u64> {
        if !self.is_writeable() {
            return None;
        }
        let obj_id = self.objset.alloc_obj(NestObjType::Dir, owner_pwm)?;
        self.dir_zap.insert_u64(name, obj_id);
        Some(obj_id)
    }

    pub fn lookup(&self, name: &str) -> Option<u64> {
        self.dir_zap.lookup_u64(name)
    }

    pub fn unlink(&self, name: &str) -> bool {
        if !self.is_writeable() {
            return false;
        }
        self.dir_zap.lookup_u64(name).map_or(false, |obj_id| {
            self.objset.free_obj(obj_id);
            self.dir_zap.remove(name);
            true
        })
    }

    pub fn link(&self, name: &str, obj_id: u64) -> bool {
        if !self.is_writeable() {
            return false;
        }
        self.dir_zap.insert_u64(name, obj_id);
        true
    }

    pub fn list_entries(&self) -> Vec<(String, u64)> {
        let keys = self.dir_zap.keys();
        let mut result = Vec::new();
        for key in keys {
            if let Some(obj_id) = self.dir_zap.lookup_u64(&key) {
                result.push((key, obj_id));
            }
        }
        result
    }

    pub fn get_used(&self) -> u64 {
        self.used_space.load(Ordering::Relaxed)
    }

    pub fn get_ref_count(&self) -> u64 {
        self.ref_count.load(Ordering::Relaxed)
    }
}

// DECISION-080: 数据集纯逻辑断言以本文件源侧 #[cfg(test)] 为唯一归属
// (host 可编译, 不再占用 kernel_test 注册表轨).
#[cfg(test)]
mod tests {
    use super::*;

    // ==== 数据集基础语义 ====

    /// 新建数据集应为 Creating 状态, init 后转为 Active 且可写.
    #[test]
    fn test_dataset_create() {
        let ds = NestDataset::new(1, "test-ds", 0);
        assert_eq!(ds.get_name(), "test-ds", "dataset name mismatch");
        assert!(
            !ds.is_active(),
            "new dataset should be Creating (not active)"
        );
        ds.init(0);
        assert!(ds.is_active(), "dataset should be active after init");
        assert!(ds.is_writeable(), "dataset should be writeable after init");
    }

    /// init 后应能读取已用空间.
    #[test]
    fn test_dataset_init() {
        let ds = NestDataset::new(2, "init-ds", 0);
        ds.init(0);
        let _used = ds.get_used();
    }
}
