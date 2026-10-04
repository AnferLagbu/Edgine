#![deny(unsafe_code)]
//! Per-process 文件描述符表 — privileged 机制实现
//!
//! ## DECISION-J 归属反转记录 (2026-09-13)
//!
//! 原定义（P1-I-01/D8 于 2026-06-16 提取到 `functions::proc::fd_table`）按
//! 统一判据"机制持有的数据结构/常量归 privileged"迁回 — FdTable 是 Process
//! 结构体字段 (privileged 进程机制状态), 被 privileged/proc/process.rs 消费。
//! privileged/proc 新增 `pub mod fd_table`; process.rs 改引 privileged 本地
//! 路径。functions 侧改 glob re-export 保持 API 兼容。
//!
//! ## 设计
//!
//! FD 表仅存储指向 OpenFile 的 handle_id, 不存储 offset/flags.
//! dup() 通过共享 OpenFile 实现 offset 共享 (POSIX 合规).
//!
//! ## 与旧实现的差异
//!
//! 旧: entries`[`i`]` = global_fd (i32)
//! 新: entries`[`i`]` = handle_id (u32) → OpenFile (共享 offset)

use crate::privileged::sync::IrqSpinLock;

/// 每进程 FD 表上限
pub const MAX_FDS_PER_PROCESS: usize = 64;

/// Per-process FD 表
///
/// `entries[local_fd] = handle_id` (指向全局 `OpenFile` 表)
/// `u32::MAX` 表示 slot 空闲.
#[derive(Debug)]
pub struct FdTable {
    /// `handle_id` 映射 (指向 `OpenFile`)
    entries: IrqSpinLock<[u32; MAX_FDS_PER_PROCESS]>,
    /// CLOEXEC 标志 (per-FD, 不随 dup 共享)
    cloexec: IrqSpinLock<[bool; MAX_FDS_PER_PROCESS]>,
}

impl FdTable {
    /// 创建未初始化的 `FdTable`
    pub const fn new() -> Self {
        Self {
            entries: IrqSpinLock::new([u32::MAX; MAX_FDS_PER_PROCESS]),
            cloexec: IrqSpinLock::new([false; MAX_FDS_PER_PROCESS]),
        }
    }

    /// 初始化 FD 表 (清空所有 slot)
    pub fn init(&self) {
        let mut entries = self.entries.lock();
        let mut cloexec = self.cloexec.lock();
        for i in 0..MAX_FDS_PER_PROCESS {
            entries[i] = u32::MAX;
            cloexec[i] = false;
        }
    }

    /// 分配 per-process FD slot, 返回本地 fd 编号.
    ///
    /// 策略: first-fit.
    pub fn alloc_fd(&self, handle_id: u32, cloexec: bool) -> Option<usize> {
        let mut entries = self.entries.lock();
        let mut cloexec_lock = self.cloexec.lock();
        for i in 0..MAX_FDS_PER_PROCESS {
            if entries[i] == u32::MAX {
                entries[i] = handle_id;
                cloexec_lock[i] = cloexec;
                return Some(i);
            }
        }
        None
    }

    /// 通过本地 fd 获取 `handle_id`.
    pub fn get_handle_id(&self, local_fd: usize) -> Option<u32> {
        let entries = self.entries.lock();
        if local_fd < MAX_FDS_PER_PROCESS {
            let hid = entries[local_fd];
            if hid == u32::MAX { None } else { Some(hid) }
        } else {
            None
        }
    }

    /// 读取本地 fd 槽位条目, 返回 `(handle_id, cloexec)`.
    ///
    /// 越界或槽位空闲返回 `None`. 供 dup/dup2 取源条目与 fork 复制使用.
    pub fn get_entry(&self, local_fd: usize) -> Option<(u32, bool)> {
        if local_fd >= MAX_FDS_PER_PROCESS {
            return None;
        }
        let entries = self.entries.lock();
        let cloexec = self.cloexec.lock();
        let hid = entries[local_fd];
        if hid == u32::MAX {
            None
        } else {
            Some((hid, cloexec[local_fd]))
        }
    }

    /// 在指定本地 fd 槽位写入条目, 返回被覆盖的原 `handle_id`.
    ///
    /// 返回 `None`: 槽位原本空闲或 `local_fd` 越界. dup2 语义要求调用方
    /// 先校验范围与源 fd 有效性. offset/flags 由共享 `OpenFile` 承载,
    /// 此处仅改写 `handle_id` 与 new-fd 的 cloexec (POSIX: dup2 结果无 CLOEXEC).
    pub fn set_fd_at(&self, local_fd: usize, handle_id: u32, cloexec: bool) -> Option<u32> {
        if local_fd >= MAX_FDS_PER_PROCESS {
            return None;
        }
        let mut entries = self.entries.lock();
        let mut cloexec_lock = self.cloexec.lock();
        let prev = entries[local_fd];
        entries[local_fd] = handle_id;
        cloexec_lock[local_fd] = cloexec;
        if prev == u32::MAX { None } else { Some(prev) }
    }

