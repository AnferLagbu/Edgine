//! 全局 OpenFile 表 — functions 策略实现
//!
//! ## 阶段 4b 归属收敛 (当前权威)
//!
//! 自阶段 4b 起 OpenFileTable 实装归 `functions::fs::open_file_table`,
//! privileged 侧不再持有打开文件表。
//!
//! ## B09-12/DECISION-H13 P1-B5 迁移记录 (2026-08-31) — 已被阶段 4b 取代
//!
//! 当时按"机制归 privileged"原则将 OpenFileTable 迁回 privileged; 阶段 4b
//! 依 Minimalism 准则再次下沉 functions, 上述归属已失效。
//!
//! 存储所有打开的文件描述 (OpenFile), 通过 handle_id 引用.
//! dup() 通过引用计数共享 OpenFile, 实现 POSIX 共享 offset 语义.

use super::vfs_types::OpenFile;
use crate::privileged::sync::IrqSpinLock;

/// 全局 `OpenFile` 表上限
const MAX_OPEN_FILES: usize = 256;

/// 全局 `OpenFile` 表
pub struct OpenFileTable {
    /// `OpenFile` 存储 (通过 `handle_id` 索引)
    files: IrqSpinLock<[Option<OpenFile>; MAX_OPEN_FILES]>,
}

impl OpenFileTable {
    /// 创建未初始化的 `OpenFileTable`
    pub const fn new() -> Self {
        Self {
            files: IrqSpinLock::new([const { None }; MAX_OPEN_FILES]),
        }
    }

    /// 分配一个新的 `OpenFile`, 返回 `handle_id` (即数组索引).
    ///
    /// ## 分配策略
    ///
    /// first-fit 复用空闲槽位. 原实现用单调递增 `next_id`, 累计
    /// `MAX_OPEN_FILES` 次 open 后永久返回 `None` (永不回绕), 属系统级
    /// 资源泄漏; 改为扫描首个空闲槽位, 槽位随 close 回收后可再分配.
    pub fn alloc(&self, file: OpenFile) -> Option<u32> {
        let mut files = self.files.lock();
        for (i, slot) in files.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(file);
                return Some(i as u32);
            }
        }
        None
    }

    /// 获取 `OpenFile` 的引用 (通过闭包安全访问)
    pub fn with_file<F, R>(&self, handle_id: u32, f: F) -> Option<R>
    where
        F: FnOnce(&OpenFile) -> R,
    {
        let files = self.files.lock();
        if (handle_id as usize) < MAX_OPEN_FILES {
            files[handle_id as usize].as_ref().map(f)
        } else {
            None
        }
    }

    /// 增加引用计数 (dup 时调用)
    pub fn inc_ref(&self, handle_id: u32) {
        let files = self.files.lock();
        if let Some(file) = files[handle_id as usize].as_ref() {
            file.inc_ref();
        }
    }

    /// 减少引用计数 (close 时调用)
    pub fn dec_ref(&self, handle_id: u32) {
        let mut files = self.files.lock();
        if let Some(file) = files[handle_id as usize].as_ref() {
            let remaining = file.dec_ref();
            if remaining == 0 {
                files[handle_id as usize] = None;
            }
        }
    }

    /// 关闭 handle (减少引用计数)
    pub fn close(&self, handle_id: u32) {
        self.dec_ref(handle_id);
    }
}

/// 全局 `OpenFile` 表实例
pub static OPEN_FILE_TABLE: OpenFileTable = OpenFileTable::new();
