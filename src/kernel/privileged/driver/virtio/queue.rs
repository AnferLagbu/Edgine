//! `VirtIO` virtqueue 实现 (`VirtIO` 1.0 split ring, 兼容 0.9.5 legacy).
//!
//! 每个 virtqueue 由三段物理连续的内存区域组成:
//! - 描述符表 (Descriptor Table): 16 字节描述符数组
//! - 可用环 (Available Ring): 驱动→设备的通知环
//! - 已用环 (Used Ring): 设备→驱动的完成环
//!
//! 内存布局遵循 `VirtIO` 1.0 规范第 2.6 节 "Split Virtqueues" 与 0.9.5 legacy
//! 口径: **环尺寸 `num` 为运行时量** (legacy 传输无 QueueNum 协商寄存器,
//! 设备端按 realize 时固定的 num 自行推算 avail/used 地址, 驱动布局必须与
//! 设备推算全量一致, 截断即错位 — P-31 根因).

#![deny(missing_docs)]

use crate::privileged::mm::{KERNEL_BASE, PAGE_SIZE};

/// modern 模式下驱动请求的队列尺寸 (经 QueueNum 协商, 可小于设备上限);
/// 同时是 net 驱动 rx_buffers 槽位容量上限.
pub const VQ_SIZE: u16 = 32;

/// avail/used 环头部 (`flags: u16 + idx: u16`) 之后即环数组.
const RING_HEAD_BYTES: usize = 4;

/// Split virtqueue 描述符.
#[repr(C)]
pub struct VqDesc {
    /// 缓冲区客户机物理地址
    pub addr: u64,
    /// 缓冲区长度
    pub len: u32,
    /// `VRING_DESC_F_*` 标志位
    pub flags: u16,
    /// 下一描述符索引 (链式描述符)
    pub next: u16,
}

/// 已用环项.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VqUsedElem {
    /// 描述符链头索引
    pub id: u32,
    /// 设备写入的总字节数
    pub len: u32,
}

// ── 描述符标志 ──

/// 链接通过 `this.next` 继续
pub const VQ_DESC_F_NEXT: u16 = 1;
/// 设备可写入此缓冲区
pub const VQ_DESC_F_WRITE: u16 = 2;
/// 间接描述符表
pub const VQ_DESC_F_INDIRECT: u16 = 4;

// ── 可用环标志 ──

/// 驱动请求设备暂不发送中断
pub const VQ_AVAIL_F_NO_INTERRUPT: u16 = 1;

// ── 已用环标志 ──

/// 设备请求驱动暂不发送通知
pub const VQ_USED_F_NO_NOTIFY: u16 = 1;

/// 单个 virtqueue (运行时 `num` 驱动的 split ring 布局).
pub struct VirtQueue {
    /// 描述符表基址 (virt)
    pub desc: *mut VqDesc,
    /// avail 环基址 (头 4 字节 + `num` 项 u16 环 + used_event 2B).
    pub avail: *mut u8,
    /// used 环基址 (头 4 字节 + `num` 项 VqUsedElem + avail_event 4B).
    pub used: *mut u8,
    /// 环尺寸 (legacy: 设备 QueueNumMax 全量; modern: 驱动 QueueNum 协商值).
    pub num: u16,
    /// 空闲描述符链头
    pub free_head: u16,
    /// 最近见到的 used ring 索引
    pub last_used_idx: u16,
    /// 驱动下次使用的 avail ring 索引
    pub next_avail_idx: u16,
    // --- 所有权追踪 (用于 DMA 安全性) ---
    /// 已分配页的物理地址 (用于 phys↔virt 转换).
    pub desc_phys: u64,
    /// avail 环物理地址
    pub avail_phys: u64,
    /// used 环物理地址
    pub used_phys: u64,
}

