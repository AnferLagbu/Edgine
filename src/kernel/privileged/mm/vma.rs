//! Virtual Memory Area (VMA) — 用户地址空间管理
//!
//! 为每个进程的地址空间维护一组 VMA 描述符，提供：
//! - `mmap`/`munmap`/`mprotect`/`mremap` 语义
//! - 页错误 (page fault) 时的 demand paging 查找
//! - 地址空间布局 (stack/heap/mmap 方向)
//!
//! ## 数据结构
//!
//! 每个地址空间 (`MmStruct`) 维护一个按起始地址排序的 VMA 列表。
//! 当前使用 `Vec<Vma>` 实现，后续可升级为红黑树优化 O(log n) 查找。
//!
//! ## 安全
//!
//! `MmStruct` 内部保护由 `spin::Mutex` 提供。
//! `Vma` 中的物理页映射感知与 PMM 协作。

use super::numa::NumaRangePolicy;
use super::{PAGE_SIZE, PageFlags, VirtAddr};
use crate::privileged::sync::IrqSpinLock as Mutex;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

/// VMA 行为属性标志 (与 `PageFlags` 区分: `PageFlags` 是硬件页表属性, `VmFlags` 是内核策略)
///
/// ## 设计
///
/// - 32 位位掩码, atomic 友好
/// - 与 Linux `vm_flags` 同源, 但仅实现 `Edgine` 用到的子集
/// - mlock 路径 (MADV_*) 与 fork 行为 (`MADV_DONTFORK`) 由 `VmFlags` 驱动
/// - 与 `PageFlags` 解耦: mlock 不修改页表权限, 仅在内核策略路径被检查
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub struct VmFlags(pub u32);

impl VmFlags {
    pub const EMPTY: Self = Self(0);

    // ─── mlock 族 ────────────────────────────────────────────
    /// `mlock`/`mlockall(MCL_CURRENT)` 锁定的页: 不参与 swap/reclaim
    pub const MLOCKED: Self = Self(1 << 0);
    /// `mlockall(MCL_FUTURE)` 设置: 此后 mmap/匿名页自动锁定
    pub const LOCKED_FUTURE: Self = Self(1 << 1);
    /// `mlockall(MCL_ONFAULT)` 设置: 触达时才锁定 (Linux 4.4+)
    pub const LOCKED_ONFAULT: Self = Self(1 << 2);

    // ─── madvise 族 ──────────────────────────────────────────
    /// `MADV_DONTNEED`: 释放页 (下次 #PF 触发 zero-page 重新 alloc)
    pub const MADV_DONTNEED: Self = Self(1 << 4);
    /// `MADV_PAGEOUT`: 把不活跃页换出到 swap / 释放到 page cache
    pub const MADV_PAGEOUT: Self = Self(1 << 5);
    /// `MADV_FREE`: 仅清 PTE present 位 + 标记可释放, 不实际回收 (POSIX 2008)
    pub const MADV_FREE: Self = Self(1 << 6);
    /// `MADV_RANDOM`: 访问模式提示
    pub const MADV_RANDOM: Self = Self(1 << 7);
    /// `MADV_SEQUENTIAL`: 顺序读, 提前丢页
    pub const MADV_SEQUENTIAL: Self = Self(1 << 8);
    /// `MADV_WILLNEED`: 预读 (触发 readahead)
    pub const MADV_WILLNEED: Self = Self(1 << 9);

    #[inline]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub const fn bits(&self) -> u32 {
        self.0
    }
    #[inline]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }
    #[inline]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
    #[inline]
    #[expect(
        clippy::return_self_not_must_use,
        reason = "return_self_not_must_use: 返回 Self 是 builder/fluent API; 当前优先 expect"
    )]
    pub const fn insert(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
    #[inline]
    #[expect(
        clippy::return_self_not_must_use,
        reason = "return_self_not_must_use: 返回 Self 是 builder/fluent API; 当前优先 expect"
    )]
    pub const fn remove(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl core::ops::BitOr for VmFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.insert(rhs)
    }
}
impl core::ops::BitOrAssign for VmFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}
impl core::ops::BitAnd for VmFlags {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}
impl core::ops::BitAndAssign for VmFlags {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

/// 虚拟内存区域 (VMA) 类型.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum VmaType {
    Anonymous = 0,  // 匿名映射 (malloc / mmap(MAP_ANONYMOUS))
    FileBacked = 1, // mmap file
    Stack = 2,      // 用户栈 (向下增长)
    Heap = 3,       // 堆 (brk/sbrk)
    Vdso = 4,       // vDSO
    Vsvar = 5,      // vsyscall / vvar
    Guard = 6,      // 保护页 (不可访问)
    Device = 7,     // 设备MMIO映射
}

impl VmaType {
    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Anonymous,
            1 => Self::FileBacked,
            2 => Self::Stack,
            3 => Self::Heap,
            4 => Self::Vdso,
            5 => Self::Vsvar,
            6 => Self::Guard,
            7 => Self::Device,
            _ => Self::Guard,
        }
    }
}

/// 虚拟内存区域 (VMA): 描述一段地址区间的映射属性与后端信息.
#[derive(Debug, Clone)]
pub struct Vma {
    pub start: usize,
    pub end: usize,
    pub flags: PageFlags,
    pub vma_type: VmaType,
    pub offset: u64,
    /// 文件映射: inode 编号 (0 = 无文件后端)
    pub inode_id: u32,
    /// 文件映射: 是否为 `MAP_SHARED` (true) 或 `MAP_PRIVATE` (false)
    pub shared: bool,
    /// 文件映射: 创建该 VMA 的 pwm (用于 #PF 时 `vfs_pread_inode` 权限校验).
    /// 匿名/堆/栈/设备/保护 VMA 始终为 0.
    pub file_pwm: u64,
    /// 文件映射: 挂载点在 `VFS_MANAGER.mounts` 中的索引. None = 匿名 VMA
    /// 或未注册挂载 (退到根). #PF miss 时由 `page_fault` 读此字段
    /// 查 `VFS_MANAGER.mounts` `[idx].fs` trait object, 调对应 `FileSystem` 的
    /// `fs_pread_inode` 完成 mmap prewarm. 用 usize 而非 &str 避免
    /// 'static 借用 / 静态 buffer 泄漏的复杂度.
    pub mount_idx: Option<usize>,
    /// 内核策略标志 (madvice/mlock/fork 行为). 与 `PageFlags` 解耦.
    pub vm_flags: VmFlags,
    /// VMA 级 NUMA 内存策略 (`mbind` 设置). `None` = 未设置, 回退进程级策略
    /// (`NumaMempolicy`), 两级查询见 `privileged::mm::numa::effective_policy_for`.
    pub numa: Option<NumaRangePolicy>,
}

