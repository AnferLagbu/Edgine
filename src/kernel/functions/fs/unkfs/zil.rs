#![deny(unsafe_code)]
#![deny(missing_docs)]
use crate::functions::fs::unkfs::bp::UnkfsBlockPointer;
use crate::functions::fs::unkfs::spa::HV_POOL_BLOCK_SIZE;
use crate::functions::sync::irq_lock::IrqSpinLock as Mutex;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// ZIL 日志环形记录上限 (条).
pub const HV_ZIL_MAX_RECORDS: usize = 1024;
/// ZIL 日志记录块大小 (字节), 与存储池块大小一致.
pub const HV_ZIL_BLOCK_SIZE: usize = crate::functions::intcast::usize_from_u64(HV_POOL_BLOCK_SIZE);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
/// ZIL (意图日志) 记录类型: 覆盖创建/删除/链接/重命名/写入/截断等操作.
pub enum UnkfsZilRecordType {
    /// 创建文件.
    Create = 1,
    /// 删除文件.
    Remove = 2,
    /// 建立硬链接.
    Link = 3,
    /// 重命名.
    Rename = 4,
    /// 写入数据.
    Write = 5,
    /// 截断文件.
    Truncate = 6,
    /// 修改属性.
    SetAttr = 7,
    /// 修改 ACL.
    Acl = 8,
    /// 创建对象时附带 ACL.
    CreateAcl = 9,
    /// 创建目录.
    Mkdir = 10,
    /// 创建符号链接.
    Symlink = 11,
    /// 增加去重引用.
    DedupRef = 12,
    /// 减少去重引用.
    DedupUnref = 13,
}

#[derive(Debug, Clone)]
/// 一条 ZIL 记录: 操作类型、事务组号、对象标识与载荷.
pub struct UnkfsZilRecord {
    /// 记录类型.
    pub rec_type: UnkfsZilRecordType,
    /// 所属事务组号.
    pub txg: u64,
    /// 目标对象编号.
    pub obj_id: u64,
    /// 父目录对象编号.
    pub parent_obj: u64,
    /// 文件内偏移.
    pub offset: u64,
    /// 数据长度 (字节).
    pub size: u32,
    /// 名字/双名字载荷 (重命名与符号链接为前后两段各 64 字节).
    pub name: [u8; 128],
    /// 数据块哈希 (去重记录使用).
    pub data_hash: [u64; 4],
    /// 全局递增序号, 重放时据此排序.
    pub seq: u64,
}

impl UnkfsZilRecord {
    /// 构造一条写入记录.
    pub fn new_write(txg: u64, obj_id: u64, offset: u64, size: u32) -> Self {
        Self {
            rec_type: UnkfsZilRecordType::Write,
            txg,
            obj_id,
            parent_obj: 0,
            offset,
            size,
            name: [0; 128],
            data_hash: [0; 4],
            seq: 0,
        }
    }

    /// 构造一条创建记录.
    pub fn new_create(txg: u64, parent_obj: u64, name: &str) -> Self {
        let mut n = [0u8; 128];
        let b = name.as_bytes();
        let len = b.len().min(127);
        n[..len].copy_from_slice(&b[..len]);
        Self {
            rec_type: UnkfsZilRecordType::Create,
            txg,
            obj_id: 0,
            parent_obj,
            offset: 0,
            size: 0,
            name: n,
            data_hash: [0; 4],
            seq: 0,
        }
    }

    /// 构造一条删除记录.
    pub fn new_remove(txg: u64, parent_obj: u64, name: &str) -> Self {
        let mut n = [0u8; 128];
        let b = name.as_bytes();
        let len = b.len().min(127);
        n[..len].copy_from_slice(&b[..len]);
        Self {
            rec_type: UnkfsZilRecordType::Remove,
            txg,
            obj_id: 0,
            parent_obj,
            offset: 0,
            size: 0,
            name: n,
            data_hash: [0; 4],
            seq: 0,
        }
    }

    /// 构造一条创建目录记录.
    pub fn new_mkdir(txg: u64, parent_obj: u64, name: &str) -> Self {
        let mut n = [0u8; 128];
        let b = name.as_bytes();
        let len = b.len().min(127);
        n[..len].copy_from_slice(&b[..len]);
        Self {
            rec_type: UnkfsZilRecordType::Mkdir,
            txg,
            obj_id: 0,
            parent_obj,
            offset: 0,
            size: 0,
            name: n,
            data_hash: [0; 4],
            seq: 0,
        }
    }

