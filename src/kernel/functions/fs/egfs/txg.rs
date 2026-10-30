#![deny(unsafe_code)]
#![deny(missing_docs)]
use crate::functions::fs::egfs::bp::EgfsBlockPointer;
use crate::functions::sync::irq_lock::IrqSpinLock as Mutex;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
/// 事务组 (TXG) 状态机的四个阶段.
pub enum EgfsTxgState {
    /// 开放中, 接受新的脏块与 IO 登记.
    Open = 0,
    /// 已停止接受新登记, 等待在途操作完成.
    Quiescing = 1,
    /// 正在把脏块刷盘并提交.
    Syncing = 2,
    /// 已完成提交, 内容已持久化.
    Committed = 3,
}

/// 事务组环中并行保留的槽位数 (Open / Quiescing / Syncing 各一).
pub const HV_TXG_SIZE: usize = 3;

#[derive(Debug, Clone)]
/// 一条待执行的块 IO 请求.
pub struct EgfsIo {
    /// 目标块指针.
    pub bp: EgfsBlockPointer,
    /// 块内起始字节偏移.
    pub offset: u64,
    /// 本次 IO 的字节数.
    pub size: u32,
    /// IO 操作类型.
    pub io_type: EgfsIoType,
    /// 调度优先级, 数值越小越优先.
    pub priority: u8,
    /// 是否已就绪可提交执行.
    pub ready: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
/// 块 IO 的操作类型.
pub enum EgfsIoType {
    /// 读取块内容.
    Read = 0,
    /// 写入块内容.
    Write = 1,
    /// 释放块占用的空间.
    Free = 2,
    /// 认领已释放的块 (延迟释放队列).
    Claim = 3,
}

/// 单个事务组: 承载一个 TXG 周期内的脏块、空闲块与 IO 请求.
pub struct EgfsTxg {
    /// 事务组编号.
    pub txg_id: u64,
    /// 当前状态机阶段.
    pub state: EgfsTxgState,
    /// 事务组的出生时间戳.
    pub birth_time: u64,
    /// 累计写入次数.
    pub nwrites: AtomicU64,
    /// 累计分配次数.
    pub nalloc: AtomicU64,
    /// 累计释放次数.
    pub nfree: AtomicU64,
    /// 本事务组造成的空间增量 (字节).
    pub space_delta: AtomicU64,
    /// 待刷盘的脏块指针列表.
    pub dirty_bps: Mutex<Vec<EgfsBlockPointer>>,
    /// 待释放的块指针列表.
    pub free_bps: Mutex<Vec<EgfsBlockPointer>>,
    /// 待执行的块 IO 请求列表.
    pub io_list: Mutex<Vec<EgfsIo>>,
    /// 是否已完成同步提交.
    pub synced: AtomicBool,
}

// SAFETY (Framekernel P2.2.2): EgfsTxg 全部字段 (Mutex<T>, AtomicBool) 自动 Send + Sync。

impl EgfsTxg {
    /// 以指定编号构造处于 Open 状态的空事务组.
    #[must_use]
    pub fn new(txg_id: u64) -> Self {
        Self {
            txg_id,
            state: EgfsTxgState::Open,
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

    /// 重置为 Open 状态并清除已同步标志, 准备接受新登记.
    pub fn open(&mut self) {
        self.state = EgfsTxgState::Open;
        self.synced.store(false, Ordering::Release);
    }

    /// 进入 Quiescing 状态, 停止接受新的脏块登记.
    pub fn quiesce(&mut self) {
        self.state = EgfsTxgState::Quiescing;
    }

    /// 进入 Syncing 状态, 开始把脏块刷盘.
    pub fn sync_start(&mut self) {
        self.state = EgfsTxgState::Syncing;
    }

    /// 进入 Committed 状态并置位已同步标志.
    pub fn commit(&mut self) {
        self.state = EgfsTxgState::Committed;
        self.synced.store(true, Ordering::Release);
    }

    /// 当前是否处于 Open 状态.
    pub fn is_open(&self) -> bool {
        self.state == EgfsTxgState::Open
    }

    /// 当前是否处于 Quiescing 状态.
    pub fn is_quiescing(&self) -> bool {
        self.state == EgfsTxgState::Quiescing
    }

    /// 当前是否处于 Syncing 状态.
    pub fn is_syncing(&self) -> bool {
        self.state == EgfsTxgState::Syncing
    }

    /// 登记一个脏块并累加写入计数.
    pub fn add_dirty(&self, bp: EgfsBlockPointer) {
        self.dirty_bps.lock().push(bp);
        self.nwrites.fetch_add(1, Ordering::Relaxed);
    }

    /// 登记一个待释放块并累加释放计数.
    pub fn add_free(&self, bp: EgfsBlockPointer) {
        self.free_bps.lock().push(bp);
        self.nfree.fetch_add(1, Ordering::Relaxed);
    }

    /// 登记一条待执行的块 IO 请求.
    pub fn add_io(&self, io: EgfsIo) {
        self.io_list.lock().push(io);
    }

    /// 取走并清空全部脏块指针.
    pub fn drain_dirty(&self) -> Vec<EgfsBlockPointer> {
        let mut dirty = self.dirty_bps.lock();
        let drained: Vec<EgfsBlockPointer> = dirty.drain(..).collect();
        drained
    }

    /// 取走并清空全部待释放块指针.
    pub fn drain_free(&self) -> Vec<EgfsBlockPointer> {
        let mut free = self.free_bps.lock();
        let drained: Vec<EgfsBlockPointer> = free.drain(..).collect();
        drained
    }

    /// 取走并清空全部块 IO 请求.
    pub fn drain_io(&self) -> Vec<EgfsIo> {
        let mut io = self.io_list.lock();
        io.drain(..).collect()
    }
}

/// 三槽事务组环: 同时维护 Open / Quiescing / Syncing 三个连续事务组.
pub struct EgfsTxgGroup {
    /// 固定三槽的事务组数组.
    pub txgs: [Option<EgfsTxg>; HV_TXG_SIZE],
    /// 下一个待分配的事务组编号.
    pub current: AtomicU64,
    /// 当前 Open 槽位下标.
    pub open_txg: AtomicU32,
    /// 当前 Quiescing 槽位下标.
    pub quiescing_txg: AtomicU32,
    /// 当前 Syncing 槽位下标.
    pub syncing_txg: AtomicU32,
    /// 是否有同步提交正在进行.
    pub sync_in_progress: AtomicBool,
    /// 累计完成的同步提交次数.
    pub total_syncs: AtomicU64,
    /// 累计登记的脏块总数.
    pub total_dirty: AtomicU64,
}

// SAFETY (Framekernel P2.2.2): EgfsTxgGroup 全部字段 (Mutex<T>, Atomic*) 自动 Send + Sync。

impl EgfsTxgGroup {
    /// 构造空的事务组环, 三个槽位均未初始化.
    #[must_use]
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

    /// 以 `start_txg` 为起点初始化三个槽位, 分别置为 Open / Quiescing / Syncing.
    pub fn init(&mut self, start_txg: u64) {
        self.current.store(start_txg, Ordering::Release);
        for (i, slot) in self.txgs.iter_mut().enumerate() {
            *slot = Some(EgfsTxg::new(start_txg + i as u64));
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

    /// 取当前 Open 事务组的只读引用; 槽位未初始化时返回 `None`.
    pub fn get_open_txg(&self) -> Option<&EgfsTxg> {
        let idx = self.open_txg.load(Ordering::Acquire) as usize;
        if idx < HV_TXG_SIZE {
            self.txgs[idx].as_ref()
        } else {
            None
        }
    }

    /// 取当前 Open 事务组的可变引用; 槽位未初始化时返回 `None`.
    pub fn get_open_txg_mut(&mut self) -> Option<&mut EgfsTxg> {
        let idx = self.open_txg.load(Ordering::Acquire) as usize;
        if idx < HV_TXG_SIZE {
            self.txgs[idx].as_mut()
        } else {
            None
        }
    }

    /// 取当前 Syncing 事务组的只读引用; 槽位未初始化时返回 `None`.
    pub fn get_syncing_txg(&self) -> Option<&EgfsTxg> {
        let idx = self.syncing_txg.load(Ordering::Acquire) as usize;
        if idx < HV_TXG_SIZE {
            self.txgs[idx].as_ref()
        } else {
            None
        }
    }

    /// 推进一代事务组: 提交 Syncing 槽、复用其槽位承载新事务组, 并返回新编号.
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
            *txg = EgfsTxg::new(new_txg_id);
            txg.open();
        }
        self.open_txg.store(
            crate::functions::intcast::low_u32_from_usize(new_open),
            Ordering::Release,
        );
        self.quiescing_txg.store(
            crate::functions::intcast::low_u32_from_usize(old_open),
            Ordering::Release,
        );
        self.syncing_txg.store(
            crate::functions::intcast::low_u32_from_usize(old_quiescing),
            Ordering::Release,
        );
        self.total_syncs.fetch_add(1, Ordering::Relaxed);
        new_txg_id
    }

    /// 返回下一个待分配的事务组编号.
    pub fn current_txg(&self) -> u64 {
        self.current.load(Ordering::Acquire)
    }

    /// 向 Open 事务组登记脏块并累加全局脏块计数.
    pub fn add_dirty_to_open(&self, bp: EgfsBlockPointer) {
        if let Some(txg) = self.get_open_txg() {
            txg.add_dirty(bp);
            self.total_dirty.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 向 Open 事务组登记待释放块.
    pub fn add_free_to_open(&self, bp: EgfsBlockPointer) {
        if let Some(txg) = self.get_open_txg() {
            txg.add_free(bp);
        }
    }

    /// 向 Open 事务组登记块 IO 请求.
    pub fn add_io_to_open(&self, io: EgfsIo) {
        if let Some(txg) = self.get_open_txg() {
            txg.add_io(io);
        }
    }
}

impl Default for EgfsTxgGroup {
    /// 委托 [`EgfsTxgGroup::new`] 构造空的事务组环.
    fn default() -> Self {
        Self::new()
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
        let mut tg = EgfsTxgGroup::new();
        tg.init(1);
        assert!(tg.current_txg() >= 1, "txg current should be at least 1");
    }

    /// transition 后 txg 应向前推进.
    #[test]
    fn test_txg_group_transition() {
        let mut tg = EgfsTxgGroup::new();
        tg.init(1);
        let new_txg = tg.transition();
        assert!(new_txg >= 2, "txg should advance");
    }

    // ==== 单个事务状态机 ====

    /// 事务状态应随 open/quiesce/sync/commit 正确迁移.
    #[test]
    fn test_txg_states() {
        let mut txg = EgfsTxg::new(1);
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
        let mut txg = EgfsTxg::new(1);
        txg.open();
        let bp = EgfsBlockPointer::null();
        txg.add_dirty(bp);
        txg.add_dirty(bp);
        let dirty = txg.drain_dirty();
        assert_eq!(dirty.len(), 2, "should have 2 dirty entries");
        let dirty2 = txg.drain_dirty();
        assert!(dirty2.is_empty(), "drain should clear entries");
    }
}