/// split ring 三段布局计算 (纯函数, 供单测锁位).
///
/// 返回 `(avail_off, used_off, total_size)`:
/// - desc 恒在偏移 0, 长 `16*num`;
/// - avail 紧跟 desc: 长 `4 + 2*num + 2` (头 + 环 + used_event 预留);
/// - used: legacy 对齐 4096 (`VIRTIO_PCI_VRING_ALIGN`, QEMU legacy 设备端
///   同口径推算), modern 对齐 4; 尾部预留 4B `avail_event` (防设备越界写);
/// - total 向上取整到页.
///
/// 与 QEMU `vring_init_split` / Linux `virtio_ring` 的 legacy 分支一致:
/// num=1024 时 avail@0x4000 / used@0x5000 (P-31 坐实的驱动侧对齐基准).
pub const fn vring_layout(legacy: bool, num: u16) -> (usize, usize, usize) {
    let n = num as usize;
    let desc_bytes = 16 * n;
    let avail_off = desc_bytes;
    let avail_bytes = RING_HEAD_BYTES + 2 * n + 2;
    let used_align = if legacy {
        crate::privileged::lib::usize_from_u64(PAGE_SIZE)
    } else {
        4
    };
    let used_off = align_up(avail_off + avail_bytes, used_align);
    let used_bytes = RING_HEAD_BYTES + 8 * n + 4;
    let total = align_up(
        used_off + used_bytes,
        crate::privileged::lib::usize_from_u64(PAGE_SIZE),
    );
    (avail_off, used_off, total)
}

// SAFETY: VirtQueue 裸指针指向 PMM 分配的 DMA 页.
// 每个设备实例拥有自己的 virtqueue. 描述符操作使用 volatile 访问与内存屏障.
// SAFETY: VirtQueue 含 MMIO 裸指针, 跨 CPU 安全性由单一所有者 &mut self 访问或外部锁保证.
unsafe impl Send for VirtQueue {}
// SAFETY: 同上, &mut self 或外部锁保证并发安全.
unsafe impl Sync for VirtQueue {}