    /// 构造一条属性修改记录.
    pub fn new_setattr(txg: u64, obj_id: u64) -> Self {
        Self {
            rec_type: UnkfsZilRecordType::SetAttr,
            txg,
            obj_id,
            parent_obj: 0,
            offset: 0,
            size: 0,
            name: [0; 128],
            data_hash: [0; 4],
            seq: 0,
        }
    }

    /// 构造一条硬链接记录.
    pub fn new_link(txg: u64, parent_obj: u64, name: &str, obj_id: u64) -> Self {
        let mut n = [0u8; 128];
        let b = name.as_bytes();
        let len = b.len().min(127);
        n[..len].copy_from_slice(&b[..len]);
        Self {
            rec_type: UnkfsZilRecordType::Link,
            txg,
            obj_id,
            parent_obj,
            offset: 0,
            size: 0,
            name: n,
            data_hash: [0; 4],
            seq: 0,
        }
    }

    /// 构造一条重命名记录 (旧名与新名各占前/后 64 字节).
    pub fn new_rename(txg: u64, parent_obj: u64, old_name: &str, new_name: &str) -> Self {
        let mut n = [0u8; 128];
        let b = old_name.as_bytes();
        let len = b.len().min(63);
        n[..len].copy_from_slice(&b[..len]);
        let b2 = new_name.as_bytes();
        let len2 = b2.len().min(63);
        n[64..64 + len2].copy_from_slice(&b2[..len2]);
        Self {
            rec_type: UnkfsZilRecordType::Rename,
            txg,
            obj_id: 0,
            parent_obj,
            offset: 0,
            size: 0,
            name: n,
            data_hash: [0; 4],
            seq: 0,
        }
    }

    /// 构造一条符号链接记录 (链接名与目标各占前/后 64 字节).
    pub fn new_symlink(txg: u64, parent_obj: u64, link_name: &str, target: &str) -> Self {
        let mut n = [0u8; 128];
        let b = link_name.as_bytes();
        let len = b.len().min(63);
        n[..len].copy_from_slice(&b[..len]);
        let b2 = target.as_bytes();
        let len2 = b2.len().min(63);
        n[64..64 + len2].copy_from_slice(&b2[..len2]);
        Self {
            rec_type: UnkfsZilRecordType::Symlink,
            txg,
            obj_id: 0,
            parent_obj,
            offset: 0,
            size: 0,
            name: n,
            data_hash: [0; 4],
            seq: 0,
        }
    }

    /// 构造一条去重引用增加记录.
    pub fn new_dedup_ref(txg: u64, hash: [u64; 4], obj_id: u64) -> Self {
        Self {
            rec_type: UnkfsZilRecordType::DedupRef,
            txg,
            obj_id,
            parent_obj: 0,
            offset: 0,
            size: 0,
            name: [0; 128],
            data_hash: hash,
            seq: 0,
        }
    }

    /// 构造一条去重引用减少记录.
    pub fn new_dedup_unref(txg: u64, hash: [u64; 4]) -> Self {
        Self {
            rec_type: UnkfsZilRecordType::DedupUnref,
            txg,
            obj_id: 0,
            parent_obj: 0,
            offset: 0,
            size: 0,
            name: [0; 128],
            data_hash: hash,
            seq: 0,
        }
    }
}

/// ZIL 意图日志: 暂存未提交记录并在事务组提交时批量落盘.
pub struct UnkfsZil {
    /// 未提交记录表.
    pub records: Mutex<Vec<UnkfsZilRecord>>,
    /// 已提交的最大序号.
    pub committed_seq: AtomicU64,
    /// 当前已分配的最大序号.
    pub current_seq: AtomicU64,
    /// 日志块指针.
    pub log_bp: Mutex<UnkfsBlockPointer>,
    /// 正在同步的事务组号.
    pub itxg: AtomicU64,
    /// 同步进行中标记 (防止并发同步).
    pub syncing: AtomicBool,
    /// 重放进行中标记 (防止并发重放).
    pub replaying: AtomicBool,
    /// 日志总开关.
    pub enabled: AtomicBool,
}