impl Vma {
    pub fn new(start: usize, end: usize, flags: PageFlags, vma_type: VmaType) -> Self {
        Self {
            start,
            end,
            flags,
            vma_type,
            offset: 0,
            inode_id: 0,
            shared: false,
            file_pwm: 0,
            mount_idx: None,
            vm_flags: VmFlags::EMPTY,
            numa: None,
        }
    }

    pub fn with_offset(start: usize, end: usize, flags: PageFlags, offset: u64) -> Self {
        Self {
            start,
            end,
            flags,
            vma_type: VmaType::FileBacked,
            offset,
            inode_id: 0,
            shared: false,
            file_pwm: 0,
            mount_idx: None,
            vm_flags: VmFlags::EMPTY,
            numa: None,
        }
    }

    /// 创建文件映射 VMA
    ///
    /// `pwm` 为创建该映射的进程凭证, #PF 同步填 pcache 时通过
    /// `vfs_pread_inode(mount_idx, inode, off, dst, pwm)` 校验文件访问权限,
    /// 避免越权读取其它用户文件. `mount_idx` 决定 #PF miss 时调哪个
    /// `FileSystem` trait (例如 0 → `RamFS`, 1 → `DevFS`).
    pub fn file_backed(
        start: usize,
        end: usize,
        flags: PageFlags,
        offset: u64,
        inode_id: u32,
        pwm: u64,
        shared: bool,
        mount_idx: Option<usize>,
    ) -> Self {
        Self {
            start,
            end,
            flags,
            vma_type: VmaType::FileBacked,
            offset,
            inode_id,
            shared,
            file_pwm: pwm,
            mount_idx,
            vm_flags: VmFlags::EMPTY,
            numa: None,
        }
    }

    #[inline]
    pub fn size(&self) -> usize {
        self.end - self.start
    }

    #[inline]
    pub fn contains(&self, addr: usize) -> bool {
        addr >= self.start && addr < self.end
    }

    #[inline]
    pub fn is_guard(&self) -> bool {
        self.vma_type == VmaType::Guard
    }

    #[inline]
    pub fn is_stack(&self) -> bool {
        self.vma_type == VmaType::Stack
    }

    /// VMA 是否被 mlock 锁定 (参与 swap/reclaim 跳过判断)
    #[inline]
    pub fn is_mlocked(&self) -> bool {
        self.vm_flags.contains(VmFlags::MLOCKED)
    }
}

/// 进程地址空间描述符 (mm): 持有 VMA 列表、brk 边界与 mlock 状态.
pub struct MmStruct {
    pub vmas: Mutex<Vec<Vma>>,
    pub start_brk: AtomicUsize,
    pub brk: AtomicUsize,
    pub start_stack: usize,
    pub mmap_base: usize,
    /// 已锁定物理字节数 (mlock 累计). 用于 `RLIMIT_MEMLOCK` 校验.
    /// 跨 fork 共享 (`MmStruct` 在 fork 中不复制, 见 `sys_fork`).
    pub locked_vm: AtomicUsize,
    /// 进程级 mlockall 标志 (`MCL_CURRENT` | `MCL_FUTURE` | `MCL_ONFAULT`).
    pub mlock_all_flags: AtomicU32,
}

impl MmStruct {
    pub fn new() -> Self {
        Self {
            vmas: Mutex::new(Vec::new()),
            start_brk: AtomicUsize::new(0),
            brk: AtomicUsize::new(0),
            start_stack: 0,
            mmap_base: 0,
            locked_vm: AtomicUsize::new(0),
            mlock_all_flags: AtomicU32::new(0),
        }
    }

    /// 查找包含 `addr` 的 VMA
    pub fn find_vma(&self, addr: usize) -> Option<Vma> {
        let vmas = self.vmas.lock();
        vmas.iter().find(|v| v.contains(addr)).cloned()
    }

    /// 判断 `[start, end)` 是否被现有 VMA 完整覆盖 (无空洞)
    ///
    /// `userfaultfd` 注册区间校验使用: 只有完整映射的范围才允许注册缺页拦截.
    ///
    /// 允许区间是某个 VMA 的真子集, 也允许跨多个相邻 VMA (目标区间本身无需对齐到 VMA
    /// 边界) — 与 [`Self::set_numa_policy_range`] 的"整 VMA 对齐"要求不同: uffd 注册
    /// 区间独立于 VMA 策略字段, 不存在"策略越界应用到整个 VMA"的问题.
    pub fn range_is_mapped(&self, start: usize, end: usize) -> bool {
        if end <= start {
            return false;
        }
        let vmas = self.vmas.lock();
        let mut covered = 0usize;
        for vma in vmas.iter() {
            if vma.end <= start || vma.start >= end {
                continue;
            }
            covered += vma.end.min(end) - vma.start.max(start);
        }
        covered == end - start
    }

    /// 查询 `addr` 所在 VMA 的 VMA 级 NUMA 策略 (两级查询的 VMA 层)
    ///
    /// 返回 `None` 表示该 VMA 未设置 `mbind` 策略, 调用方应回退进程级策略
    /// (`NumaMempolicy`). 两级组合见 `privileged::mm::numa::effective_policy_for`.
    pub fn numa_policy_at(&self, addr: usize) -> Option<NumaRangePolicy> {
        let vmas = self.vmas.lock();
        vmas.iter().find(|v| v.contains(addr)).and_then(|v| v.numa)
    }

