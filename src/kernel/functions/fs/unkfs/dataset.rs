#![deny(unsafe_code)]
#![deny(missing_docs)]
use crate::functions::fs::unkfs::bp::UnkfsBlockPointer;
use crate::functions::fs::unkfs::dmu::{UnkfsObjSet, UnkfsObjType};
use crate::functions::fs::unkfs::zap::UnkfsZap;
use crate::functions::sync::irq_lock::IrqSpinLock as Mutex;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

/// 数据集名/快照名的最大字节数.
pub const HV_DS_MAX_NAME: usize = 128;
/// 单个存储池可注册的数据集上限.
pub const HV_DS_MAX_DATASETS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
/// 数据集生命周期状态.
pub enum UnkfsDsState {
    /// 未初始化.
    Uninit = 0,
    /// 正在创建.
    Creating = 1,
    /// 正常可用.
    Active = 2,
    /// 正在销毁.
    Destroying = 3,
    /// 已挂起 (不可访问).
    Suspended = 4,
    /// 只读挂载.
    ReadOnly = 5,
}

#[derive(Debug, Clone)]
/// 数据集属性: 记录大小、压缩/校验策略、配额与属主等.
pub struct UnkfsDsProps {
    /// 记录大小 (字节).
    pub record_size: u32,
    /// 压缩算法编码.
    pub compression: u8,
    /// 校验算法编码.
    pub checksum: u8,
    /// 是否更新访问时间.
    pub atime: bool,
    /// 同步写入策略编码.
    pub sync: u8,
    /// 敏感度等级.
    pub sensitivity: u8,
    /// 属主 PWM 身份字.
    pub owner_pwm: u64,
    /// 数据配额 (字节).
    pub quota: u64,
    /// 空间预留 (字节).
    pub reservation: u64,
    /// 引用计数配额.
    pub ref_quota: u64,
    /// 引用计数空间预留.
    pub ref_reservation: u64,
}