impl VirtQueue {
    /// 分配并初始化一个 split virtqueue (运行时尺寸 `num`).
    ///
    /// `legacy` 为 true 时 used ring 按 4096 对齐 (QEMU legacy 设备端同口径
    /// 推算 — 布局不一致则设备读到错误 avail/used 地址, 永不回写 used, 即
    /// P-31 TX 超时根因). `num` 必须由调用方按传输形态选定: legacy 取设备
    /// `QueueNumMax` 全量, modern 取驱动协商值.
    pub fn new(legacy: bool, num: u16) -> Option<Self> {
        // SAFETY: pmm_alloc_pages 由 PMM 模块提供, 符号与 C ABI 声明一致
        unsafe extern "C" {
            fn pmm_alloc_pages(count: u64) -> *mut u8;
        }
        if num == 0 || !num.is_power_of_two() {
            // split ring 规范要求 num 为 2 的幂 (取模即环回).
            return None;
        }
        let (_avail_off, used_off, total_size) = vring_layout(legacy, num);
        let desc_size = 16 * num as usize;
        let pages = total_size.div_ceil(crate::privileged::lib::usize_from_u64(PAGE_SIZE));
        // SAFETY: extern 函数的参数/返回值类型与 C ABI 声明一致; 调用方保证指针有效
        let mem = unsafe { pmm_alloc_pages(pages as u64) };
        if mem.is_null() {
            return None;
        }

        let mem_phys = mem as u64;
        let mem_virt = (mem_phys + KERNEL_BASE) as *mut u8;

        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            core::ptr::write_bytes(mem_virt, 0, total_size);

            let desc_ptr = mem_virt.cast::<VqDesc>();
            let avail_ptr = mem_virt.add(desc_size);
            let used_ptr = mem_virt.add(used_off);

            for i in 0..num {
                let desc = &mut *desc_ptr.add(i as usize);
                desc.flags = 0;
                desc.next = if i + 1 < num { i + 1 } else { 0xFFFF };
            }

            Some(Self {
                desc: desc_ptr,
                avail: avail_ptr,
                used: used_ptr,
                num,
                free_head: 0,
                last_used_idx: 0,
                next_avail_idx: 0,
                desc_phys: mem_phys,
                avail_phys: mem_phys + desc_size as u64,
                used_phys: mem_phys + used_off as u64,
            })
        }
    }

    /// 获取描述符表的物理地址.
    pub fn desc_paddr(&self) -> u64 {
        self.desc_phys
    }
    /// 获取 available ring 的物理地址.
    pub fn avail_paddr(&self) -> u64 {
        self.avail_phys
    }
    /// 获取 used ring 的物理地址.
    pub fn used_paddr(&self) -> u64 {
        self.used_phys
    }

    /// 准备一个描述符链并返回头索引.
    /// 若无空闲描述符返回 `0xFFFF`.
    /// 对于简单读写操作, 这将创建单描述符链.
    pub fn prepare_desc(&mut self, buf_paddr: u64, buf_len: u32, write: bool) -> u16 {
        if self.free_head == 0xFFFF {
            return 0xFFFF;
        }
        let head = self.free_head;
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            let desc = &mut *self.desc.add(head as usize);
            let next_free = desc.next; // 覆盖前先保存
            desc.addr = buf_paddr;
            desc.len = buf_len;
            desc.flags = if write { VQ_DESC_F_WRITE } else { 0 };
            desc.next = 0;
            // free_head 推进到下一个空闲描述符
            self.free_head = next_free;
        }
        head
    }

    /// 提交描述符链到可用环 (不碰 idx, 由 `commit_and_kick` 发布).
    /// 返回已提交的可用环索引.
    pub fn submit(&mut self, desc_head: u16) -> u16 {
        // SAFETY: 调用方保证指针有效; 环槽位 = 头 4B + (idx % num) * 2
        unsafe {
            let slot = self
                .avail
                .add(RING_HEAD_BYTES + (self.next_avail_idx as usize % self.num as usize) * 2)
                .cast::<u16>();
            core::ptr::write_volatile(slot, desc_head);
        }
        let idx = self.next_avail_idx;
        self.next_avail_idx = self.next_avail_idx.wrapping_add(1);
        idx
    }

    /// 提交后发布 avail.idx (调用方随后写 `QueueNotify`).
    pub fn commit_and_kick(&mut self) {
        // SAFETY: 调用方保证指针有效; avail.idx 位于环基址偏移 2
        unsafe {
            // 全内存屏障: 确保描述符与 ring 写入全局可见
            crate::privileged::sync::arch::fence();
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            core::ptr::write_volatile(self.avail.add(2).cast::<u16>(), self.next_avail_idx);
        }
    }

    /// 检查是否有已用描述符可用, 有则返回.
    pub fn pop_used(&mut self) -> Option<(u16, u32)> {
        // SAFETY: 调用方保证指针有效; used.idx 位于基址偏移 2,
        // 环项 = 头 4B + (idx % num) * size_of(VqUsedElem). 全部 volatile:
        // 设备 DMA 写入, 普通读可被 spin 循环 CSE 提升致永不可见.
        unsafe {
            let used_idx = core::ptr::read_volatile(self.used.add(2) as *const u16);
            if self.last_used_idx == used_idx {
                return None;
            }
            let elem_ptr = self.used.add(
                RING_HEAD_BYTES
                    + (self.last_used_idx as usize % self.num as usize)
                        * core::mem::size_of::<VqUsedElem>(),
            ) as *const VqUsedElem;
            let elem = core::ptr::read_volatile(elem_ptr);
            self.last_used_idx = self.last_used_idx.wrapping_add(1);
            Some((crate::privileged::lib::low_u16_from_u32(elem.id), elem.len))
        }
    }

    /// 完成后将描述符归还到空闲链.
    pub fn reclaim_desc(&mut self, head: u16) {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            let desc = &mut *self.desc.add(head as usize);
            desc.next = self.free_head;
            self.free_head = head;
        }
    }

    /// 将描述符 head 链接到 next 描述符 (设置 NEXT 标志和 next 指针).
    ///
    /// 用于构建多描述符链 (如 virtio-blk 的 header→data→status).
    pub fn link_desc(&mut self, head: u16, next: u16) {
        // SAFETY: head/next 均为有效的描述符索引 (由 prepare_desc 返回),
        // desc 指针指向 PMM 分配的 DMA 页, 生命周期由 VirtQueue 管理.
        unsafe {
            let desc = &mut *self.desc.add(head as usize);
            desc.flags |= VQ_DESC_F_NEXT;
            desc.next = next;
        }
    }
}

// ============================================================================
// DMA 缓冲区 (safe API, 供 functions 层调用)
// ============================================================================