    /// 设置 `[start, end)` 覆盖 VMA 的 VMA 级 NUMA 策略 (`mbind` 机制辅助)
    ///
    /// `policy` 为 `None` 表示清除 (Linux `MPOL_DEFAULT` 语义, 回退进程级策略).
    /// 返回被设置的字节数.
    ///
    /// SIMPLIFIED: 要求 `[start, end)` 被现有 VMA 完整覆盖且不跨 VMA 边界 (无 split_vma);
    /// 空洞或部分重叠一律 `EFAULT`, 不做拆分. 影响面: Linux "按 VMA 边界拆分后部分生效"
    /// 的调用在此被整体拒绝. 何时需扩展: 引入 split_vma 后改为逐 VMA 拆分处理.
    ///
    /// # Errors
    /// 当 `end <= start`, 或范围未被现有 VMA 完整覆盖时返回 `EFAULT`.
    pub fn set_numa_policy_range(
        &self,
        start: usize,
        end: usize,
        policy: Option<NumaRangePolicy>,
    ) -> Result<usize, crate::privileged::syscall::Errno> {
        use crate::privileged::errno::Errno;

        if end <= start {
            return Err(Errno::EFAULT);
        }

        let mut vmas = self.vmas.lock();

        // 第一遍: 校验完整覆盖 (避免部分 VMA 已改后再返回错误)
        let mut covered = 0usize;
        for vma in vmas.iter() {
            if vma.end <= start || vma.start >= end {
                continue;
            }
            if vma.start < start || vma.end > end {
                return Err(Errno::EFAULT);
            }
            covered += vma.end - vma.start;
        }
        if covered != end - start {
            return Err(Errno::EFAULT);
        }

        // 第二遍: 写入策略
        for vma in vmas.iter_mut() {
            if vma.start >= start && vma.end <= end {
                vma.numa = policy;
            }
        }

        Ok(covered)
    }

    /// 添加 VMA (合并相邻同类 VMA)
    ///
    /// # Errors
    /// 当新 VMA 与已有 VMA 重叠且类型 (`vma_type`) 或标志 (`flags`) 不同 (即不兼容映射) 时,
    /// 返回 `Err("VMA overlap with incompatible mapping")`.
    pub fn insert_vma(&self, vma: Vma) -> Result<(), &'static str> {
        let mut vmas = self.vmas.lock();

        for existing in vmas.iter() {
            if vma.start < existing.end && vma.end > existing.start {
                if existing.vma_type != vma.vma_type || existing.flags != vma.flags {
                    return Err("VMA overlap with incompatible mapping");
                }
            }
        }

        let mut merged = vma;
        let mut i = 0;
        while i < vmas.len() {
            let existing = &vmas[i];
            // file_pwm 不参与合并语义, 但不同 pwm 的相邻 VMA 不可合并:
            // 合并后用谁的 pwm 调用 vfs_pread_inode 都不严谨 (权限模型).
            if existing.vma_type != merged.vma_type
                || existing.flags != merged.flags
                || existing.file_pwm != merged.file_pwm
            {
                i += 1;
                continue;
            }
            let adjacent_or_overlap = merged.start <= existing.end && merged.end >= existing.start;
            if adjacent_or_overlap {
                let new_start = merged.start.min(existing.start);
                let new_end = merged.end.max(existing.end);
                merged.start = new_start;
                merged.end = new_end;
                vmas.remove(i);
                continue;
            }
            i += 1;
        }

        let pos = vmas
            .iter()
            .position(|v| v.start > merged.start)
            .unwrap_or(vmas.len());
        vmas.insert(pos, merged);