impl UnkfsDsProps {
    // 2026-09-11 实测: default 触发 should_implement_trait, 需豁免
    #[allow(clippy::should_implement_trait)]
    /// 返回 ZFS 风格默认属性: 128 KiB 记录、Fletcher4 校验、开启 atime.
    pub fn default() -> Self {
        Self {
            record_size: 131_072,
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

/// 一个数据集 (文件系统实例): 对象集、目录 ZAP 与快照链.
pub struct UnkfsDataset {
    /// 数据集编号.
    pub ds_id: u64,
    /// 数据集名 (NUL 填充的定长字节数组).
    pub name: [u8; HV_DS_MAX_NAME],
    /// 生命周期状态 (`UnkfsDsState` 编码).
    pub state: AtomicU8,
    /// 数据集属性.
    pub props: UnkfsDsProps,
    /// 本数据集的对象集.
    pub objset: UnkfsObjSet,
    /// 根目录项 ZAP (名字 → 对象编号).
    pub dir_zap: UnkfsZap,
    /// 扩展属性 ZAP.
    pub xattr_zap: UnkfsZap,
    /// 父数据集编号.
    pub parent_id: Option<u64>,
    /// 子数据集编号列表.
    pub child_ids: Vec<u64>,
    /// 已用空间 (字节).
    pub used_space: AtomicU64,
    /// 引用计数.
    pub ref_count: AtomicU64,
    /// 出生事务组号.
    pub birth_txg: AtomicU64,
    /// 根块指针 (受锁保护).
    pub root_bp: Mutex<UnkfsBlockPointer>,
    /// 是否为快照.
    pub is_snapshot: bool,
    /// 快照对应的源数据集编号.
    pub snapshot_origin: Option<u64>,
    /// 快照链上的前一个快照.
    pub prev_snap: Option<u64>,
    /// 快照链上的后一个快照.
    pub next_snap: Option<u64>,
    /// 快照名 (定长字节数组).
    pub snap_name: [u8; HV_DS_MAX_NAME],
    /// 是否允许写入.
    pub writeable: bool,
    /// 是否已挂载.
    pub mounted: AtomicBool,
}

// SAFETY: UnkfsDataset uses AtomicBool for mounted; other fields are plain
// SAFETY (Framekernel P2.2.2): UnkfsDataset 全部字段自动 Send + Sync。

impl UnkfsDataset {
    /// 以编号、名称与属主创建数据集, 初始状态 Creating、可写.
    pub fn new(ds_id: u64, name: &str, owner_pwm: u64) -> Self {
        let mut n = [0u8; HV_DS_MAX_NAME];
        let b = name.as_bytes();
        let len = b.len().min(HV_DS_MAX_NAME - 1);
        n[..len].copy_from_slice(&b[..len]);
        let mut props = UnkfsDsProps::default();
        props.owner_pwm = owner_pwm;
        Self {
            ds_id,
            name: n,
            state: AtomicU8::new(UnkfsDsState::Creating as u8),
            props,
            objset: UnkfsObjSet::new(),
            dir_zap: UnkfsZap::new(),
            xattr_zap: UnkfsZap::new(),
            parent_id: None,
            child_ids: Vec::new(),
            used_space: AtomicU64::new(0),
            ref_count: AtomicU64::new(1),
            birth_txg: AtomicU64::new(0),
            root_bp: Mutex::new(UnkfsBlockPointer::null()),
            is_snapshot: false,
            snapshot_origin: None,
            prev_snap: None,
            next_snap: None,
            snap_name: [0; HV_DS_MAX_NAME],
            writeable: true,
            mounted: AtomicBool::new(false),
        }
    }

    /// 以 UTF-8 返回数据集名 (去除 NUL 填充; 非法编码返回空串).
    pub fn get_name(&self) -> &str {
        let end = self
            .name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(HV_DS_MAX_NAME);
        core::str::from_utf8(&self.name[..end]).unwrap_or("")
    }

    /// 初始化对象集并把状态转为 Active、标记为已挂载.
    pub fn init(&self, owner_pwm: u64) {
        self.objset.init(owner_pwm);
        self.state
            .store(UnkfsDsState::Active as u8, Ordering::Release);
        self.mounted.store(true, Ordering::Release);
    }

    /// 是否处于 Active 状态.
    pub fn is_active(&self) -> bool {
        self.state.load(Ordering::Acquire) == UnkfsDsState::Active as u8
    }

    /// 是否可写 (写权限位为真且状态为 Active).
    pub fn is_writeable(&self) -> bool {
        self.writeable && self.state.load(Ordering::Acquire) == UnkfsDsState::Active as u8
    }

    /// 在根目录创建普通文件并返回对象编号; 不可写或分配失败返回 `None`.
    pub fn create_file(&self, name: &str, owner_pwm: u64) -> Option<u64> {
        if !self.is_writeable() {
            return None;
        }
        let obj_id = self.objset.alloc_obj(UnkfsObjType::File, owner_pwm)?;
        self.dir_zap.insert_u64(name, obj_id);
        Some(obj_id)
    }

    /// 在根目录创建子目录并返回对象编号; 不可写或分配失败返回 `None`.
    pub fn create_dir(&self, name: &str, owner_pwm: u64) -> Option<u64> {
        if !self.is_writeable() {
            return None;
        }
        let obj_id = self.objset.alloc_obj(UnkfsObjType::Dir, owner_pwm)?;
        self.dir_zap.insert_u64(name, obj_id);
        Some(obj_id)
    }

    /// 按名字查根目录项, 返回对象编号.
    pub fn lookup(&self, name: &str) -> Option<u64> {
        self.dir_zap.lookup_u64(name)
    }

    /// 删除根目录下的名字并释放对应对象; 不可写或名字不存在返回 false.
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

    /// 把已有对象编号以给定名字登记到根目录; 不可写返回 false.
    pub fn link(&self, name: &str, obj_id: u64) -> bool {
        if !self.is_writeable() {
            return false;
        }
        self.dir_zap.insert_u64(name, obj_id);
        true
    }

    /// 列出根目录全部 (名字, 对象编号) 项.
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

    /// 读取已用空间 (字节).
    pub fn get_used(&self) -> u64 {
        self.used_space.load(Ordering::Relaxed)
    }

    /// 读取引用计数.
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
        let ds = UnkfsDataset::new(1, "test-ds", 0);
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
        let ds = UnkfsDataset::new(2, "init-ds", 0);
        ds.init(0);
        let _used = ds.get_used();
    }
}