// SAFETY (Framekernel P2.2.2): UnkfsZil 全部字段 (Mutex<T>, Atomic*) 自动 Send + Sync。

impl UnkfsZil {
    /// 创建空日志实例 (默认启用).
    pub fn new() -> Self {
        Self {
            records: Mutex::new(Vec::new()),
            committed_seq: AtomicU64::new(0),
            current_seq: AtomicU64::new(0),
            log_bp: Mutex::new(UnkfsBlockPointer::null()),
            itxg: AtomicU64::new(0),
            syncing: AtomicBool::new(false),
            replaying: AtomicBool::new(false),
            enabled: AtomicBool::new(true),
        }
    }

    /// 重置日志: 清零序号、清空记录并重新启用.
    pub fn init(&self) {
        self.committed_seq.store(0, Ordering::Release);
        self.current_seq.store(0, Ordering::Release);
        self.records.lock().clear();
        self.enabled.store(true, Ordering::Release);
    }

    /// 追加一条记录并分配序号 (日志关闭时直接丢弃).
    pub fn add_record(&self, record: UnkfsZilRecord) {
        if !self.enabled.load(Ordering::Acquire) {
            return;
        }
        let seq = self.current_seq.fetch_add(1, Ordering::AcqRel) + 1;
        let mut rec = record;
        rec.seq = seq;
        self.records.lock().push(rec);
    }

    /// 提交不晚于给定事务组的记录, 并推进已提交序号.
    pub fn commit(&self, txg: u64) {
        let mut records = self.records.lock();
        let mut max_seq = 0u64;
        records.retain(|r| {
            if r.txg <= txg {
                max_seq = max_seq.max(r.seq);
                false
            } else {
                true
            }
        });
        self.committed_seq.store(max_seq, Ordering::Release);
    }

    /// 同步日志: 抢占同步标记后提交指定事务组 (已在同步中则直接返回).
    pub fn sync(&self, txg: u64) {
        if self
            .syncing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        self.commit(txg);
        self.syncing.store(false, Ordering::Release);
    }

    /// 取记录快照并按序号升序返回, 供崩溃恢复重放 (已在重放中则返回空表).
    pub fn replay(&self) -> Vec<UnkfsZilRecord> {
        if self
            .replaying
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            return Vec::new();
        }
        let records = self.records.lock().clone();
        let mut sorted = records;
        sorted.sort_by_key(|r| r.seq);
        self.replaying.store(false, Ordering::Release);
        sorted
    }

    /// 是否仍有未提交记录.
    pub fn has_uncommitted(&self) -> bool {
        let committed = self.committed_seq.load(Ordering::Acquire);
        let current = self.current_seq.load(Ordering::Acquire);
        current > committed
    }

    /// 当前待提交记录条数.
    pub fn pending_count(&self) -> usize {
        self.records.lock().len()
    }
}

// DECISION-080: ZIL 记录构造 / 提交纯逻辑断言以本文件源侧 #[cfg(test)] 为唯一归属.
#[cfg(test)]
mod tests {
    use super::*;

    /// 创建记录应带上正确的事务号与父对象.
    #[test]
    fn test_zil_record_create() {
        let rec = UnkfsZilRecord::new_create(1, 0, "test_file");
        assert_eq!(rec.txg, 1, "txg mismatch");
        assert_eq!(rec.obj_id, 0, "obj_id should be 0");
    }

    /// 写记录应带上正确的事务号与对象号.
    #[test]
    fn test_zil_record_write() {
        let rec = UnkfsZilRecord::new_write(2, 10, 0, 1024);
        assert_eq!(rec.txg, 2, "txg mismatch");
        assert_eq!(rec.obj_id, 10, "obj_id mismatch");
    }

    /// add_record + sync 后提交序号应推进.
    #[test]
    fn test_zil_add_and_sync() {
        let zil = UnkfsZil::new();
        zil.init();
        zil.add_record(UnkfsZilRecord::new_write(1, 5, 0, 512));
        zil.sync(1);
        assert!(
            zil.committed_seq.load(Ordering::SeqCst) >= 1,
            "committed_seq should advance"
        );
    }
}