        Ok(())
    }

    /// 删除 [start, end) 范围内的 VMA 映射, 并拆除对应页表项
    ///
    /// `cr3` 为**目标用户页表根** (进程 `Process::cr3`). 拆除必须作用在与
    /// 建立侧相同的表上: 用户数据页由缺页路径建在进程用户页表上, 而
    /// `vmm.unmap_page` 等全局单表变体固定操作内核表
    /// (`KERNEL_PML4`), 两者低半区不同源 ⇒ 必须走显式变体.
    /// `cr3 == 0` 时显式变体内部直接返回 (fail-closed, 不触碰任何表).
    pub fn remove_range(&self, start: usize, end: usize, cr3: u64) {
        let mut vmas = self.vmas.lock();

        let mut i = 0;
        while i < vmas.len() {
            let vma_start = vmas[i].start;
            let vma_end = vmas[i].end;
            let vma_flags = vmas[i].flags;
            let vma_type = vmas[i].vma_type;

            if vma_end <= start {
                i += 1;
                continue;
            }

            if vma_start >= end {
                break;
            }

            if start <= vma_start && end >= vma_end {
                let removed = vmas.remove(i);
                self.unmap_vma_pages(&removed, cr3);
            } else if start <= vma_start {
                let mut truncated = vmas.remove(i);
                truncated.start = end;
                vmas.insert(i, truncated);
                self.unmap_vma_pages(&Vma::new(start, end, vma_flags, vma_type), cr3);
                i += 1;
            } else if end >= vma_end {
                let mut truncated = vmas.remove(i);
                truncated.end = start;
                vmas.insert(i, truncated);
                self.unmap_vma_pages(&Vma::new(start, vma_end, vma_flags, vma_type), cr3);
                i += 1;
            } else {
                let left = Vma::new(vma_start, start, vma_flags, vma_type);
                let right = Vma::new(end, vma_end, vma_flags, vma_type);
                let mid = Vma::new(start, end, vma_flags, vma_type);
                vmas.remove(i);
                vmas.insert(i, right);
                vmas.insert(i, left);
                self.unmap_vma_pages(&mid, cr3);
                i += 2;
            }
        }
    }

    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    fn unmap_vma_pages(&self, vma: &Vma, cr3: u64) {
        // 锁序: 调用者持有 VMA_LOCK, 此处获取 VMM_LOCK
        // 这是唯一合法的嵌套方向 (VMA → VMM).
        // 禁止在持有 VMM_LOCK 时获取 VMA_LOCK 以避免 ABBA 死锁.
        let vmm = super::vmm::get_vmm();
        let mut addr = vma.start;
        while addr < vma.end {
            // 显式变体: 拆除作用在进程用户页表上 (与建立侧同表),
            // 并承担 §8.1 规则 3 的帧持有注销 (frame_dec + 归零才延迟释放).
            vmm.unmap_page_in_table(cr3, VirtAddr(addr as u64));
            addr += PAGE_SIZE as usize;
        }
    }

    /// 查找空闲地址范围 (用于 mmap 的 hint-less 分配)
    pub fn find_free_range(&self, size: usize) -> Option<usize> {
        let vmas = self.vmas.lock();
        let mut cursor = self.mmap_base;

        for vma in vmas.iter() {
            if cursor + size <= vma.start {
                return Some(cursor);
            }
            cursor = vma.end;
        }

        // TASK_SIZE: 64 位用户地址空间上限 (集中于 constants::limits::USER_ADDR_MAX, B05-45)
        // SIMPLIFIED: usize 转换在 64 位系统下无损; 32 位系统需额外 assert (无 32 位目标, 当前可省略)
        let task_size: usize = crate::privileged::constants::limits::USER_ADDR_MAX as usize;

        if cursor + size <= task_size {
            Some(cursor)
        } else {
            None
        }
    }

    /// 修改 [start, end) 范围的保护属性 (mprotect 实现)
    ///
    /// 1. 查找与 [start, end) 重叠的所有 VMA
    /// 2. 必要时拆分 VMA (前后部分保持原权限)
    /// 3. 修改目标部分的 VMA flags 和页表权限
    /// 4. flush TLB
    ///
    /// `cr3` 为**目标用户页表根** (进程 `Process::cr3`): 权限改写必须作用在
    /// 用户页表上, 否则用户页权限根本未被修改 (内核表低半区与用户表不同源).
    ///
    /// # Errors
    /// 当 `len` 为 0 时返回 `EINVAL`; 当 `start + len` 溢出, 或范围内没有任何已映射的 VMA 时返回 `ENOMEM`.
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    pub fn mprotect(
        &self,
        start: usize,
        len: usize,
        new_flags: PageFlags,
        cr3: u64,
    ) -> Result<(), crate::privileged::syscall::Errno> {
        use crate::privileged::errno::Errno;

        if len == 0 {
            return Err(Errno::EINVAL);
        }

        let end = start.checked_add(len).ok_or(Errno::ENOMEM)?;
        let end = (end + PAGE_SIZE as usize - 1) & !(PAGE_SIZE as usize - 1); // 页对齐上界

        let mut vmas = self.vmas.lock();

        // 收集需要修改的 VMA 索引
        let mut to_modify: alloc::vec::Vec<(usize, usize, usize, PageFlags, VmaType)> =
            alloc::vec::Vec::new();

        for vma in vmas.iter() {
            if vma.end <= start {
                continue;
            }
            if vma.start >= end {
                break;
            }
            // 有重叠
            let overlap_start = vma.start.max(start);
            let overlap_end = vma.end.min(end);
            to_modify.push((
                overlap_start,
                overlap_end,
                vma.start,
                vma.flags,
                vma.vma_type,
            ));
        }

        if to_modify.is_empty() {
            return Err(Errno::ENOMEM); // 没有映射的区域
        }

        // 收集要保留的前后片段
        let mut fragments: alloc::vec::Vec<Vma> = alloc::vec::Vec::new();

        for vma in vmas.iter() {
            if vma.end <= start || vma.start >= end {
                // 无重叠, 保留
                fragments.push(vma.clone());
                continue;
            }

            // 前段: [vma.start, start)
            if vma.start < start {
                fragments.push(Vma {
                    start: vma.start,
                    end: start,
                    flags: vma.flags,
                    vma_type: vma.vma_type,
                    offset: vma.offset,
                    inode_id: vma.inode_id,
                    shared: vma.shared,
                    file_pwm: vma.file_pwm,
                    mount_idx: vma.mount_idx,
                    vm_flags: vma.vm_flags,
                    numa: vma.numa,
                });
            }

            // 中段: [overlap, overlap_end) — 新权限
            let overlap_start = vma.start.max(start);
            let overlap_end = vma.end.min(end);
            fragments.push(Vma {
                start: overlap_start,
                end: overlap_end,
                flags: new_flags,
                vma_type: vma.vma_type,
                offset: vma.offset,
                inode_id: vma.inode_id,
                shared: vma.shared,
                file_pwm: vma.file_pwm,
                mount_idx: vma.mount_idx,
                vm_flags: vma.vm_flags,
                numa: vma.numa,
            });

            // 后段: [end, vma.end)
            if vma.end > end {
                fragments.push(Vma {
                    start: end,
                    end: vma.end,
                    flags: vma.flags,
                    vma_type: vma.vma_type,
                    offset: vma.offset,
                    inode_id: vma.inode_id,
                    shared: vma.shared,
                    file_pwm: vma.file_pwm,
                    mount_idx: vma.mount_idx,
                    vm_flags: vma.vm_flags,
                    numa: vma.numa,
                });
            }
        }

        // 替换 VMA 列表
        *vmas = fragments;

        // 修改页表权限
        #[cfg(target_arch = "x86_64")]
        {
            let vmm = crate::privileged::mm::vmm::get_vmm();
            let page_start = start & !(PAGE_SIZE as usize - 1);
            let mut addr = page_start;
            while addr < end {
                vmm.protect_page_in_table(cr3, VirtAddr(addr as u64), new_flags);
                addr += PAGE_SIZE as usize;
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            let vmm = crate::privileged::mm::vmm::get_vmm();
            let page_start = start & !(PAGE_SIZE as usize - 1);
            let mut addr = page_start;
            while addr < end {
                vmm.protect_page_in_table(cr3, VirtAddr(addr as u64), new_flags);
                addr += PAGE_SIZE as usize;
            }
        }

        Ok(())
    }

    /// POSIX `mremap(old_addr, old_size, new_size, flags)` — VMA 描述符搬迁
    ///
    /// ## 语义
    ///
    /// - `new_size == 0`: 退化为 `munmap`, 返 0
    /// - `new_size <= old_size`: 截断尾部 VMA, 返 `old_addr`
    /// - `new_size > old_size`:
    ///   - `flags & MREMAP_MAYMOVE == 0`: 尝试原地扩展, 失败返 `EFAULT`
    ///   - `flags & MREMAP_MAYMOVE != 0`: 在空闲区分配新 VMA, 删除旧 VMA, 返新地址
    ///
    /// ## 物理页处理 (v1)
    ///
    /// v1 阶段只搬迁 VMA 描述符; 旧 vaddr 范围内的已触达物理页在新区域
    /// 触达时通过 page fault on-demand 重新 alloc (清零).
    /// v2 计划: 引入 page migration, 逐页 copy 旧→新.
    ///
    /// `cr3` 为**目标用户页表根** (进程 `Process::cr3`), 用于拆除被裁掉
    /// 区间的页表项.
    ///
    /// ## 错误
    ///
    /// - `EFAULT`: 旧地址未映射 / 范围不匹配 / 原地扩展失败
    /// - `EINVAL`: 大小参数为 0 或 `flags` 含未实现位
    /// - `ENOMEM`: 无法分配新范围
    ///
    /// # Errors
    /// 当 `old_size` 为 0 或 `flags` 含未实现位 (如 `MREMAP_FIXED`) 时返回 `EINVAL`;
    /// 当旧地址未映射、范围不匹配或原地扩展失败时返回 `EFAULT`;
    /// 当无法找到足够的空闲区 (`find_free_range` 失败) 或插入新 VMA 失败时返回 `ENOMEM`.
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    pub fn mremap(
        &self,
        old_addr: usize,
        old_size: usize,
        new_size: usize,
        flags: i32,
        cr3: u64,
    ) -> Result<usize, crate::privileged::syscall::Errno> {
        use crate::privileged::errno::Errno;

        // Linux mremap flags (仅 MAYMOVE = 1; MREMAP_FIXED = 2 由 glibc 模拟, 不支持)
        const MREMAP_MAYMOVE: i32 = 1;

        if old_size == 0 {
            return Err(Errno::EINVAL);
        }
        if flags & !MREMAP_MAYMOVE != 0 {
            // 含 MREMAP_FIXED 等未实现位
            return Err(Errno::EINVAL);
        }

        let old_size_aligned = (old_size + PAGE_SIZE as usize - 1) & !(PAGE_SIZE as usize - 1);

        // 验证旧 vma 存在且精确覆盖 [old_addr, old_addr+old_size_aligned)
        let old_vma = self.find_vma(old_addr).ok_or(Errno::EFAULT)?;
        if old_vma.start != old_addr || old_vma.end != old_addr + old_size_aligned {
            return Err(Errno::EFAULT);
        }

        // new_size == 0 退化为 munmap
        if new_size == 0 {
            self.remove_range(old_addr, old_addr + old_size_aligned, cr3);
            return Ok(0);
        }

        let new_size_aligned = (new_size + PAGE_SIZE as usize - 1) & !(PAGE_SIZE as usize - 1);

        // 缩小: 截断尾部
        if new_size_aligned <= old_size_aligned {
            self.remove_range(
                old_addr + new_size_aligned,
                old_addr + old_size_aligned,
                cr3,
            );
            return Ok(old_addr);
        }

        // 扩大: 必须 MAYMOVE
        if flags & MREMAP_MAYMOVE == 0 {
            // 原地扩展: 检查 old_addr+old_size 邻接空区
            let mut vmas = self.vmas.lock();
            // 找第一个 start >= old_addr+old_size 的 vma
            let boundary = old_addr + old_size_aligned;
            let mut next_start = usize::MAX;
            for v in vmas.iter() {
                if v.start >= boundary && v.start < next_start {
                    next_start = v.start;
                }
            }
            if next_start >= boundary + (new_size_aligned - old_size_aligned) {
                // 邻接空区足够大: 扩 VMA 尾
                for v in vmas.iter_mut() {
                    if v.start == old_addr {
                        v.end = boundary + (new_size_aligned - old_size_aligned);
                        return Ok(old_addr);
                    }
                }
            }
            return Err(Errno::EFAULT);
        }

        // MAYMOVE: 找新空区
        let new_start = self
            .find_free_range(new_size_aligned)
            .ok_or(Errno::ENOMEM)?;

        // 删除旧 vma
        self.remove_range(old_addr, old_addr + old_size_aligned, cr3);

        // 插入新 vma (继承旧 vma 的 flags / type / offset / inode_id / shared / file_pwm)
        let new_vma = Vma {
            start: new_start,
            end: new_start + new_size_aligned,
            flags: old_vma.flags,
            vma_type: old_vma.vma_type,
            offset: old_vma.offset,
            inode_id: old_vma.inode_id,
            shared: old_vma.shared,
            file_pwm: old_vma.file_pwm,
            mount_idx: old_vma.mount_idx,
            vm_flags: old_vma.vm_flags,
            numa: old_vma.numa,
        };
        self.insert_vma(new_vma).map_err(|_| Errno::ENOMEM)?;
        Ok(new_start)
    }

    /// 设置 brk 终点.
    ///
    /// `brk/start_brk` 使用 `AtomicUsize` 实现无锁线程安全访问.
    ///
    /// `cr3` 为**目标用户页表根** (进程 `Process::cr3`), 仅在堆收缩
    /// (拆除堆页) 时被使用.
    ///
    /// # Errors
    /// 当扩展堆时插入的新 VMA 与已有 VMA 重叠且不兼容时, 返回 `Err`
    /// (错误信息来自 `insert_vma`).
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    pub fn set_brk(&self, new_brk: usize, cr3: u64) -> Result<usize, &'static str> {
        let page_aligned = (new_brk + PAGE_SIZE as usize - 1) & !(PAGE_SIZE as usize - 1);

        let start_brk = self.start_brk.load(Ordering::Acquire);
        let current_brk = self.brk.load(Ordering::Acquire);

        if page_aligned > start_brk {
            let flags = PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::USER;
            let vma = Vma::new(current_brk, page_aligned, flags, VmaType::Heap);
            self.insert_vma(vma)?;
            self.brk.store(page_aligned, Ordering::Release);
        } else if page_aligned < current_brk {
            // 先更新 brk，防止其他 CPU 在 remove_range 后读到旧值
            // 去访问已被 unmap 的堆区域
            self.brk.store(page_aligned, Ordering::Release);
            self.remove_range(page_aligned, current_brk, cr3);
        }

        Ok(self.brk.load(Ordering::Acquire))
    }

    // ============================================================================
    // madvise / mlock / mincore 接口 (P1 #15)
    // ============================================================================

    /// madvise: 对 [start, end) 范围设置 `VmFlags` hint
    ///
    /// 返回成功锁定的字节数 (用于 mlock/mlockall 累计)
    /// 与 errno (MADV_* 实现细节见 Linux man).
    ///
    /// # Errors
    /// 当 `len` 为 0 或 `advice` 为未实现的建议值时返回 `EINVAL`;
    /// 当 `start + len` 溢出时返回 `ENOMEM`;
    /// 当 PAGEOUT/DONTNEED 目标区域被 `mlock` 锁定而无法回收时返回 `EAGAIN`.
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::too_many_lines,
        reason = "函数体超 100 行 (复杂度阈值); 拆分需追改调用链且增加间接层, 当前任务优先 expect 兑底"
    )]
    pub fn madvise_range(
        &self,
        start: usize,
        len: usize,
        advice: u32,
    ) -> Result<usize, crate::privileged::syscall::Errno> {
        use crate::privileged::errno::Errno;

        if len == 0 {
            return Err(Errno::EINVAL);
        }

        // advice → VmFlags bit
        // 已移除未实现项: KSM (7/8), THP (14/15), DONTFORK (10/11),
        // 预触达 (22/23), 软下线 (19), 冷标记 (20)
        let flag = match advice {
            4 => VmFlags::MADV_DONTNEED,
            5 => VmFlags::MADV_PAGEOUT,
            6 => VmFlags::MADV_FREE,
            1 => VmFlags::MADV_RANDOM,
            2 => VmFlags::MADV_SEQUENTIAL,
            3 => VmFlags::MADV_WILLNEED,
            _ => return Err(Errno::EINVAL),
        };

        let end_addr = start.checked_add(len).ok_or(Errno::ENOMEM)?;
        let end_addr = (end_addr + PAGE_SIZE as usize - 1) & !(PAGE_SIZE as usize - 1);

        let mut vmas = self.vmas.lock();
        let mut locked = 0usize;
        let mut touched: alloc::vec::Vec<(usize, usize)> = alloc::vec::Vec::new();

        for vma in vmas.iter_mut() {
            if vma.end <= start || vma.start >= end_addr {
                continue;
            }
            // 只取重叠部分
            let ovl_start = vma.start.max(start);
            let ovl_end = vma.end.min(end_addr);

            // 需要拆分时延后, 简化路径: 整 VMA 覆盖
            if vma.start >= start && vma.end <= end_addr {
                vma.vm_flags = vma.vm_flags.insert(flag);
                locked += ovl_end - ovl_start;
                touched.push((ovl_start, ovl_end));
            }
        }

        // 跨 VMA 边界的部分: 单独拆分处理
        let mut i = 0;
        while i < vmas.len() {
            let vma_start = vmas[i].start;
            let vma_end = vmas[i].end;
            if vma_end <= start || vma_start >= end_addr {
                i += 1;
                continue;
            }
            // 部分覆盖 [start, end) 范围, 拆分
            let prefix_end = start.max(vma_start);
            let suffix_start = end_addr.min(vma_end);
            if vma_start < start {
                // 拆分前缀 [vma_start, start)
                let prefix = Vma {
                    start: vma_start,
                    end: start,
                    flags: vmas[i].flags,
                    vma_type: vmas[i].vma_type,
                    offset: vmas[i].offset,
                    inode_id: vmas[i].inode_id,
                    shared: vmas[i].shared,
                    file_pwm: vmas[i].file_pwm,
                    mount_idx: vmas[i].mount_idx,
                    vm_flags: vmas[i].vm_flags,
                    numa: vmas[i].numa,
                };
                let new = Vma {
                    start,
                    end: vma_end,
                    flags: vmas[i].flags,
                    vma_type: vmas[i].vma_type,
                    offset: vmas[i].offset + (start - vma_start) as u64,
                    inode_id: vmas[i].inode_id,
                    shared: vmas[i].shared,
                    file_pwm: vmas[i].file_pwm,
                    mount_idx: vmas[i].mount_idx,
                    vm_flags: vmas[i].vm_flags.insert(flag),
                    numa: vmas[i].numa,
                };
                vmas[i] = prefix;
                vmas.insert(i + 1, new);
                let ins_start = vmas[i + 1].start;
                let ins_end = vmas[i + 1].end;
                locked += ins_end - ins_start;
                touched.push((ins_start, ins_end));
                i += 2;
            } else if vma_end > end_addr {
                // 拆分后缀 [end_addr, vma_end)
                let new = Vma {
                    start: vma_start,
                    end: end_addr,
                    flags: vmas[i].flags,
                    vma_type: vmas[i].vma_type,
                    offset: vmas[i].offset,
                    inode_id: vmas[i].inode_id,
                    shared: vmas[i].shared,
                    file_pwm: vmas[i].file_pwm,
                    mount_idx: vmas[i].mount_idx,
                    vm_flags: vmas[i].vm_flags.insert(flag),
                    numa: vmas[i].numa,
                };
                let suffix = Vma {
                    start: end_addr,
                    end: vma_end,
                    flags: vmas[i].flags,
                    vma_type: vmas[i].vma_type,
                    offset: vmas[i].offset + (end_addr - vma_start) as u64,
                    inode_id: vmas[i].inode_id,
                    shared: vmas[i].shared,
                    file_pwm: vmas[i].file_pwm,
                    mount_idx: vmas[i].mount_idx,
                    vm_flags: vmas[i].vm_flags,
                    numa: vmas[i].numa,
                };
                vmas[i] = new;
                vmas.insert(i + 1, suffix);
                let ins_start = vmas[i].start;
                let ins_end = vmas[i].end;
                locked += ins_end - ins_start;
                touched.push((ins_start, ins_end));
                i += 2;
            } else {
                i += 1;
            }

            // 避免前缀/后缀端点外溢
            let _ = prefix_end;
            let _ = suffix_start;
        }
        // PAGEOUT/DONTNEED 触发实际页面回收
        if flag == VmFlags::MADV_PAGEOUT || flag == VmFlags::MADV_DONTNEED {
            drop(vmas);
            for (s, e) in touched {
                self.madvise_evict_range(s, e, flag == VmFlags::MADV_DONTNEED)?;
            }
        }

        Ok(locked)
    }

    /// madvise 触发的页面回收 (PAGEOUT 走 swap, DONTNEED 走 free)
    ///
    /// 仅回收 locked=false 的页 (受 VmFlags.MLOCKED 保护).
    fn madvise_evict_range(
        &self,
        start: usize,
        end: usize,
        _dontneed: bool,
    ) -> Result<(), crate::privileged::syscall::Errno> {
        use crate::privileged::errno::Errno;
        use crate::privileged::mm::swap;

        // 检查 VMA 是否锁定
        {
            let vmas = self.vmas.lock();
            for v in vmas.iter() {
                if v.start <= start && v.end >= end && v.is_mlocked() {
                    return Err(Errno::EAGAIN);
                }
            }
        }

        // 当前 LRU 没有 per-virt 跨 mm 区分, 简化:
        // 触发 kswapd 周期回收
        swap::kswapd_wakeup();

        Ok(())
    }

    /// mlock: 锁定 [start, len) 范围 VMA
    ///
    /// 返回实际锁定的字节数. 受 `RLIMIT_MEMLOCK` 约束.
    ///
    /// # Errors
    /// 当 `len` 为 0 时返回 `EINVAL`; 当 `start + len` 溢出、范围内没有任何重叠 VMA,
    /// 或累计锁定字节数超过 `RLIMIT_MEMLOCK` 上限时返回 `ENOMEM`.
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    pub fn mlock_range(
        &self,
        start: usize,
        len: usize,
    ) -> Result<usize, crate::privileged::syscall::Errno> {
        use crate::privileged::errno::Errno;
        use crate::privileged::rlimit_query;

        if len == 0 {
            return Err(Errno::EINVAL);
        }

        let end_addr = start.checked_add(len).ok_or(Errno::ENOMEM)?;
        let end_addr = (end_addr + PAGE_SIZE as usize - 1) & !(PAGE_SIZE as usize - 1);

        // 验证范围在 VMA 中
        let mut total = 0usize;
        {
            let vmas = self.vmas.lock();
            for v in vmas.iter() {
                if v.end <= start || v.start >= end_addr {
                    continue;
                }
                let ovl_start = v.start.max(start);
                let ovl_end = v.end.min(end_addr);
                total += ovl_end - ovl_start;
            }
        }

        if total == 0 {
            return Err(Errno::ENOMEM);
        }

        // RLIMIT_MEMLOCK 检查
        let current = self.locked_vm.load(Ordering::Acquire);
        if rlimit_query::check_memlock_exceeded(current as u64, total as u64) {
            return Err(Errno::ENOMEM);
        }

        // 标记 VMA MLOCKED + 累计 locked_vm
        let mut vmas = self.vmas.lock();
        for v in vmas.iter_mut() {
            if v.end <= start || v.start >= end_addr {
                continue;
            }
            v.vm_flags = v.vm_flags.insert(VmFlags::MLOCKED);
        }
        self.locked_vm.fetch_add(total, Ordering::AcqRel);

        // 对范围内已触达页设置 LRU locked
        drop(vmas);
        let page_size = PAGE_SIZE as usize;
        let mut addr = start;
        while addr < end_addr {
            crate::privileged::mm::set_page_locked(addr as u64, true);
            addr += page_size;
        }

        Ok(total)
    }

    /// munlock: 解锁 [start, len) 范围 VMA
    ///
    /// # Errors
    /// 当 `len` 为 0 时返回 `EINVAL`; 当 `start + len` 溢出时返回 `ENOMEM`.
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    pub fn munlock_range(
        &self,
        start: usize,
        len: usize,
    ) -> Result<usize, crate::privileged::syscall::Errno> {
        use crate::privileged::errno::Errno;

        if len == 0 {
            return Err(Errno::EINVAL);
        }

        let end_addr = start.checked_add(len).ok_or(Errno::ENOMEM)?;
        let end_addr = (end_addr + PAGE_SIZE as usize - 1) & !(PAGE_SIZE as usize - 1);

        let mut total = 0usize;
        let mut vmas = self.vmas.lock();
        for v in vmas.iter_mut() {
            if v.end <= start || v.start >= end_addr {
                continue;
            }
            v.vm_flags = v.vm_flags.remove(VmFlags::MLOCKED);
            let ovl_start = v.start.max(start);
            let ovl_end = v.end.min(end_addr);
            total += ovl_end - ovl_start;
        }
        let _ = self
            .locked_vm
            .try_update(Ordering::AcqRel, Ordering::Acquire, |cur| {
                Some(cur.saturating_sub(total))
            });
        drop(vmas);

        let page_size = PAGE_SIZE as usize;
        let mut addr = start;
        while addr < end_addr {
            crate::privileged::mm::set_page_locked(addr as u64, false);
            addr += page_size;
        }

        Ok(total)
    }

    /// mlockall: 进程级 mlock
    ///
    /// `flags` 取值: `MCL_CURRENT=1`, `MCL_FUTURE=2`, `MCL_ONFAULT=4`.
    /// 返回成功设置的标志位.
    ///
    /// # Errors
    /// 当 `flags` 包含未实现的位 (除 `MCL_CURRENT | MCL_FUTURE | MCL_ONFAULT` 之外的位) 时返回 `EINVAL`.
    pub fn mlock_all(&self, flags: u32) -> Result<u32, crate::privileged::syscall::Errno> {
        use crate::privileged::errno::Errno;

        const MCL_CURRENT: u32 = 1;
        const MCL_FUTURE: u32 = 2;
        const MCL_ONFAULT: u32 = 4;

        if flags & !(MCL_CURRENT | MCL_FUTURE | MCL_ONFAULT) != 0 {
            return Err(Errno::EINVAL);
        }

        let mut applied = 0u32;
        let mut vmas = self.vmas.lock();

        if flags & MCL_CURRENT != 0 {
            // 锁定所有现有 VMA
            for v in vmas.iter_mut() {
                if v.vma_type == VmaType::Guard || v.vma_type == VmaType::Device {
                    continue;
                }
                v.vm_flags = v.vm_flags.insert(VmFlags::MLOCKED);
            }
            self.locked_vm.store(usize::MAX / 2, Ordering::Release); // 简化
            applied |= MCL_CURRENT;
        }

        if flags & MCL_FUTURE != 0 {
            // 后续 mmap 自动锁定: 在 MmStruct 中设置 LOCKED_FUTURE
            self.mlock_all_flags.fetch_or(MCL_FUTURE, Ordering::AcqRel);
            // 同步到所有现有 VMA 的 VmFlags (影响后续 #PF)
            for v in vmas.iter_mut() {
                v.vm_flags = v.vm_flags.insert(VmFlags::LOCKED_FUTURE);
            }
            applied |= MCL_FUTURE;
        }

        if flags & MCL_ONFAULT != 0 {
            self.mlock_all_flags.fetch_or(MCL_ONFAULT, Ordering::AcqRel);
            for v in vmas.iter_mut() {
                v.vm_flags = v.vm_flags.insert(VmFlags::LOCKED_ONFAULT);
            }
            applied |= MCL_ONFAULT;
        }

        Ok(applied)
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
    )]
    /// munlockall: 解除所有 mlock
    ///
    /// # Errors
    /// 当前实现总是返回 `Ok(())`, 不会返回错误.
    pub fn munlock_all(&self) -> Result<(), crate::privileged::syscall::Errno> {
        let mut vmas = self.vmas.lock();
        for v in vmas.iter_mut() {
            v.vm_flags = v
                .vm_flags
                .remove(VmFlags::MLOCKED)
                .remove(VmFlags::LOCKED_FUTURE)
                .remove(VmFlags::LOCKED_ONFAULT);
        }
        self.locked_vm.store(0, Ordering::Release);
        self.mlock_all_flags.store(0, Ordering::Release);
        Ok(())
    }

    /// mincore: 查询 [start, len) 范围每页是否驻留
    ///
    /// `out_vec`: 输出缓冲区, 每页 1 字节 (1=驻留, 0=未驻留)
    /// 返回 0 成功, 否则 errno.
    ///
    /// # Errors
    /// 当 `len` 为 0 时返回 `EINVAL`; 当 `out_vec` 容量不足以容纳全部页的驻留状态
    /// (`out_vec.len() < n_pages`) 时返回 `ENOMEM`.
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    pub fn mincore_range(
        &self,
        start: usize,
        len: usize,
        out_vec: &mut [u8],
    ) -> Result<usize, crate::privileged::syscall::Errno> {
        use crate::privileged::errno::Errno;
        use crate::privileged::mm::VirtAddr;
        use crate::privileged::mm::vmm;

        if len == 0 {
            return Err(Errno::EINVAL);
        }

        let page_size = PAGE_SIZE as usize;
        let n_pages = (len + page_size - 1) / page_size;
        if out_vec.len() < n_pages {
            return Err(Errno::ENOMEM);
        }

        let vmm_inst = vmm::get_vmm();
        let pml4 = vmm::get_current_pml4();
        let mut resident = 0usize;

        for i in 0..n_pages {
            let addr = start + i * page_size;
            let pte = vmm_inst.get_pte_value(pml4, VirtAddr(addr as u64));
            let present = pte.is_some_and(|p| (p & 1) != 0);
            out_vec[i] = u8::from(present);
            if present {
                resident += 1;
            }
        }

        Ok(resident)
    }
}

