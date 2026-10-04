#![deny(unsafe_code)]

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use super::ramfs_node::{RamFsACE, RamFsDirEntry, RamFsNode};
use super::{
    DIRECT_BLOCKS, FS_CAP_CREATE, FS_CAP_READ, FS_CAP_WRITE, INDIRECT_BLOCKS_PER_BLOCK,
    RAMFS_BLOCK_SIZE, RAMFS_MAX_ACES, RAMFS_MAX_BLOCKS, RAMFS_MAX_NODES, SENSITIVITY_PUBLIC,
};
use crate::functions::fs::KernelError;
use crate::functions::fs::dcache;
use crate::functions::fs::{VFS_MAX_NAME, VfsFileType, VfsSeekWhence, VfsStat};
use crate::privileged::mm::LazyPageArena;
use crate::privileged::sgeg::api as pwm_api;

pub struct RamFsData {
    /// 文件系统实例标识 (dcache/icache 命名空间, 0 = 未挂载)
    ///
    /// 多个 `RamFsData` 实例 (全局 `RAMFS_DATA` / overlay 内嵌 upper/work 层 /
    /// `SafeRamFs` / `TmpFsData`) 根 inode 同为 1、节点号重叠, 独立的 `fs_id`
    /// 使共享的全局 dcache/icache 不会跨实例污染.
    pub fs_id: u32,
    /// 节点表 — 惰性堆置 (见 [`RamFsData::allocate`]), 避免 BSS 常驻大数组
    pub nodes: Vec<RamFsNode>,
    /// 文件数据区 — 惰性页池 (每块 4KB, 首次写入时按需向 PMM 申请物理页)
    pub data_area: LazyPageArena<RAMFS_MAX_BLOCKS>,
    /// 节点位图 — 惰性堆置
    pub node_bitmap: Vec<u8>,
    /// 数据块位图 — 惰性堆置
    pub block_bitmap: Vec<u8>,
    /// ACE 访问控制表 — 惰性堆置
    pub aces: Vec<RamFsACE>,
    /// 符号链接目标 — 惰性堆置
    pub symlink_targets: Vec<[u8; 128]>,
    /// 符号链接长度 — 惰性堆置
    pub symlink_lens: Vec<u8>,
    pub root_node: u32,
    pub free_nodes: AtomicU32,
    pub free_blocks: AtomicU32,
}

// RamFsData 全部字段自动实现 Send + Sync, 无需手动 impl.

/// 全局 fs 实例标识分配器 (从 1 开始, 0 保留表示"未挂载")
static NEXT_FS_ID: AtomicU32 = AtomicU32::new(1);

/// 分配全局唯一的 fs 实例标识
///
/// 每次 `RamFsData` 挂载时调用一次, 用于隔离共享 dcache/icache 的命名空间.
/// 回绕到 0 时跳过 (0 保留给"未挂载").
fn alloc_fs_id() -> u32 {
    let id = NEXT_FS_ID.fetch_add(1, Ordering::SeqCst);
    // 回绕到 0 时改发 1 (0 保留给"未挂载")
    if id == 0 { 1 } else { id }
}

impl RamFsData {
    /// 构造空实例 (各表均为空 `Vec`, 不分配堆内存).
    ///
    /// 供 BSS 静态 (`RAMFS_DATA` / `TMPFS_DATA` / `OVERLAY_FS` / `ANONYMOUS_FS`)
    /// 与 `#[cfg(test)]` 静态常量初始化使用. 真正的表内存由
    /// [`RamFsData::allocate`] 在堆就绪后惰性分配.
    pub const fn new() -> Self {
        Self {
            fs_id: 0,
            nodes: Vec::new(),
            data_area: LazyPageArena::new(),
            node_bitmap: Vec::new(),
            block_bitmap: Vec::new(),
            aces: Vec::new(),
            symlink_targets: Vec::new(),
            symlink_lens: Vec::new(),
            root_node: 0,
            free_nodes: AtomicU32::new(0),
            free_blocks: AtomicU32::new(0),
        }
    }

    /// 惰性堆置: 将各表填满到各自容量 (与旧静态数组语义等价).
    ///
    /// 逐元素 `map + collect` 构造, 避免栈上大临时对象与 rodata 模板.
    /// 由 [`RamFsData::mount`] 在挂载时调用, 或由无挂载路径的实例
    /// (如 `ANONYMOUS_FS`) 通过 [`RamFsData::ensure_allocated`] 触发.
    pub fn allocate(&mut self) {
        self.nodes = (0..RAMFS_MAX_NODES).map(|_| RamFsNode::new()).collect();
        self.node_bitmap = (0..RAMFS_MAX_NODES / 8).map(|_| 0u8).collect();
        self.block_bitmap = (0..RAMFS_MAX_BLOCKS / 8).map(|_| 0u8).collect();
        self.aces = (0..RAMFS_MAX_ACES).map(|_| RamFsACE::new()).collect();
        self.symlink_targets = (0..RAMFS_MAX_NODES).map(|_| [0u8; 128]).collect();
        self.symlink_lens = (0..RAMFS_MAX_NODES).map(|_| 0u8).collect();
    }

    /// 幂等惰性堆置: 若各表尚未分配则调用 [`RamFsData::allocate`].
    ///
    /// 供从不经 `mount` 的实例 (如 `ANONYMOUS_FS`) 在访问前确保表已就绪.
    pub fn ensure_allocated(&mut self) {
        if self.nodes.is_empty() {
            self.allocate();
        }
    }

    fn get_time() -> u64 {
        crate::arch!(timestamp())
    }

    fn block_is_free(&self, block_num: u32) -> bool {
        if block_num as usize >= RAMFS_MAX_BLOCKS {
            return false;
        }
        let byte_idx = (block_num / 8) as usize;
        let bit_idx = (block_num % 8) as usize;
        (self.block_bitmap[byte_idx] & (1 << bit_idx)) == 0
    }

    fn block_set_used(&mut self, block_num: u32) {
        if block_num as usize >= RAMFS_MAX_BLOCKS {
            return;
        }
        let byte_idx = (block_num / 8) as usize;
        let bit_idx = (block_num % 8) as usize;
        self.block_bitmap[byte_idx] |= 1 << bit_idx;
        self.free_blocks.fetch_sub(1, Ordering::SeqCst);
    }

    fn block_set_free(&mut self, block_num: u32) {
        if block_num as usize >= RAMFS_MAX_BLOCKS {
            return;
        }
        let byte_idx = (block_num / 8) as usize;
        let bit_idx = (block_num % 8) as usize;
        self.block_bitmap[byte_idx] &= !(1 << bit_idx);
        self.free_blocks.fetch_add(1, Ordering::SeqCst);
    }