    /// 从 `src` 复制全部条目 (fork 子进程复制父进程 fd 表).
    ///
    /// 先快照 `src` (锁作用域内拷贝到栈数组), 再写入自身, 避免同时持有两个
    /// `FdTable` 的锁. 调用方负责对复制出的每个 `handle_id` 增加 `OpenFile`
    /// 引用计数 (子进程新增引用). 前置条件: `src` 与 `self` 非同一实例.
    pub fn copy_from(&self, src: &FdTable) {
        let (src_entries, src_cloexec) = {
            let e = src.entries.lock();
            let c = src.cloexec.lock();
            (*e, *c)
        };
        {
            let mut e = self.entries.lock();
            *e = src_entries;
        }
        {
            let mut c = self.cloexec.lock();
            *c = src_cloexec;
        }
    }

    /// 关闭本地 fd, 返回被关闭的 `handle_id`.
    pub fn close_fd(&self, local_fd: usize) -> Option<u32> {
        if local_fd >= MAX_FDS_PER_PROCESS {
            return None;
        }
        let mut entries = self.entries.lock();
        let mut cloexec = self.cloexec.lock();
        let hid = entries[local_fd];
        if hid == u32::MAX {
            None
        } else {
            entries[local_fd] = u32::MAX;
            cloexec[local_fd] = false;
            Some(hid)
        }
    }

    /// 获取 CLOEXEC 标志
    pub fn is_cloexec(&self, local_fd: usize) -> bool {
        if local_fd >= MAX_FDS_PER_PROCESS {
            return false;
        }
        let cloexec = self.cloexec.lock();
        cloexec[local_fd]
    }

    /// 设置 CLOEXEC 标志
    pub fn set_cloexec(&self, local_fd: usize, cloexec: bool) {
        if local_fd >= MAX_FDS_PER_PROCESS {
            return;
        }
        let mut cloexec_lock = self.cloexec.lock();
        cloexec_lock[local_fd] = cloexec;
    }

    /// 获取所有已分配的 FD 列表
    pub fn get_all_fds(&self) -> alloc::vec::Vec<(usize, u32)> {
        let entries = self.entries.lock();
        entries
            .iter()
            .enumerate()
            .filter(|&(_, &hid)| hid != u32::MAX)
            .map(|(local, &handle)| (local, handle))
            .collect()
    }

    /// 获取所有 CLOEXEC 的 FD (用于 exec 时关闭)
    pub fn get_cloexec_fds(&self) -> alloc::vec::Vec<usize> {
        let entries = self.entries.lock();
        let cloexec = self.cloexec.lock();
        (0..MAX_FDS_PER_PROCESS)
            .filter(|&i| entries[i] != u32::MAX && cloexec[i])
            .collect()
    }
}

impl Default for FdTable {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// 单元测试 (DECISION-080 双轨: 纯逻辑测试归源侧 #[cfg(test)])
// ============================================================================

#[cfg(test)]
mod tests {
    use super::FdTable;

    /// per-process fd 表 (B-9.5): first-fit 分配 + close 后槽位复用 + dup 共享 handle
    #[test]
    fn test_fd_table_alloc_close() {
        let table = FdTable::new();
        let fd1 = table.alloc_fd(7, false);
        assert!(fd1.is_some(), "first alloc should succeed");
        let fd2 = table.alloc_fd(8, false);
        assert!(fd2.is_some(), "second alloc should succeed");
        assert_ne!(fd1.unwrap(), fd2.unwrap(), "fds should be different");
        assert_eq!(table.get_handle_id(fd1.unwrap()), Some(7));
        assert_eq!(table.get_handle_id(fd2.unwrap()), Some(8));

        // close 后槽位空闲, 下一次分配复用最小空闲槽位 (first-fit)
        assert_eq!(
            table.close_fd(fd1.unwrap()),
            Some(7),
            "close 应返回被关闭的 handle"
        );
        assert_eq!(
            table.get_handle_id(fd1.unwrap()),
            None,
            "已关闭 fd 应无映射"
        );
        let fd3 = table.alloc_fd(9, false);
        assert_eq!(fd3, fd1, "first-fit 应复用刚释放的槽位");

        // dup 语义: 两个本地 fd 共享同一 handle (offset 由 OpenFile 承载)
        let dup_fd = table.alloc_fd(9, false);
        assert!(dup_fd.is_some(), "dup slot alloc 应成功");
        assert_eq!(
            table.get_handle_id(dup_fd.unwrap()),
            table.get_handle_id(fd3.unwrap()),
            "dup 出的两个 fd 应共享同一 handle"
        );
    }
}
