#![deny(unsafe_code)]
#![deny(missing_docs)]
use crate::functions::fs::unkfs::bp::UnkfsBlockPointer;
use crate::functions::fs::unkfs::dataset::UnkfsDataset;
use crate::functions::sync::irq_lock::IrqSpinLock as Mutex;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

/// 单个数据集允许保留的快照数量上限
pub const HV_SNAP_MAX: usize = 64;

/// 快照管理器 (快照表 + 单调递增的 ID 分配器)
pub struct UnkfsSnapshotManager {
    /// 全部已登记快照
    pub snapshots: Mutex<Vec<UnkfsSnapshot>>,
    /// 下一个可用的快照 ID
    pub next_snap_id: AtomicU64,
}

// SAFETY (Framekernel P2.2.2): UnkfsSnapshotManager 全部字段 (Mutex<T>, AtomicU64) 自动 Send + Sync。

/// 快照记录 (ID / 归属数据集 / 根块指针 / 出生事务组)
#[derive(Debug, Clone)]
pub struct UnkfsSnapshot {
    /// 快照唯一标识
    pub snap_id: u64,
    /// 所属数据集标识
    pub ds_id: u64,
    /// 快照名称 (NUL 结尾, 最长 127 字节)
    pub name: [u8; 128],
    /// 快照时数据集根块指针
    pub root_bp: UnkfsBlockPointer,
    /// 创建快照时的事务组号
    pub birth_txg: u64,
    /// 快照时数据集已用空间
    pub used_space: u64,
    /// 引用计数 (克隆会增加)
    pub ref_count: u64,
    /// 该快照是否由克隆产生
    pub is_clone: bool,
    /// 克隆来源快照的 ID
    pub origin_snap: Option<u64>,
}

impl UnkfsSnapshot {
    /// 构造快照记录 (名称超长截断, 引用计数初始为 1)
    #[must_use]
    pub fn new(snap_id: u64, ds_id: u64, name: &str, root_bp: UnkfsBlockPointer, txg: u64) -> Self {
        let mut n = [0u8; 128];
        let b = name.as_bytes();
        let len = b.len().min(127);
        n[..len].copy_from_slice(&b[..len]);
        Self {
            snap_id,
            ds_id,
            name: n,
            root_bp,
            birth_txg: txg,
            used_space: 0,
            ref_count: 1,
            is_clone: false,
            origin_snap: None,
        }
    }

    /// 读取快照名称 (非 UTF-8 时返回空串)
    #[must_use]
    pub fn get_name(&self) -> &str {
        let end = self.name.iter().position(|&b| b == 0).unwrap_or(128);
        core::str::from_utf8(&self.name[..end]).unwrap_or("")
    }
}

impl UnkfsSnapshotManager {
    /// 构造空快照管理器 (ID 从 1 开始分配)
    #[must_use]
    pub fn new() -> Self {
        Self {
            snapshots: Mutex::new(Vec::new()),
            next_snap_id: AtomicU64::new(1),
        }
    }

    /// 为数据集创建快照并返回新快照 ID
    pub fn create_snapshot(&self, ds: &UnkfsDataset, name: &str, txg: u64) -> u64 {
        let snap_id = self.next_snap_id.fetch_add(1, Ordering::AcqRel);
        let root_bp = *ds.root_bp.lock();
        let mut snap = UnkfsSnapshot::new(snap_id, ds.ds_id, name, root_bp, txg);
        snap.used_space = ds.get_used();
        self.snapshots.lock().push(snap);
        snap_id
    }

    /// 销毁快照; 不存在或仍被克隆引用时返回 `false`
    pub fn destroy_snapshot(&self, snap_id: u64) -> bool {
        let mut snaps = self.snapshots.lock();
        let idx = snaps.iter().position(|s| s.snap_id == snap_id);
        match idx {
            Some(i) => {
                let snap = &snaps[i];
                if snap.ref_count > 1 {
                    return false;
                }
                snaps.remove(i);
                true
            }
            None => false,
        }
    }

    /// 以快照为源创建可写克隆数据集, 并增加源快照引用计数
    pub fn create_clone(
        &self,
        snap_id: u64,
        ds_id: u64,
        name: &str,
        txg: u64,
    ) -> Option<UnkfsDataset> {
        let mut snaps = self.snapshots.lock();
        let snap = snaps.iter_mut().find(|s| s.snap_id == snap_id)?;
        let root_bp = snap.root_bp;
        let mut ds = UnkfsDataset::new(ds_id, name, 0);
        *ds.root_bp.lock() = root_bp;
        ds.birth_txg.store(txg, Ordering::Release);
        ds.is_snapshot = false;
        ds.snapshot_origin = Some(snap_id);
        ds.writeable = true;
        snap.ref_count += 1;
        Some(ds)
    }

    /// 按 ID 读取快照副本
    pub fn get_snapshot(&self, snap_id: u64) -> Option<UnkfsSnapshot> {
        self.snapshots
            .lock()
            .iter()
            .find(|s| s.snap_id == snap_id)
            .cloned()
    }

    /// 列出指定数据集的全部快照副本
    pub fn list_snapshots(&self, ds_id: u64) -> Vec<UnkfsSnapshot> {
        self.snapshots
            .lock()
            .iter()
            .filter(|s| s.ds_id == ds_id)
            .cloned()
            .collect()
    }

    /// 将数据集回滚到指定快照; 快照不存在或归属不符时返回 `false`
    pub fn rollback(&self, snap_id: u64, ds: &UnkfsDataset) -> bool {
        let snaps = self.snapshots.lock();
        let Some(snap) = snaps.iter().find(|s| s.snap_id == snap_id) else {
            return false;
        };
        if snap.ds_id != ds.ds_id {
            return false;
        }
        *ds.root_bp.lock() = snap.root_bp;
        ds.used_space.store(snap.used_space, Ordering::Release);
        true
    }

    /// 当前登记的快照总数
    pub fn snapshot_count(&self) -> usize {
        self.snapshots.lock().len()
    }
}

impl Default for UnkfsSnapshotManager {
    /// 委托 [`UnkfsSnapshotManager::new`] 构造空快照管理器.
    fn default() -> Self {
        Self::new()
    }
}

// DECISION-080: 快照纯逻辑断言以本文件源侧 #[cfg(test)] 为唯一归属
// (host 可编译, 不再占用 kernel_test 注册表轨).
#[cfg(test)]
mod tests {
    use super::*;

    // ==== 快照基础语义 ====

    /// 构造快照后应能读回名称.
    #[test]
    fn test_snapshot_create() {
        let snap = UnkfsSnapshot::new(1, 10, "test-snap", UnkfsBlockPointer::null(), 5);
        assert_eq!(snap.get_name(), "test-snap", "snapshot name mismatch");
    }

    /// 新建快照管理器应无任何快照.
    #[test]
    fn test_snapshot_manager() {
        let mgr = UnkfsSnapshotManager::new();
        assert_eq!(
            mgr.snapshot_count(),
            0,
            "new manager should have 0 snapshots"
        );
    }
}