    fn get_or_alloc_block(
        node: &mut RamFsNode,
        data_area: &mut LazyPageArena<RAMFS_MAX_BLOCKS>,
        block_bitmap: &mut [u8],
        free_blocks: &AtomicU32,
        block_idx: usize,
    ) -> Option<u32> {
        let direct_limit = DIRECT_BLOCKS;
        let indirect_limit = direct_limit + INDIRECT_BLOCKS_PER_BLOCK;
        let double_indirect_limit =
            indirect_limit + INDIRECT_BLOCKS_PER_BLOCK * INDIRECT_BLOCKS_PER_BLOCK;

        if block_idx < direct_limit {
            if node.direct_blocks[block_idx] == 0 {
                let new_block = Self::alloc_block_internal(data_area, block_bitmap, free_blocks);
                if new_block == u32::MAX {
                    return None;
                }
                node.direct_blocks[block_idx] = new_block;
            }
            Some(node.direct_blocks[block_idx])
        } else if block_idx < indirect_limit {
            if node.indirect_block == 0 {
                let new_indirect = Self::alloc_block_internal(data_area, block_bitmap, free_blocks);
                if new_indirect == u32::MAX {
                    return None;
                }
                node.indirect_block = new_indirect;
            }

            let indirect_offset = block_idx - direct_limit;
            let indirect_ptr_addr =
                node.indirect_block as usize * RAMFS_BLOCK_SIZE + indirect_offset * 4;

            let existing_block: u32 = data_area.read_u32(indirect_ptr_addr);

            if existing_block == 0 {
                let new_data_block =
                    Self::alloc_block_internal(data_area, block_bitmap, free_blocks);
                if new_data_block == u32::MAX {
                    return None;
                }

                if !data_area.write_u32(indirect_ptr_addr, new_data_block) {
                    return None;
                }

                Some(new_data_block)
            } else {
                Some(existing_block)
            }
        } else if block_idx < double_indirect_limit {
            if node.double_indirect_block == 0 {
                let new_double_indirect =
                    Self::alloc_block_internal(data_area, block_bitmap, free_blocks);
                if new_double_indirect == u32::MAX {
                    return None;
                }
                node.double_indirect_block = new_double_indirect;
            }

            let double_indirect_offset = block_idx - indirect_limit;
            let indirect_index = double_indirect_offset / INDIRECT_BLOCKS_PER_BLOCK;
            let block_index_in_indirect = double_indirect_offset % INDIRECT_BLOCKS_PER_BLOCK;

            let indirect_ptr_addr =
                node.double_indirect_block as usize * RAMFS_BLOCK_SIZE + indirect_index * 4;

            let existing_indirect: u32 = data_area.read_u32(indirect_ptr_addr);

            let indirect_block_num = if existing_indirect == 0 {
                let new_indirect = Self::alloc_block_internal(data_area, block_bitmap, free_blocks);
                if new_indirect == u32::MAX {
                    return None;
                }

                if !data_area.write_u32(indirect_ptr_addr, new_indirect) {
                    return None;
                }

                new_indirect
            } else {
                existing_indirect
            };

            let data_ptr_addr =
                indirect_block_num as usize * RAMFS_BLOCK_SIZE + block_index_in_indirect * 4;

            let existing_data: u32 = data_area.read_u32(data_ptr_addr);

            if existing_data == 0 {
                let new_data_block =
                    Self::alloc_block_internal(data_area, block_bitmap, free_blocks);
                if new_data_block == u32::MAX {
                    return None;
                }

                if !data_area.write_u32(data_ptr_addr, new_data_block) {
                    return None;
                }

                Some(new_data_block)
            } else {
                Some(existing_data)
            }
        } else {
            None
        }
    }

    fn alloc_block_internal(
        data_area: &mut LazyPageArena<RAMFS_MAX_BLOCKS>,
        block_bitmap: &mut [u8],
        free_blocks: &AtomicU32,
    ) -> u32 {
        for i in 0..RAMFS_MAX_BLOCKS {
            let byte_idx = i / 8;
            let bit_idx = i % 8;
            if (block_bitmap[byte_idx] & (1 << bit_idx)) == 0 {
                block_bitmap[byte_idx] |= 1 << bit_idx;
                free_blocks.fetch_sub(1, Ordering::SeqCst);

                // 页池分配失败时回滚位图与计数, 保持与原「无可用块」语义一致
                if !data_area.zero(i * RAMFS_BLOCK_SIZE, RAMFS_BLOCK_SIZE) {
                    block_bitmap[byte_idx] &= !(1 << bit_idx);
                    free_blocks.fetch_add(1, Ordering::SeqCst);
                    return u32::MAX;
                }
                return i as u32;
            }
        }
        u32::MAX
    }

    fn block_alloc(&mut self) -> u32 {
        for i in 0..RAMFS_MAX_BLOCKS {
            if self.block_is_free(i as u32) {
                self.block_set_used(i as u32);
                // 页池分配失败时回滚, 返回 u32::MAX (与原「无可用块」语义一致)
                if !self.data_area.zero(i * RAMFS_BLOCK_SIZE, RAMFS_BLOCK_SIZE) {
                    self.block_set_free(i as u32);
                    return u32::MAX;
                }
                return i as u32;
            }
        }
        u32::MAX
    }

    fn node_set_used(&mut self, node_id: u32) {
        if node_id as usize >= RAMFS_MAX_NODES {
            return;
        }
        let byte_idx = (node_id / 8) as usize;
        let bit_idx = (node_id % 8) as usize;
        self.node_bitmap[byte_idx] |= 1 << bit_idx;
        self.free_nodes.fetch_sub(1, Ordering::SeqCst);
    }

    /// 从页池读取一个目录项 (72 字节, 可能跨页, 经 72B 栈缓冲中转)
    fn read_dirent(arena: &LazyPageArena<RAMFS_MAX_BLOCKS>, offset: usize) -> RamFsDirEntry {
        let mut buf = [0u8; core::mem::size_of::<RamFsDirEntry>()];
        arena.read(offset, &mut buf);
        RamFsDirEntry::read_at(&buf, 0)
    }

    /// 将目录项写入页池; 返回 `false` = 目标页分配失败
    fn write_dirent(
        arena: &mut LazyPageArena<RAMFS_MAX_BLOCKS>,
        offset: usize,
        entry: &RamFsDirEntry,
    ) -> bool {
        let mut buf = [0u8; core::mem::size_of::<RamFsDirEntry>()];
        entry.write_at(&mut buf, 0);
        arena.write(offset, &buf)
    }

    fn free_indirect_chain(&mut self, indirect_block: u32, start_idx: usize, end_idx: usize) {
        if indirect_block == 0 {
            return;
        }

        for i in start_idx..end_idx.min(INDIRECT_BLOCKS_PER_BLOCK) {
            let ptr_addr = indirect_block as usize * RAMFS_BLOCK_SIZE + i * 4;
            let block_num: u32 = self.data_area.read_u32(ptr_addr);
            if block_num != 0 {
                self.block_set_free(block_num);
            }
        }

        self.block_set_free(indirect_block);
    }

