#![deny(unsafe_code)]
use crate::services::fs::unkfs::bp::NestBlockPointer;
use crate::services::sync::irq_lock::IrqSpinLock as Mutex;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NestTxgState {
    Open = 0,
    Quiescing = 1,
    Syncing = 2,
    Committed = 3,
}

pub const HV_TXG_SIZE: usize = 3;

#[derive(Debug, Clone)]
pub struct NestIo {
    pub bp: NestBlockPointer,
    pub offset: u64,
    pub size: u32,
    pub io_type: NestIoType,
    pub priority: u8,
    pub ready: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NestIoType {
    Read = 0,
    Write = 1,
    Free = 2,
    Claim = 3,
}

pub struct NestTxg {
    pub txg_id: u64,
    pub state: NestTxgState,
    pub birth_time: u64,
    pub nwrites: AtomicU64,
    pub nalloc: AtomicU64,
    pub nfree: AtomicU64,
    pub space_delta: AtomicU64,
    pub dirty_bps: Mutex<Vec<NestBlockPointer>>,
    pub free_bps: Mutex<Vec<NestBlockPointer>>,
    pub io_list: Mutex<Vec<NestIo>>,
    pub synced: AtomicBool,
}

// SAFETY (Framekernel P2.2.2): NestTxg 全部字段 (Mutex<T>, AtomicBool) 自动 Send + Sync。

impl NestTxg {
    pub fn new(txg_id: u64) -> Self {
        Self {
            txg_id,
            state: NestTxgState::Open,
            birth_time: 0,
            nwrites: AtomicU64::new(0),
            nalloc: AtomicU64::new(0),
            nfree: AtomicU64::new(0),
            space_delta: AtomicU64::new(0),
            dirty_bps: Mutex::new(Vec::new()),
            free_bps: Mutex::new(Vec::new()),
            io_list: Mutex::new(Vec::new()),
            synced: AtomicBool::new(false),
        }
    }

    pub fn open(&mut self) {
        self.state = NestTxgState::Open;
        self.synced.store(false, Ordering::Release);
    }

    pub fn quiesce(&mut self) {
        self.state = NestTxgState::Quiescing;
    }

    pub fn sync_start(&mut self) {
        self.state = NestTxgState::Syncing;
    }

    pub fn commit(&mut self) {
        self.state = NestTxgState::Committed;
        self.synced.store(true, Ordering::Release);
    }

    pub fn is_open(&self) -> bool {
        self.state == NestTxgState::Open
    }

    pub fn is_quiescing(&self) -> bool {
        self.state == NestTxgState::Quiescing
    }

    pub fn is_syncing(&self) -> bool {
        self.state == NestTxgState::Syncing
    }