// SAFETY: MmStruct 对 Vec<Vma> 使用 Mutex, brk/start_brk 使用 AtomicUsize.
// 所有可变字段都经过适当的同步原语.
// start_stack/mmap_base 在 init 后只读.
// SAFETY: MmStruct 含裸指针, 但跨线程共享访问是安全的, 因为所有变更都通过原子操作与锁在内部同步.
unsafe impl Send for MmStruct {}
// SAFETY: 同上, 原子操作与锁保证并发安全.
unsafe impl Sync for MmStruct {}

static CURRENT_MM: core::sync::atomic::AtomicPtr<MmStruct> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

#[expect(
    clippy::ptr_cast_constness,
    reason = "ptr_cast_constness: *mut T as *const T 是已知安全 (Rust 2024 可用 ptr.cast_const 或 &raw const; 当前优先 expect"
)]
/// 设置当前进程的地址空间指针 (进程切换时由调度器调用).
pub fn set_current_mm(mm: *const MmStruct) {
    // SAFETY: CURRENT_MM 是当前 CPU 的 per-CPU 状态指针，
    // 仅在进程切换时由调度器写入，调用者保证无并发写入。
    CURRENT_MM.store(mm as *mut MmStruct, core::sync::atomic::Ordering::Release);
}

/// 获取当前进程的地址空间引用 (未设置时为 `None`).
pub fn get_current_mm() -> Option<&'static MmStruct> {
    // SAFETY: CURRENT_MM 在 set_current_mm 中设置，
    // 要么为 null，要么指向有效的 MmStruct。
    // 返回 &'static 引用是安全的，因为 MmStruct 生命周期
    // 与进程一致，进程存在期间指针有效。
    let ptr = CURRENT_MM.load(core::sync::atomic::Ordering::Acquire);
    if ptr.is_null() {
        None
    } else {
        // SAFETY: ptr 由 set_current_mm 写入，保证为有效 MmStruct 指针
        Some(unsafe { &*ptr })
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
pub extern "C" fn vma_find(mm_ptr: *const MmStruct, addr: u64) -> u64 {
    if mm_ptr.is_null() {
        return 0;
    }
    // SAFETY: `mm_ptr` 由调用方保证为有效指针; 只读访问
    let mm = unsafe { &*mm_ptr };
    mm.find_vma(addr as usize).map_or(0, |vma| {
        let flags = vma.flags.bits();
        ((vma.start as u64) << 32) | flags
    })
}