    fn free_double_indirect_chain(
        &mut self,
        double_indirect_block: u32,
        start_global_idx: usize,
        end_global_idx: usize,
    ) {
        if double_indirect_block == 0 {
            return;
        }

        let start_indirect_idx = start_global_idx / INDIRECT_BLOCKS_PER_BLOCK;
        let end_indirect_idx = end_global_idx.div_ceil(INDIRECT_BLOCKS_PER_BLOCK);

        for indirect_idx in start_indirect_idx..end_indirect_idx.min(INDIRECT_BLOCKS_PER_BLOCK) {
            let indirect_ptr_addr =
                double_indirect_block as usize * RAMFS_BLOCK_SIZE + indirect_idx * 4;

            let indirect_block_num: u32 = self.data_area.read_u32(indirect_ptr_addr);

            if indirect_block_num != 0 {
                let local_start = if indirect_idx == start_indirect_idx {
                    start_global_idx % INDIRECT_BLOCKS_PER_BLOCK
                } else {
                    0
                };

                let local_end = if indirect_idx == end_indirect_idx - 1 {
                    end_global_idx % INDIRECT_BLOCKS_PER_BLOCK
                } else {
                    INDIRECT_BLOCKS_PER_BLOCK
                };

                if local_end > local_start {
                    self.free_indirect_chain(indirect_block_num, local_start, local_end);
                }
            }
        }

        self.block_set_free(double_indirect_block);
    }

    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    fn check_permission(&self, node: &RamFsNode, pwm: u64, cap: u64) -> bool {
        let level = pwm_api::pwm_get_privilege_level(pwm);

        if level == 0xFF {
            return false;
        }

        if level > 0 && node.sensitivity > 0 {
            let clearance = match level {
                0 => 255u8,
                1 => 255u8,
                2 => 128u8,
                _ => 64u8,
            };
            if clearance < node.sensitivity {
                return false;
            }
        }

        let ino = node.node_id;
        for ace in &self.aces {
            if ace.used && ace.node_id == ino {
                if ace.pwm == 0 || ace.pwm == pwm {
                    if (ace.deny_mask & cap) != 0 {
                        return false;
                    }
                    if (ace.allow_mask & cap) != 0 {
                        return true;
                    }
                }
            }
        }

        let caps = pwm_api::pwm_get_fs_capability(pwm);
        if (caps & cap) == cap {
            return true;
        }

        if node.owner_pwm != 0 && node.owner_pwm != pwm {
            let has_cap = pwm_api::pwm_has_capability(pwm, 1, cap);
            if has_cap {
                return true;
            }
        }

        false
    }

    pub fn resolve_path(&self, path: &str) -> Option<u32> {
        let mut current = self.root_node;
        let p = path.trim_start_matches('/');

        if p.is_empty() {
            return Some(current);
        }

        for component in p.split('/') {
            if component.is_empty() {
                continue;
            }

            // dcache 快速路径
            match dcache::dcache_lookup(self.fs_id, current, component) {
                dcache::DCacheResult::Hit { ino, file_type: _ } => {
                    current = ino;
                    continue;
                }
                dcache::DCacheResult::Negative => {
                    return None;
                }
                dcache::DCacheResult::Miss => {}
            }

            let node = &self.nodes[current as usize];

            if node.file_type != VfsFileType::Dir as u8 {
                return None;
            }

            let block_num = node.direct_blocks[0];
            if block_num == u32::MAX {
                return None;
            }

            let dirent_size = core::mem::size_of::<RamFsDirEntry>();
            let num_entries = node.size as usize / dirent_size;

            let mut found = false;

            for i in 0..num_entries {
                let offset = (block_num as usize) * RAMFS_BLOCK_SIZE + i * dirent_size;
                let entry = Self::read_dirent(&self.data_area, offset);

                if entry.node != 0 {
                    let end = entry
                        .name
                        .iter()
                        .position(|&b| b == 0)
                        .unwrap_or(VFS_MAX_NAME);
                    let name = core::str::from_utf8(&entry.name[..end]).unwrap_or("");
                    if name == component {
                        current = entry.node;
                        found = true;
                        dcache::dcache_insert(
                            self.fs_id,
                            node.node_id,
                            component,
                            entry.node,
                            entry.file_type,
                        );
                        break;
                    }
                }
            }

            if !found {
                dcache::dcache_insert_negative(self.fs_id, node.node_id, component);
                return None;
            }
        }

        Some(current)
    }