    pub fn add_dirty(&self, bp: NestBlockPointer) {
        self.dirty_bps.lock().push(bp);
        self.nwrites.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add_free(&self, bp: NestBlockPointer) {
        self.free_bps.lock().push(bp);
        self.nfree.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add_io(&self, io: NestIo) {
        self.io_list.lock().push(io);
    }

    pub fn drain_dirty(&self) -> Vec<NestBlockPointer> {
        let mut dirty = self.dirty_bps.lock();
        let drained: Vec<NestBlockPointer> = dirty.drain(..).collect();
        drained
    }

    pub fn drain_free(&self) -> Vec<NestBlockPointer> {
        let mut free = self.free_bps.lock();
        let drained: Vec<NestBlockPointer> = free.drain(..).collect();
        drained
    }

    pub fn drain_io(&self) -> Vec<NestIo> {
        let mut io = self.io_list.lock();
        io.drain(..).collect()
    }
}

pub struct NestTxgGroup {
    pub txgs: [Option<NestTxg>; HV_TXG_SIZE],
    pub current: AtomicU64,
    pub open_txg: AtomicU32,
    pub quiescing_txg: AtomicU32,
    pub syncing_txg: AtomicU32,
    pub sync_in_progress: AtomicBool,
    pub total_syncs: AtomicU64,
    pub total_dirty: AtomicU64,
}

// SAFETY (Framekernel P2.2.2): NestTxgGroup 全部字段 (Mutex<T>, Atomic*) 自动 Send + Sync。

impl NestTxgGroup {
    pub fn new() -> Self {
        Self {
            txgs: [const { None }, const { None }, const { None }],
            current: AtomicU64::new(1),
            open_txg: AtomicU32::new(0),
            quiescing_txg: AtomicU32::new(0),
            syncing_txg: AtomicU32::new(0),
            sync_in_progress: AtomicBool::new(false),
            total_syncs: AtomicU64::new(0),
            total_dirty: AtomicU64::new(0),
        }
    }

    pub fn init(&mut self, start_txg: u64) {
        self.current.store(start_txg, Ordering::Release);
        for i in 0..HV_TXG_SIZE {
            self.txgs[i] = Some(NestTxg::new(start_txg + i as u64));
        }
        self.open_txg.store(0, Ordering::Release);
        self.quiescing_txg.store(1, Ordering::Release);
        self.syncing_txg.store(2, Ordering::Release);
        if let Some(ref mut txg) = self.txgs[0] {
            txg.open();
        }
        if let Some(ref mut txg) = self.txgs[1] {
            txg.quiesce();
        }
        if let Some(ref mut txg) = self.txgs[2] {
            txg.sync_start();
        }
    }

    pub fn get_open_txg(&self) -> Option<&NestTxg> {
        let idx = self.open_txg.load(Ordering::Acquire) as usize;
        if idx < HV_TXG_SIZE {
            self.txgs[idx].as_ref()
        } else {
            None
        }
    }

    pub fn get_open_txg_mut(&mut self) -> Option<&mut NestTxg> {
        let idx = self.open_txg.load(Ordering::Acquire) as usize;
        if idx < HV_TXG_SIZE {
            self.txgs[idx].as_mut()
        } else {
            None
        }
    }

    pub fn get_syncing_txg(&self) -> Option<&NestTxg> {
        let idx = self.syncing_txg.load(Ordering::Acquire) as usize;
        if idx < HV_TXG_SIZE {
            self.txgs[idx].as_ref()
        } else {
            None
        }
    }

    pub fn transition(&mut self) -> u64 {
        let old_open = self.open_txg.load(Ordering::Acquire) as usize;
        let old_quiescing = self.quiescing_txg.load(Ordering::Acquire) as usize;
        let old_syncing = self.syncing_txg.load(Ordering::Acquire) as usize;
        if let Some(ref mut txg) = self.txgs[old_open] {
            txg.quiesce();
        }
        if let Some(ref mut txg) = self.txgs[old_quiescing] {
            txg.sync_start();
        }
        if let Some(ref mut txg) = self.txgs[old_syncing] {
            txg.commit();
        }
        let new_txg_id = self.current.fetch_add(1, Ordering::AcqRel) + HV_TXG_SIZE as u64;
        let new_open = old_syncing;
        if let Some(ref mut txg) = self.txgs[new_open] {
            *txg = NestTxg::new(new_txg_id);
            txg.open();
        }
        self.open_txg.store(new_open as u32, Ordering::Release);
        self.quiescing_txg.store(old_open as u32, Ordering::Release);
        self.syncing_txg
            .store(old_quiescing as u32, Ordering::Release);
        self.total_syncs.fetch_add(1, Ordering::Relaxed);
        new_txg_id
    }

    pub fn current_txg(&self) -> u64 {
        self.current.load(Ordering::Acquire)
    }

    pub fn add_dirty_to_open(&self, bp: NestBlockPointer) {
        if let Some(txg) = self.get_open_txg() {
            txg.add_dirty(bp);
            self.total_dirty.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn add_free_to_open(&self, bp: NestBlockPointer) {
        if let Some(txg) = self.get_open_txg() {
            txg.add_free(bp);
        }
    }

    pub fn add_io_to_open(&self, io: NestIo) {
        if let Some(txg) = self.get_open_txg() {
            txg.add_io(io);
        }
    }
}

// DECISION-080: TXG 事务组 / 状态机纯逻辑断言以本文件源侧 #[cfg(test)] 为唯一归属.
#[cfg(test)]
mod tests {
    use super::*;

    // ==== 事务组生命周期 ====

    /// init 后 current txg 至少为起始值.
    #[test]
    fn test_txg_group_init() {
        let mut tg = NestTxgGroup::new();
        tg.init(1);
        assert!(tg.current_txg() >= 1, "txg current should be at least 1");
    }

    /// transition 后 txg 应向前推进.
    #[test]
    fn test_txg_group_transition() {
        let mut tg = NestTxgGroup::new();
        tg.init(1);
        let new_txg = tg.transition();
        assert!(new_txg >= 2, "txg should advance");
    }

    // ==== 单个事务状态机 ====

    /// 事务状态应随 open/quiesce/sync/commit 正确迁移.
    #[test]
    fn test_txg_states() {
        let mut txg = NestTxg::new(1);
        assert!(txg.is_open(), "new txg should default to Open");

        txg.quiesce();
        assert!(txg.is_quiescing(), "txg should be quiescing");

        txg.sync_start();
        assert!(txg.is_syncing(), "txg should be syncing");

        txg.commit();
        assert!(!txg.is_open(), "committed txg should not be open");
    }

    /// dirty 列表 drain 后应清空.
    #[test]
    fn test_txg_dirty_drain() {
        let mut txg = NestTxg::new(1);
        txg.open();
        let bp = NestBlockPointer::null();
        txg.add_dirty(bp);
        txg.add_dirty(bp);
        let dirty = txg.drain_dirty();
        assert_eq!(dirty.len(), 2, "should have 2 dirty entries");
        let dirty2 = txg.drain_dirty();
        assert!(dirty2.is_empty(), "drain should clear entries");
    }
}