/// DMA 缓冲区: 物理连续的内核页, 供 `VirtIO` 设备 DMA 访问.
///
/// 提供 safe 的字节级读写方法, 使 functions 层无需 unsafe 即可
/// 构造请求头和读写数据.
pub struct DmaBuffer {
    /// 缓冲区物理地址 (设备 DMA 使用)
    phys: u64,
    /// 缓冲区虚拟地址 (内核读写使用)
    virt: *mut u8,
    /// 分配的页数 (Drop 时用于释放, 当前 PMM 未提供释放接口)
    pages: usize,
    /// 缓冲区总大小 (字节)
    size: usize,
}

// SAFETY: DmaBuffer 拥有 PMM 分配的独占页; 单一所有者
unsafe impl Send for DmaBuffer {}

impl DmaBuffer {
    /// 分配指定大小的 DMA 缓冲区.
    ///
    /// 返回 `Some(DmaBuffer)` 表示分配成功, `None` 表示内存不足.
    /// 缓冲区内容初始化为零.
    pub fn new(size: usize) -> Option<Self> {
        // SAFETY: pmm_alloc_pages 由 PMM 模块提供, 符号与 C ABI 声明一致
        unsafe extern "C" {
            fn pmm_alloc_pages(count: u64) -> *mut u8;
        }
        let pages = (size + crate::privileged::lib::usize_from_u64(PAGE_SIZE) - 1)
            / crate::privileged::lib::usize_from_u64(PAGE_SIZE);
        // SAFETY: pmm_alloc_pages 由 PMM 模块提供, 参数与返回值类型匹配
        let ptr = unsafe { pmm_alloc_pages(pages as u64) };
        if ptr.is_null() {
            return None;
        }
        let phys = ptr as u64;
        let virt = (phys + KERNEL_BASE) as *mut u8;
        // SAFETY: virt 指向有效内核页, 长度 pages * PAGE_SIZE >= size
        unsafe {
            core::ptr::write_bytes(virt, 0, size);
        }
        Some(Self {
            phys,
            virt,
            pages,
            size,
        })
    }

    /// 物理地址 (用于 `VirtQueue` 描述符)
    #[inline]
    pub fn phys_addr(&self) -> u64 {
        self.phys
    }

    /// 缓冲区总大小
    #[inline]
    pub fn size(&self) -> usize {
        self.size
    }

    /// 从缓冲区 `offset` 处读取一个字节.
    ///
    /// # Panic
    /// `offset >= size` 时 panic.
    /// # Panics
    /// offset 超出缓冲区范围时 panic。
    pub fn read_byte(&self, offset: usize) -> u8 {
        assert!(
            offset < self.size,
            "DmaBuffer::read_byte: offset out of bounds"
        );
        // SAFETY: virt 指向有效内核页, offset < size 已检查
        unsafe { *self.virt.add(offset) }
    }

    /// 向缓冲区 `offset` 处写入一个字节.
    /// # Panics
    /// offset 超出缓冲区范围时 panic。
    pub fn write_byte(&mut self, offset: usize, val: u8) {
        assert!(
            offset < self.size,
            "DmaBuffer::write_byte: offset out of bounds"
        );
        // SAFETY: virt 指向有效内核页, offset < size 已检查, &mut self 保证独占
        unsafe {
            *self.virt.add(offset) = val;
        }
    }

    /// 从缓冲区 `offset` 处读取一个 u32 (小端).
    pub fn read_u32(&self, offset: usize) -> u32 {
        let b0 = u32::from(self.read_byte(offset));
        let b1 = u32::from(self.read_byte(offset + 1));
        let b2 = u32::from(self.read_byte(offset + 2));
        let b3 = u32::from(self.read_byte(offset + 3));
        b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
    }

    /// 向缓冲区 `offset` 处写入一个 u32 (小端).
    pub fn write_u32(&mut self, offset: usize, val: u32) {
        self.write_byte(offset, crate::privileged::lib::low_u8_from_u32(val));
        self.write_byte(
            offset + 1,
            crate::privileged::lib::low_u8_from_u32(val >> 8),
        );
        self.write_byte(
            offset + 2,
            crate::privileged::lib::low_u8_from_u32(val >> 16),
        );
        self.write_byte(offset + 3, (val >> 24) as u8);
    }

    /// 从缓冲区 `offset` 处读取一个 u64 (小端).
    pub fn read_u64(&self, offset: usize) -> u64 {
        let lo = u64::from(self.read_u32(offset));
        let hi = u64::from(self.read_u32(offset + 4));
        lo | (hi << 32)
    }

