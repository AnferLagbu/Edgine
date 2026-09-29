#![deny(unsafe_code)]
use crate::services::fs::nestfs::bp::NestBlockPointer;
use crate::services::fs::nestfs::spa::HV_POOL_BLOCK_SIZE;
use crate::services::sync::irq_lock::IrqSpinLock as Mutex;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub const HV_ZIL_MAX_RECORDS: usize = 1024;
pub const HV_ZIL_BLOCK_SIZE: usize = HV_POOL_BLOCK_SIZE as usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NestZilRecordType {
    Create = 1,
    Remove = 2,
    Link = 3,
    Rename = 4,
    Write = 5,
    Truncate = 6,
    SetAttr = 7,
    Acl = 8,
    CreateAcl = 9,
    Mkdir = 10,
    Symlink = 11,
    DedupRef = 12,
    DedupUnref = 13,
}

#[derive(Debug, Clone)]
pub struct NestZilRecord {
    pub rec_type: NestZilRecordType,
    pub txg: u64,
    pub obj_id: u64,
    pub parent_obj: u64,
    pub offset: u64,
    pub size: u32,
    pub name: [u8; 128],
    pub data_hash: [u64; 4],
    pub seq: u64,
}

impl NestZilRecord {
    pub fn new_write(txg: u64, obj_id: u64, offset: u64, size: u32) -> Self {
        Self {
            rec_type: NestZilRecordType::Write,
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

    pub fn new_create(txg: u64, parent_obj: u64, name: &str) -> Self {
        let mut n = [0u8; 128];
        let b = name.as_bytes();
        let len = b.len().min(127);
        n[..len].copy_from_slice(&b[..len]);
        Self {
            rec_type: NestZilRecordType::Create,
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

    pub fn new_remove(txg: u64, parent_obj: u64, name: &str) -> Self {
        let mut n = [0u8; 128];
        let b = name.as_bytes();
        let len = b.len().min(127);
        n[..len].copy_from_slice(&b[..len]);
        Self {
            rec_type: NestZilRecordType::Remove,
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

    pub fn new_mkdir(txg: u64, parent_obj: u64, name: &str) -> Self {
        let mut n = [0u8; 128];
        let b = name.as_bytes();
        let len = b.len().min(127);
        n[..len].copy_from_slice(&b[..len]);
        Self {
            rec_type: NestZilRecordType::Mkdir,
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

    pub fn new_setattr(txg: u64, obj_id: u64) -> Self {
        Self {
            rec_type: NestZilRecordType::SetAttr,
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

    pub fn new_link(txg: u64, parent_obj: u64, name: &str, obj_id: u64) -> Self {
        let mut n = [0u8; 128];
        let b = name.as_bytes();
        let len = b.len().min(127);
        n[..len].copy_from_slice(&b[..len]);
        Self {
            rec_type: NestZilRecordType::Link,
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

    pub fn new_rename(txg: u64, parent_obj: u64, old_name: &str, new_name: &str) -> Self {
        let mut n = [0u8; 128];
        let b = old_name.as_bytes();
        let len = b.len().min(63);
        n[..len].copy_from_slice(&b[..len]);
        let b2 = new_name.as_bytes();
        let len2 = b2.len().min(63);
        n[64..64 + len2].copy_from_slice(&b2[..len2]);
        Self {
            rec_type: NestZilRecordType::Rename,
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

    pub fn new_symlink(txg: u64, parent_obj: u64, link_name: &str, target: &str) -> Self {
        let mut n = [0u8; 128];
        let b = link_name.as_bytes();
        let len = b.len().min(63);
        n[..len].copy_from_slice(&b[..len]);
        let b2 = target.as_bytes();
        let len2 = b2.len().min(63);
        n[64..64 + len2].copy_from_slice(&b2[..len2]);
        Self {
            rec_type: NestZilRecordType::Symlink,
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

    pub fn new_dedup_ref(txg: u64, hash: [u64; 4], obj_id: u64) -> Self {
        Self {
            rec_type: NestZilRecordType::DedupRef,
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

    pub fn new_dedup_unref(txg: u64, hash: [u64; 4]) -> Self {
        Self {
            rec_type: NestZilRecordType::DedupUnref,
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

pub struct NestZil {
    pub records: Mutex<Vec<NestZilRecord>>,
    pub committed_seq: AtomicU64,
    pub current_seq: AtomicU64,
    pub log_bp: Mutex<NestBlockPointer>,
    pub itxg: AtomicU64,
    pub syncing: AtomicBool,
    pub replaying: AtomicBool,
    pub enabled: AtomicBool,
}

// SAFETY (Framekernel P2.2.2): NestZil 全部字段 (Mutex<T>, Atomic*) 自动 Send + Sync。

impl NestZil {
    pub fn new() -> Self {
        Self {
            records: Mutex::new(Vec::new()),
            committed_seq: AtomicU64::new(0),
            current_seq: AtomicU64::new(0),
            log_bp: Mutex::new(NestBlockPointer::null()),
            itxg: AtomicU64::new(0),
            syncing: AtomicBool::new(false),
            replaying: AtomicBool::new(false),
            enabled: AtomicBool::new(true),
        }
    }

    pub fn init(&self) {
        self.committed_seq.store(0, Ordering::Release);
        self.current_seq.store(0, Ordering::Release);
        self.records.lock().clear();
        self.enabled.store(true, Ordering::Release);
    }

    pub fn add_record(&self, record: NestZilRecord) {
        if !self.enabled.load(Ordering::Acquire) {
            return;
        }
        let seq = self.current_seq.fetch_add(1, Ordering::AcqRel) + 1;
        let mut rec = record;
        rec.seq = seq;
        self.records.lock().push(rec);
    }

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

    pub fn replay(&self) -> Vec<NestZilRecord> {
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

    pub fn has_uncommitted(&self) -> bool {
        let committed = self.committed_seq.load(Ordering::Acquire);
        let current = self.current_seq.load(Ordering::Acquire);
        current > committed
    }

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
        let rec = NestZilRecord::new_create(1, 0, "test_file");
        assert_eq!(rec.txg, 1, "txg mismatch");
        assert_eq!(rec.obj_id, 0, "obj_id should be 0");
    }

    /// 写记录应带上正确的事务号与对象号.
    #[test]
    fn test_zil_record_write() {
        let rec = NestZilRecord::new_write(2, 10, 0, 1024);
        assert_eq!(rec.txg, 2, "txg mismatch");
        assert_eq!(rec.obj_id, 10, "obj_id mismatch");
    }

    /// add_record + sync 后提交序号应推进.
    #[test]
    fn test_zil_add_and_sync() {
        let zil = NestZil::new();
        zil.init();
        zil.add_record(NestZilRecord::new_write(1, 5, 0, 512));
        zil.sync(1);
        assert!(
            zil.committed_seq.load(Ordering::SeqCst) >= 1,
            "committed_seq should advance"
        );
    }
}