    pub fn mount(&mut self, _path: &str) -> i32 {
        // 分配全局唯一的 fs 实例标识, 使共享的 dcache/icache 按实例隔离
        self.fs_id = alloc_fs_id();
        // 惰性堆置各表 (等价于旧 `fill` 逐表清零), 建立空表基线
        self.allocate();
        self.data_area.clear();

        self.free_nodes
            .store((RAMFS_MAX_NODES - 1) as u32, Ordering::SeqCst);
        self.free_blocks
            .store(RAMFS_MAX_BLOCKS as u32, Ordering::SeqCst);
        self.root_node = 1;

        let block = self.block_alloc();

        self.nodes[1] = RamFsNode {
            node_id: 1,
            file_type: VfsFileType::Dir as u8,
            sensitivity: SENSITIVITY_PUBLIC,
            owner_pwm: 1,
            group_pwm: 1,
            perm: 0o777,
            size: (2 * core::mem::size_of::<RamFsDirEntry>()) as u32,
            atime: Self::get_time(),
            mtime: Self::get_time(),
            ctime: Self::get_time(),
            direct_blocks: [block, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            indirect_block: 0,
            double_indirect_block: 0,
            link_count: 2,
            used: true,
        };
        self.node_set_used(1);

        let dirent_size = core::mem::size_of::<RamFsDirEntry>();
        let offset = (block as usize) * RAMFS_BLOCK_SIZE;

        let mut dot = Self::read_dirent(&self.data_area, offset);
        dot.node = 1;
        dot.file_type = VfsFileType::Dir as u8;
        dot.set_name(".");
        Self::write_dirent(&mut self.data_area, offset, &dot);

        let mut dotdot = Self::read_dirent(&self.data_area, offset + dirent_size);
        dotdot.node = 1;
        dotdot.file_type = VfsFileType::Dir as u8;
        dotdot.set_name("..");
        Self::write_dirent(&mut self.data_area, offset + dirent_size, &dotdot);

        0
    }

    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    pub fn open(&mut self, path: &str, _flags: u32, pwm: u64) -> Option<(u32, u64, u8)> {
        if path.is_empty() {
            return None;
        }

        let node_id = match self.resolve_path(path) {
            Some(n) => n,
            None => return None,
        };

        if node_id as usize >= RAMFS_MAX_NODES || !self.nodes[node_id as usize].used {
            return None;
        }

        if !self.check_permission(&self.nodes[node_id as usize], pwm, FS_CAP_READ) {
            return None;
        }

        self.nodes[node_id as usize].atime = Self::get_time();

        Some((node_id, 0, self.nodes[node_id as usize].file_type))
    }

    pub fn alloc_node(&mut self, file_type: u8, pwm: u64) -> Option<u32> {
        for i in 1..RAMFS_MAX_NODES {
            if !self.nodes[i].used {
                let block = self.block_alloc();
                self.nodes[i] = RamFsNode {
                    node_id: i as u32,
                    file_type,
                    sensitivity: SENSITIVITY_PUBLIC,
                    owner_pwm: pwm,
                    group_pwm: pwm,
                    perm: 0o644,
                    size: if file_type == VfsFileType::Dir as u8 {
                        (2 * core::mem::size_of::<RamFsDirEntry>()) as u32
                    } else {
                        0
                    },
                    atime: Self::get_time(),
                    mtime: Self::get_time(),
                    ctime: Self::get_time(),
                    direct_blocks: [block, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                    indirect_block: 0,
                    double_indirect_block: 0,
                    link_count: 1,
                    used: true,
                };
                self.node_set_used(i as u32);
                return Some(i as u32);
            }
        }
        None
    }

    pub fn read(&mut self, node_id: u32, offset: &mut u64, buf: &mut [u8], pwm: u64) -> i32 {
        let node = &self.nodes[node_id as usize];

        if !self.check_permission(node, pwm, FS_CAP_READ) {
            return KernelError::PermissionDenied.as_i32();
        }

        let mut bytes_read = 0usize;
        let node_size = u64::from(node.size);

        while bytes_read < buf.len() && *offset < node_size {
            let block_idx = (*offset as usize) / RAMFS_BLOCK_SIZE;
            let block_offset = (*offset as usize) % RAMFS_BLOCK_SIZE;
            let mut bytes_to_read = RAMFS_BLOCK_SIZE - block_offset;

            if bytes_to_read > buf.len() - bytes_read {
                bytes_to_read = buf.len() - bytes_read;
            }
            if bytes_to_read > (node_size - *offset) as usize {
                bytes_to_read = (node_size - *offset) as usize;
            }

            let block_num = Self::get_or_alloc_block(
                &mut self.nodes[node_id as usize],
                &mut self.data_area,
                &mut self.block_bitmap,
                &self.free_blocks,
                block_idx,
            );

            if let Some(block_num) = block_num {
                let start = (block_num as usize) * RAMFS_BLOCK_SIZE + block_offset;
                if start + bytes_to_read <= self.data_area.capacity_bytes() {
                    self.data_area
                        .read(start, &mut buf[bytes_read..bytes_read + bytes_to_read]);
                }
            }

            bytes_read += bytes_to_read;
            *offset += bytes_to_read as u64;
        }

        self.nodes[node_id as usize].atime = Self::get_time();

        bytes_read as i32
    }

    pub fn write(&mut self, node_id: u32, offset: &mut u64, buf: &[u8], pwm: u64) -> i32 {
        if !self.check_permission(&self.nodes[node_id as usize], pwm, FS_CAP_CREATE) {
            return KernelError::PermissionDenied.as_i32();
        }

        let mut bytes_written = 0usize;

        while bytes_written < buf.len() {
            let block_idx = (*offset as usize) / RAMFS_BLOCK_SIZE;
            let block_offset = (*offset as usize) % RAMFS_BLOCK_SIZE;
            let mut bytes_to_write = RAMFS_BLOCK_SIZE - block_offset;

            if bytes_to_write > buf.len() - bytes_written {
                bytes_to_write = buf.len() - bytes_written;
            }

            let block_num = Self::get_or_alloc_block(
                &mut self.nodes[node_id as usize],
                &mut self.data_area,
                &mut self.block_bitmap,
                &self.free_blocks,
                block_idx,
            );

            match block_num {
                Some(block_num) => {
                    let start = (block_num as usize) * RAMFS_BLOCK_SIZE + block_offset;
                    if start + bytes_to_write <= self.data_area.capacity_bytes()
                        && !self
                            .data_area
                            .write(start, &buf[bytes_written..bytes_written + bytes_to_write])
                    {
                        break;
                    }
                }
                None => break,
            }

            bytes_written += bytes_to_write;
            *offset += bytes_to_write as u64;

            if *offset > u64::from(self.nodes[node_id as usize].size) {
                self.nodes[node_id as usize].size = *offset as u32;
            }
        }

        self.nodes[node_id as usize].mtime = Self::get_time();

        dcache::icache_invalidate(self.fs_id, node_id);

        bytes_written as i32
    }

    // ========================================================================
    // Inode 适配: offset-by-value 方法 (供 Inode adapter 调用)
    // ========================================================================
    //
    // 与 read/write 的区别: offset 按值传入而非 &mut, 由调用者 (OpenFile) 管理偏移.
    // 适用于 Plan B Inode trait 的 read/write 实现.

    /// 读取文件数据 (offset-by-value 版本, 供 Inode adapter 使用)
    ///
    /// 返回 (实际读取字节数, 新偏移).
    pub fn read_at_offset(
        &mut self,
        node_id: u32,
        offset: u64,
        buf: &mut [u8],
        pwm: u64,
    ) -> (usize, u64) {
        let node = &self.nodes[node_id as usize];

        if !self.check_permission(node, pwm, FS_CAP_READ) {
            return (0, offset);
        }

        let mut bytes_read = 0usize;
        let node_size = u64::from(node.size);
        let mut current_offset = offset;

        while bytes_read < buf.len() && current_offset < node_size {
            let block_idx = (current_offset as usize) / RAMFS_BLOCK_SIZE;
            let block_offset = (current_offset as usize) % RAMFS_BLOCK_SIZE;
            let mut bytes_to_read = RAMFS_BLOCK_SIZE - block_offset;

            if bytes_to_read > buf.len() - bytes_read {
                bytes_to_read = buf.len() - bytes_read;
            }
            if bytes_to_read > (node_size - current_offset) as usize {
                bytes_to_read = (node_size - current_offset) as usize;
            }

            let block_num = Self::get_or_alloc_block(
                &mut self.nodes[node_id as usize],
                &mut self.data_area,
                &mut self.block_bitmap,
                &self.free_blocks,
                block_idx,
            );

            if let Some(block_num) = block_num {
                let start = (block_num as usize) * RAMFS_BLOCK_SIZE + block_offset;
                if start + bytes_to_read <= self.data_area.capacity_bytes() {
                    self.data_area
                        .read(start, &mut buf[bytes_read..bytes_read + bytes_to_read]);
                }
            }

            bytes_read += bytes_to_read;
            current_offset += bytes_to_read as u64;
        }

        self.nodes[node_id as usize].atime = Self::get_time();

        (bytes_read, current_offset)
    }

    /// 写入文件数据 (offset-by-value 版本, 供 Inode adapter 使用)
    ///
    /// 返回 (实际写入字节数, 新偏移).
    pub fn write_at_offset(
        &mut self,
        node_id: u32,
        offset: u64,
        buf: &[u8],
        pwm: u64,
    ) -> (usize, u64) {
        if !self.check_permission(&self.nodes[node_id as usize], pwm, FS_CAP_CREATE) {
            return (0, offset);
        }

        let mut bytes_written = 0usize;
        let mut current_offset = offset;

        while bytes_written < buf.len() {
            let block_idx = (current_offset as usize) / RAMFS_BLOCK_SIZE;
            let block_offset = (current_offset as usize) % RAMFS_BLOCK_SIZE;
            let mut bytes_to_write = RAMFS_BLOCK_SIZE - block_offset;

            if bytes_to_write > buf.len() - bytes_written {
                bytes_to_write = buf.len() - bytes_written;
            }

            let block_num = Self::get_or_alloc_block(
                &mut self.nodes[node_id as usize],
                &mut self.data_area,
                &mut self.block_bitmap,
                &self.free_blocks,
                block_idx,
            );

            match block_num {
                Some(block_num) => {
                    let start = (block_num as usize) * RAMFS_BLOCK_SIZE + block_offset;
                    if start + bytes_to_write <= self.data_area.capacity_bytes()
                        && !self
                            .data_area
                            .write(start, &buf[bytes_written..bytes_written + bytes_to_write])
                    {
                        break;
                    }
                }
                None => break,
            }

            bytes_written += bytes_to_write;
            current_offset += bytes_to_write as u64;

            if current_offset > u64::from(self.nodes[node_id as usize].size) {
                self.nodes[node_id as usize].size = current_offset as u32;
            }
        }

        self.nodes[node_id as usize].mtime = Self::get_time();

        dcache::icache_invalidate(self.fs_id, node_id);

        (bytes_written, current_offset)
    }

    /// 获取节点 stat 信息 (供 Inode stat 使用)
    ///
    /// # Errors
    /// 当 `node_id` 超出节点表范围时返回 `InvalidArgument`;
    /// 当节点未使用 (不存在) 时返回 `FileNotFound`.
    pub fn get_stat(&self, node_id: u32, _pwm: u64) -> crate::functions::fs::KernelResult<VfsStat> {
        use crate::functions::fs::KernelError as KE;
        if node_id as usize >= RAMFS_MAX_NODES {
            return Err(KE::InvalidArgument);
        }
        let node = &self.nodes[node_id as usize];
        if !node.used {
            return Err(KE::FileNotFound);
        }
        Ok(VfsStat {
            node_id: node.node_id,
            mode: node.perm,
            uid: 0,
            gid: 0,
            size: node.size,
            atime: node.atime,
            mtime: node.mtime,
            ctime: node.ctime,
            owner_pwm: node.owner_pwm,
            group_pwm: node.group_pwm,
            perm: node.perm,
            file_type: node.file_type,
            sensitivity: node.sensitivity,
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "函数体超 100 行 (复杂度阈值); 拆分需追改调用链且增加间接层, 当前任务优先 expect 兑底"
    )]
    pub fn truncate(&mut self, node_id: u32, new_size: u64, pwm: u64) -> i32 {
        if node_id as usize >= RAMFS_MAX_NODES {
            return KernelError::InvalidArgument.as_i32();
        }

        {
            let node = &self.nodes[node_id as usize];
            if !node.used {
                return KernelError::FileNotFound.as_i32();
            }
            if !self.check_permission(node, pwm, FS_CAP_WRITE) {
                return KernelError::PermissionDenied.as_i32();
            }
        }
        let old_size = {
            let node = &self.nodes[node_id as usize];
            u64::from(node.size)
        };

        if new_size == old_size {
            return 0;
        }

        if new_size < old_size && new_size > 0 {
            let last_block_idx = ((new_size - 1) as usize) / RAMFS_BLOCK_SIZE;
            let offset_in_block = ((new_size - 1) as usize) % RAMFS_BLOCK_SIZE;

            {
                let block_num = Self::get_or_alloc_block(
                    &mut self.nodes[node_id as usize],
                    &mut self.data_area,
                    &mut self.block_bitmap,
                    &self.free_blocks,
                    last_block_idx,
                );

                if let Some(block_num) = block_num {
                    if block_num != 0 {
                        let start = block_num as usize * RAMFS_BLOCK_SIZE + offset_in_block + 1;
                        let end = ((block_num as usize + 1) * RAMFS_BLOCK_SIZE)
                            .min(self.data_area.capacity_bytes());
                        if start < end {
                            self.data_area.zero(start, end - start);
                        }
                    }
                }
            }

            let first_block_to_free =
                (new_size + RAMFS_BLOCK_SIZE as u64 - 1) as usize / RAMFS_BLOCK_SIZE + 1;
            let last_block = (old_size + RAMFS_BLOCK_SIZE as u64 - 1) as usize / RAMFS_BLOCK_SIZE;

            {
                let mut blocks_to_free: Vec<u32> = Vec::new();
                let node_ref = &self.nodes[node_id as usize];

                for idx in first_block_to_free..last_block.min(DIRECT_BLOCKS) {
                    if node_ref.direct_blocks[idx] != 0 {
                        blocks_to_free.push(node_ref.direct_blocks[idx]);
                    }
                }

                for block_num in blocks_to_free {
                    self.block_set_free(block_num);
                }

                let node_mut = &mut self.nodes[node_id as usize];
                for idx in first_block_to_free..last_block.min(DIRECT_BLOCKS) {
                    if node_mut.direct_blocks[idx] != 0 {
                        node_mut.direct_blocks[idx] = 0;
                    }
                }

                let indirect_block = self.nodes[node_id as usize].indirect_block;
                if first_block_to_free < DIRECT_BLOCKS + INDIRECT_BLOCKS_PER_BLOCK
                    && indirect_block != 0
                {
                    let indirect_start = first_block_to_free.saturating_sub(DIRECT_BLOCKS);
                    let indirect_end = last_block
                        .saturating_sub(DIRECT_BLOCKS)
                        .min(INDIRECT_BLOCKS_PER_BLOCK);
                    self.free_indirect_chain(indirect_block, indirect_start, indirect_end);

                    if indirect_start == 0 {
                        self.nodes[node_id as usize].indirect_block = 0;
                    }
                }

                let double_indirect_block = self.nodes[node_id as usize].double_indirect_block;
                if first_block_to_free >= DIRECT_BLOCKS + INDIRECT_BLOCKS_PER_BLOCK
                    && double_indirect_block != 0
                {
                    let double_indirect_start = first_block_to_free
                        .saturating_sub(DIRECT_BLOCKS + INDIRECT_BLOCKS_PER_BLOCK);
                    let double_indirect_end =
                        last_block.saturating_sub(DIRECT_BLOCKS + INDIRECT_BLOCKS_PER_BLOCK);
                    self.free_double_indirect_chain(
                        double_indirect_block,
                        double_indirect_start,
                        double_indirect_end,
                    );

                    if double_indirect_start == 0 {
                        self.nodes[node_id as usize].double_indirect_block = 0;
                    }
                }
            }
        } else if new_size == 0 {
            let mut blocks_to_free: Vec<u32> = Vec::new();
            let indirect_blk = self.nodes[node_id as usize].indirect_block;
            let double_indirect_blk = self.nodes[node_id as usize].double_indirect_block;

            {
                let node = &self.nodes[node_id as usize];
                for i in 0..DIRECT_BLOCKS {
                    if node.direct_blocks[i] != 0 {
                        blocks_to_free.push(node.direct_blocks[i]);
                    }
                }
            }

            for block_num in blocks_to_free {
                self.block_set_free(block_num);
            }

            let node = &mut self.nodes[node_id as usize];
            for i in 0..DIRECT_BLOCKS {
                if node.direct_blocks[i] != 0 {
                    node.direct_blocks[i] = 0;
                }
            }

            if indirect_blk != 0 {
                self.free_indirect_chain(indirect_blk, 0, INDIRECT_BLOCKS_PER_BLOCK);
                self.nodes[node_id as usize].indirect_block = 0;
            }

            if double_indirect_blk != 0 {
                self.free_double_indirect_chain(
                    double_indirect_blk,
                    0,
                    INDIRECT_BLOCKS_PER_BLOCK * INDIRECT_BLOCKS_PER_BLOCK,
                );
                self.nodes[node_id as usize].double_indirect_block = 0;
            }
        }

        let node = &mut self.nodes[node_id as usize];
        node.size = new_size as u32;
        node.mtime = Self::get_time();

        dcache::icache_invalidate(self.fs_id, node_id);

        0
    }

    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    pub fn unlink(&mut self, path: &str, pwm: u64) -> i32 {
        let node_id = match self.resolve_path(path) {
            Some(n) => n,
            None => return KernelError::FileNotFound.as_i32(),
        };

        {
            let node = &self.nodes[node_id as usize];
            if !node.used {
                return KernelError::FileNotFound.as_i32();
            }
            if !self.check_permission(node, pwm, FS_CAP_WRITE) {
                return KernelError::PermissionDenied.as_i32();
            }
        }

        let (parent_path, _name) = path.rfind('/').map_or(("/", path), |pos| {
            if pos == 0 {
                ("/", &path[1..])
            } else {
                (&path[..pos], &path[pos + 1..])
            }
        });

        let parent_num = match self.resolve_path(parent_path) {
            Some(n) => n,
            None => return KernelError::FileNotFound.as_i32(),
        };

        let parent_block = self.nodes[parent_num as usize].direct_blocks[0];
        if parent_block != u32::MAX {
            let dirent_size = core::mem::size_of::<RamFsDirEntry>();
            let num_entries = self.nodes[parent_num as usize].size as usize / dirent_size;

            for i in 0..num_entries {
                let offset = (parent_block as usize) * RAMFS_BLOCK_SIZE + i * dirent_size;
                let mut entry = Self::read_dirent(&self.data_area, offset);
                if entry.node == node_id {
                    entry.node = 0;
                    Self::write_dirent(&mut self.data_area, offset, &entry);
                    break;
                }
            }
        }

        self.truncate(node_id, 0, pwm);
        {
            let node = &mut self.nodes[node_id as usize];
            node.used = false;
            node.file_type = 0;
            node.link_count = 0;
            node.owner_pwm = 0;
        }

        dcache::dcache_invalidate_parent(self.fs_id, parent_num);
        dcache::icache_invalidate(self.fs_id, node_id);

        0
    }

    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    pub fn create_file(&mut self, parent_path: &str, name: &str, pwm: u64) -> Option<u32> {
        if name.is_empty() || name.contains('/') {
            return None;
        }

        let parent_num = match self.resolve_path(parent_path) {
            Some(n) => n,
            None => return None,
        };

        if parent_num as usize >= RAMFS_MAX_NODES || !self.nodes[parent_num as usize].used {
            return None;
        }

        if self.nodes[parent_num as usize].file_type != VfsFileType::Dir as u8 {
            return None;
        }

        if !self.check_permission(&self.nodes[parent_num as usize], pwm, FS_CAP_CREATE) {
            return None;
        }

        let parent_block = self.nodes[parent_num as usize].direct_blocks[0];
        if parent_block == u32::MAX {
            return None;
        }

        let dirent_size = core::mem::size_of::<RamFsDirEntry>();
        let num_entries = self.nodes[parent_num as usize].size as usize / dirent_size;

        for i in 0..num_entries {
            let offset = (parent_block as usize) * RAMFS_BLOCK_SIZE + i * dirent_size;
            let entry = Self::read_dirent(&self.data_area, offset);
            if entry.node != 0 {
                let end = entry
                    .name
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(VFS_MAX_NAME);
                if core::str::from_utf8(&entry.name[..end]).unwrap_or("") == name {
                    return None;
                }
            }
        }

        let new_node_id = self.alloc_node(VfsFileType::File as u8, pwm)?;

        let parent_block = self.nodes[parent_num as usize].direct_blocks[0];
        let num_entries = self.nodes[parent_num as usize].size as usize / dirent_size;
        let offset = (parent_block as usize) * RAMFS_BLOCK_SIZE + num_entries * dirent_size;

        if offset + dirent_size > self.data_area.capacity_bytes() {
            return None;
        }

        let mut entry = RamFsDirEntry::new();
        entry.node = new_node_id;
        entry.file_type = VfsFileType::File as u8;
        entry.set_name(name);
        Self::write_dirent(&mut self.data_area, offset, &entry);

        self.nodes[parent_num as usize].size += dirent_size as u32;
        self.nodes[parent_num as usize].link_count += 1;
        self.nodes[parent_num as usize].mtime = Self::get_time();

        dcache::dcache_invalidate_parent(self.fs_id, parent_num);

        Some(new_node_id)
    }

    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    pub fn mkdir(&mut self, parent_path: &str, name: &str, pwm: u64) -> i32 {
        if name.is_empty() || name.contains('/') {
            return KernelError::InvalidArgument.as_i32();
        }

        let parent_num = match self.resolve_path(parent_path) {
            Some(n) => n,
            None => return KernelError::FileNotFound.as_i32(),
        };

        if parent_num as usize >= RAMFS_MAX_NODES {
            return KernelError::InvalidArgument.as_i32();
        }

        if !self.nodes[parent_num as usize].used {
            return KernelError::FileNotFound.as_i32();
        }

        if self.nodes[parent_num as usize].file_type != VfsFileType::Dir as u8 {
            return KernelError::NotADirectory.as_i32();
        }

        if !self.check_permission(&self.nodes[parent_num as usize], pwm, FS_CAP_CREATE) {
            return KernelError::PermissionDenied.as_i32();
        }

        let parent_block = self.nodes[parent_num as usize].direct_blocks[0];
        if parent_block == u32::MAX {
            return KernelError::FileNotFound.as_i32();
        }

        let dirent_size = core::mem::size_of::<RamFsDirEntry>();
        let num_entries = self.nodes[parent_num as usize].size as usize / dirent_size;

        for i in 0..num_entries {
            let offset = (parent_block as usize) * RAMFS_BLOCK_SIZE + i * dirent_size;
            let entry = Self::read_dirent(&self.data_area, offset);

            if entry.node != 0 {
                let end = entry
                    .name
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(VFS_MAX_NAME);
                let existing_name = core::str::from_utf8(&entry.name[..end]).unwrap_or("");
                if existing_name == name {
                    return KernelError::AlreadyExists.as_i32();
                }
            }
        }

        let new_node_id = match self.alloc_node(VfsFileType::Dir as u8, pwm) {
            Some(n) => n,
            None => return KernelError::FileNotFound.as_i32(),
        };

        let block = self.nodes[new_node_id as usize].direct_blocks[0];
        if block == u32::MAX {
            return KernelError::FileNotFound.as_i32();
        }

        let block_base = (block as usize) * RAMFS_BLOCK_SIZE;
        let mut dot = RamFsDirEntry::new();
        dot.node = new_node_id;
        dot.file_type = VfsFileType::Dir as u8;
        dot.set_name(".");
        Self::write_dirent(&mut self.data_area, block_base, &dot);

        let mut dotdot = RamFsDirEntry::new();
        dotdot.node = parent_num;
        dotdot.file_type = VfsFileType::Dir as u8;
        dotdot.set_name("..");
        Self::write_dirent(&mut self.data_area, block_base + dirent_size, &dotdot);

        self.nodes[new_node_id as usize].link_count = 2;

        let parent_block = self.nodes[parent_num as usize].direct_blocks[0];
        if parent_block == u32::MAX {
            return KernelError::FileNotFound.as_i32();
        }

        let num_entries = self.nodes[parent_num as usize].size as usize / dirent_size;
        let offset = (parent_block as usize) * RAMFS_BLOCK_SIZE + num_entries * dirent_size;

        if offset + dirent_size > self.data_area.capacity_bytes() {
            return KernelError::NoSpace.as_i32();
        }

        let mut entry = RamFsDirEntry::new();
        entry.node = new_node_id;
        entry.file_type = VfsFileType::Dir as u8;
        entry.set_name(name);
        Self::write_dirent(&mut self.data_area, offset, &entry);

        self.nodes[parent_num as usize].size += dirent_size as u32;
        self.nodes[parent_num as usize].link_count += 1;
        self.nodes[parent_num as usize].mtime = Self::get_time();

        dcache::dcache_invalidate_parent(self.fs_id, parent_num);

        0
    }

    pub fn stat(&self, node_id: u32) -> Option<VfsStat> {
        let node = &self.nodes[node_id as usize];

        if !node.used {
            return None;
        }

        Some(VfsStat {
            node_id: node.node_id,
            mode: node.perm,
            uid: 0xFFFF_FFFF,
            gid: 0xFFFF_FFFF,
            size: node.size,
            atime: node.atime,
            mtime: node.mtime,
            ctime: node.ctime,
            owner_pwm: node.owner_pwm,
            group_pwm: node.group_pwm,
            perm: node.perm,
            file_type: node.file_type,
            sensitivity: node.sensitivity,
        })
    }

    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    pub fn chmod(&mut self, path: &str, mode: u16, pwm: u64) -> i32 {
        let node_id = match self.resolve_path(path) {
            Some(n) => n,
            None => return KernelError::FileNotFound.as_i32(),
        };

        let node = &mut self.nodes[node_id as usize];
        if !node.used {
            return KernelError::FileNotFound.as_i32();
        }

        if node.owner_pwm != pwm {
            let level = pwm_api::pwm_get_privilege_level(pwm);
            if level != 0 {
                return KernelError::PermissionDenied.as_i32();
            }
        }

        node.perm = mode;
        node.ctime = Self::get_time();
        0
    }

    pub fn chown(&mut self, path: &str, owner_pwm: u64, pwm: u64) -> i32 {
        self.chown_ext(path, owner_pwm, 0, pwm)
    }

    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    pub fn chown_ext(&mut self, path: &str, owner_pwm: u64, group_pwm: u64, pwm: u64) -> i32 {
        let node_id = match self.resolve_path(path) {
            Some(n) => n,
            None => return KernelError::FileNotFound.as_i32(),
        };

        let node = &mut self.nodes[node_id as usize];
        if !node.used {
            return KernelError::FileNotFound.as_i32();
        }

        let level = pwm_api::pwm_get_privilege_level(pwm);
        if level != 0 {
            return KernelError::PermissionDenied.as_i32();
        }

        node.owner_pwm = owner_pwm;
        if group_pwm != 0 {
            node.group_pwm = group_pwm;
        } else if owner_pwm != 0 {
            node.group_pwm = owner_pwm;
        }
        node.ctime = Self::get_time();
        0
    }

    /// 设置节点时间戳 (atime/mtime), `u64::MAX` 表示该字段不修改 (utimensat 的
    /// `UTIME_OMIT` 语义)。
    ///
    /// 与 `chmod`/`chown` 同判据: 属主或特权级 0 方可写回, 判据与写回同锁域
    /// (调用方持锁, 不引入二次路径解析, 无 TOCTOU 窗口)。任一字段实际变更时
    /// 更新 `ctime` 为元数据变更时刻。
    ///
    /// # Errors
    ///
    /// - 节点号越界或节点未使用: `KernelError::FileNotFound`;
    /// - 非属主且特权级非 0: `KernelError::PermissionDenied`。
    pub fn set_times(&mut self, node_id: u32, atime: u64, mtime: u64, pwm: u64) -> i32 {
        if node_id as usize >= RAMFS_MAX_NODES {
            return KernelError::FileNotFound.as_i32();
        }
        let node = &mut self.nodes[node_id as usize];
        if !node.used {
            return KernelError::FileNotFound.as_i32();
        }
        if node.owner_pwm != pwm {
            let level = pwm_api::pwm_get_privilege_level(pwm);
            if level != 0 {
                return KernelError::PermissionDenied.as_i32();
            }
        }
        let changed = atime != u64::MAX || mtime != u64::MAX;
        if atime != u64::MAX {
            node.atime = atime;
        }
        if mtime != u64::MAX {
            node.mtime = mtime;
        }
        if changed {
            node.ctime = Self::get_time();
        }
        0
    }

    pub fn seek(
        &self,
        node_id: u32,
        current_offset: u64,
        offset: i64,
        whence: VfsSeekWhence,
    ) -> Option<u64> {
        if node_id as usize >= RAMFS_MAX_NODES {
            return None;
        }

        let node = &self.nodes[node_id as usize];
        if !node.used {
            return None;
        }

        let file_size = i64::from(node.size);

        let new_offset = match whence {
            VfsSeekWhence::Set => offset,
            VfsSeekWhence::Cur => {
                let current = current_offset as i64;
                current + offset
            }
            VfsSeekWhence::End => file_size + offset,
        };

        if new_offset < 0 {
            return None;
        }

        Some(new_offset as u64)
    }

    pub fn get_file_size(&self, node_id: u32) -> Option<u32> {
        if node_id as usize >= RAMFS_MAX_NODES {
            return None;
        }

        let node = &self.nodes[node_id as usize];
        if !node.used {
            return None;
        }

        Some(node.size)
    }

    pub fn link(&mut self, parent_node: u32, target_node: u32, name: &str, _pwm: u64) -> i32 {
        if name.is_empty() || name.contains('/') {
            return KernelError::InvalidArgument.as_i32();
        }
        if parent_node as usize >= RAMFS_MAX_NODES || target_node as usize >= RAMFS_MAX_NODES {
            return KernelError::InvalidArgument.as_i32();
        }
        if !self.nodes[parent_node as usize].used || !self.nodes[target_node as usize].used {
            return KernelError::FileNotFound.as_i32();
        }
        if self.nodes[parent_node as usize].file_type != VfsFileType::Dir as u8 {
            return KernelError::NotADirectory.as_i32();
        }

        let parent = &self.nodes[parent_node as usize];
        let parent_block = parent.direct_blocks[0];
        if parent_block == u32::MAX {
            return KernelError::FileNotFound.as_i32();
        }

        let dirent_size = core::mem::size_of::<RamFsDirEntry>();
        let num_entries = parent.size as usize / dirent_size;

        for i in 0..num_entries {
            let offset = (parent_block as usize) * RAMFS_BLOCK_SIZE + i * dirent_size;
            let entry = Self::read_dirent(&self.data_area, offset);
            if entry.node != 0 {
                let end = entry
                    .name
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(VFS_MAX_NAME);
                let existing = core::str::from_utf8(&entry.name[..end]).unwrap_or("");
                if existing == name {
                    return KernelError::AlreadyExists.as_i32();
                }
            }
        }

        let offset = (parent_block as usize) * RAMFS_BLOCK_SIZE + num_entries * dirent_size;
        if offset + dirent_size > self.data_area.capacity_bytes() {
            return KernelError::NoSpace.as_i32();
        }

        let mut entry = RamFsDirEntry::new();
        entry.node = target_node;
        entry.file_type = self.nodes[target_node as usize].file_type;
        entry.set_name(name);
        Self::write_dirent(&mut self.data_area, offset, &entry);

        self.nodes[parent_node as usize].size += dirent_size as u32;
        self.nodes[parent_node as usize].link_count += 1;
        self.nodes[parent_node as usize].mtime = Self::get_time();
        self.nodes[target_node as usize].link_count += 1;

        dcache::dcache_invalidate_parent(self.fs_id, parent_node);

        0
    }

    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    pub fn symlink(&mut self, target: &str, parent_path: &str, name: &str, pwm: u64) -> i32 {
        if name.is_empty() || name.contains('/') {
            return KernelError::InvalidArgument.as_i32();
        }
        if target.is_empty() || target.len() >= 128 {
            return KernelError::NameTooLong.as_i32();
        }
        let parent_num = match self.resolve_path(parent_path) {
            Some(n) => n,
            None => return KernelError::FileNotFound.as_i32(),
        };
        if parent_num as usize >= RAMFS_MAX_NODES || !self.nodes[parent_num as usize].used {
            return KernelError::FileNotFound.as_i32();
        }
        if self.nodes[parent_num as usize].file_type != VfsFileType::Dir as u8 {
            return KernelError::NotADirectory.as_i32();
        }
        if !self.check_permission(&self.nodes[parent_num as usize], pwm, FS_CAP_CREATE) {
            return KernelError::PermissionDenied.as_i32();
        }

        let parent_block = self.nodes[parent_num as usize].direct_blocks[0];
        if parent_block == u32::MAX {
            return KernelError::FileNotFound.as_i32();
        }
        let dirent_size = core::mem::size_of::<RamFsDirEntry>();
        let num_entries = self.nodes[parent_num as usize].size as usize / dirent_size;
        for i in 0..num_entries {
            let offset = (parent_block as usize) * RAMFS_BLOCK_SIZE + i * dirent_size;
            let entry = Self::read_dirent(&self.data_area, offset);
            if entry.node != 0 {
                let end = entry
                    .name
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(VFS_MAX_NAME);
                let existing = core::str::from_utf8(&entry.name[..end]).unwrap_or("");
                if existing == name {
                    return KernelError::AlreadyExists.as_i32();
                }
            }
        }

        let new_id = match self.alloc_node(VfsFileType::Symlink as u8, pwm) {
            Some(id) => id,
            None => return KernelError::FileNotFound.as_i32(),
        };
        let now = Self::get_time();
        let target_bytes = target.as_bytes();
        let target_len = target_bytes.len();
        {
            let node = &mut self.nodes[new_id as usize];
            node.node_id = new_id;
            node.file_type = VfsFileType::Symlink as u8;
            node.perm = 0o777;
            node.owner_pwm = pwm;
            node.link_count = 1;
            node.atime = now;
            node.mtime = now;
            node.ctime = now;
            node.used = true;
        }
        self.symlink_targets[new_id as usize][..target_len].copy_from_slice(target_bytes);
        self.symlink_lens[new_id as usize] = target_len as u8;

        let offset = (parent_block as usize) * RAMFS_BLOCK_SIZE + num_entries * dirent_size;
        if offset + dirent_size > self.data_area.capacity_bytes() {
            let n = &mut self.nodes[new_id as usize];
            n.used = false;
            n.file_type = 0;
            n.link_count = 0;
            self.symlink_lens[new_id as usize] = 0;
            return KernelError::NoSpace.as_i32();
        }
        let mut entry = RamFsDirEntry::new();
        entry.node = new_id;
        entry.file_type = VfsFileType::Symlink as u8;
        entry.set_name(name);
        Self::write_dirent(&mut self.data_area, offset, &entry);

        self.nodes[parent_num as usize].size += dirent_size as u32;
        self.nodes[parent_num as usize].link_count += 1;
        self.nodes[parent_num as usize].mtime = now;

        dcache::dcache_invalidate_parent(self.fs_id, parent_num);

        new_id as i32
    }

    pub fn readlink(&self, node_id: u32, buf: &mut [u8]) -> i32 {
        if node_id as usize >= RAMFS_MAX_NODES {
            return KernelError::InvalidArgument.as_i32();
        }
        let node = &self.nodes[node_id as usize];
        if !node.used || node.file_type != VfsFileType::Symlink as u8 {
            return KernelError::InvalidArgument.as_i32();
        }
        let len = self.symlink_lens[node_id as usize] as usize;
        if len > buf.len() {
            return KernelError::NoSpace.as_i32();
        }
        buf[..len].copy_from_slice(&self.symlink_targets[node_id as usize][..len]);
        len as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::privileged::sync::IrqSpinLock as Mutex;

    /// 测试专用静态实例 — 经 `RamFsData::new()` 常量初始化 (各表为空 `Vec`),
    /// 表内存由测试内 `ensure_allocated()` 惰性堆置。
    static TEST_FS: Mutex<RamFsData> = Mutex::new(RamFsData::new());

    /// `set_times`: 属主写回 / UTIME_OMIT / 非属主拒绝 / 未使用节点拒绝。
    #[test]
    fn test_set_times_owner_omit_and_permission() {
        let mut fs = TEST_FS.lock();
        // 惰性堆置各表 (避免空 Vec 索引越界)
        fs.ensure_allocated();
        // 直接构造最小已用节点 (绕过 mount/alloc 路径)
        {
            let node = &mut fs.nodes[1];
            node.node_id = 1;
            node.owner_pwm = 7;
            node.atime = 0;
            node.mtime = 0;
            node.used = true;
        }

        // 属主写回 + UTIME_OMIT: 仅 atime 更新, mtime 保持
        assert_eq!(fs.set_times(1, 111, u64::MAX, 7), 0);
        assert_eq!(fs.nodes[1].atime, 111);
        assert_eq!(fs.nodes[1].mtime, 0);

        // 非属主且特权级非 0 (未注册 pwm 恒 0xFF, fail-closed) → PermissionDenied
        assert_eq!(
            fs.set_times(1, 222, 222, 99),
            KernelError::PermissionDenied.as_i32()
        );
        // 拒绝后时间戳保持
        assert_eq!(fs.nodes[1].atime, 111);

        // 未使用节点 → FileNotFound; 越界节点 → FileNotFound
        assert_eq!(fs.set_times(2, 1, 1, 7), KernelError::FileNotFound.as_i32());
        assert_eq!(
            fs.set_times(RAMFS_MAX_NODES as u32, 1, 1, 7),
            KernelError::FileNotFound.as_i32()
        );
    }
}