    /// 向缓冲区 `offset` 处写入一个 u64 (小端).
    pub fn write_u64(&mut self, offset: usize, val: u64) {
        self.write_u32(offset, crate::privileged::lib::low_u32_from_u64(val));
        self.write_u32(offset + 4, (val >> 32) as u32);
    }

    /// 从 `src` 拷贝 `len` 字节到缓冲区 `offset` 处.
    /// # Panics
    /// 目标范围超出缓冲区大小时 panic。
    pub fn write_slice(&mut self, offset: usize, src: &[u8]) {
        let len = src.len();
        assert!(
            offset + len <= self.size,
            "DmaBuffer::write_slice: out of bounds"
        );
        // SAFETY: virt 指向有效内核页, offset + len <= size 已检查, &mut self 保证独占
        unsafe {
            core::ptr::copy_nonoverlapping(src.as_ptr(), self.virt.add(offset), len);
        }
    }

    /// 从缓冲区 `offset` 处拷贝 `len` 字节到 `dst`.
    /// # Panics
    /// 目标范围超出缓冲区大小时 panic。
    pub fn read_slice(&self, offset: usize, dst: &mut [u8]) {
        let len = dst.len();
        assert!(
            offset + len <= self.size,
            "DmaBuffer::read_slice: out of bounds"
        );
        // SAFETY: virt 指向有效内核页, offset + len <= size 已检查
        unsafe {
            core::ptr::copy_nonoverlapping(self.virt.add(offset), dst.as_mut_ptr(), len);
        }
    }
}

impl Drop for DmaBuffer {
    #[expect(
        clippy::no_effect_underscore_binding,
        reason = "no_effect_underscore_binding: let _ = expr 用于类型推导/副作用; 当前优先 expect"
    )]
    fn drop(&mut self) {
        // 当前 PMM 不提供 pmm_free_pages, 缓冲区生命周期由系统管理.
        // 占位: PMM 增量后接入实际释放. self.pages 记录分配页数供将来使用.
        let _pages = self.pages;
    }
}

/// 将 `val` 向上对齐到 `align` 的下一个倍数.
const fn align_up(val: usize, align: usize) -> usize {
    (val + align - 1) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::vring_layout;

    /// P-31 根因锚点: legacy num=1024 时布局必须与 QEMU 设备端推算一致 —
    /// avail@desc+0x4000 / used@desc+0x5000 (页对齐). 不一致则设备读到的
    /// avail.idx 恒 0, 永不取描述符也永不回写 used → TX 超时.
    #[test]
    fn legacy_1024_matches_device_inference() {
        let (avail_off, used_off, total) = vring_layout(true, 1024);
        assert_eq!(avail_off, 16 * 1024);
        assert_eq!(avail_off, 0x4000);
        assert_eq!(used_off, 0x5000);
        assert_eq!(used_off % 4096, 0, "legacy used ring 必须页对齐");
        assert_eq!(total, 0x8000, "num=1024 全布局共 8 页");
    }

    /// modern num=32: used 按 4 对齐 (规范 Split Ring Used Align = 4), 整体一页内.
    #[test]
    fn modern_32_uses_dword_align() {
        let (avail_off, used_off, total) = vring_layout(false, 32);
        assert_eq!(avail_off, 512);
        assert_eq!(used_off, 584); // align(512 + 4 + 64 + 2, 4)
        assert_eq!(used_off % 4, 0);
        assert_eq!(total, 4096);
    }

    /// 通用下界: 任意 (legacy, num) 的 total 必须完整容纳 used 环项 +
    /// 尾部 4B avail_event 预留, 且取整到页 (防设备尾部写抓出分配区).
    #[test]
    fn total_covers_all_segments() {
        for &(legacy, num) in &[(true, 1024), (true, 256), (false, 32), (false, 256)] {
            let (_, used_off, total) = vring_layout(legacy, num);
            let used_end = used_off + 4 + 8 * num as usize + 4;
            assert!(
                total >= used_end,
                "legacy={legacy} num={num}: total={total} < used_end={used_end}"
            );
            assert_eq!(total % 4096, 0);
        }
    }
}
